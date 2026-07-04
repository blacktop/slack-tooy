use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{Context, Result, bail};
use reqwest::Client;

use crate::slack::types::{
    AuthInfo, Channel, CompleteUploadData, ConversationsHistoryData, ConversationsListData,
    EmojiListData, PostMessageData, SlackApiResponse, SlackMessage, StarsListData, UploadUrlData,
    User, UserInfoData,
};

const SLACK_API_BASE: &str = "https://slack.com/api";
const URLENCODE_HEX: &[u8; 16] = b"0123456789ABCDEF";
/// Full-size file transfers (uploads and downloads) get a longer
/// deadline than the 30s API timeout, which large attachments could
/// not finish within.
const FILE_TRANSFER_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Clone)]
pub struct SlackClient {
    client: Client,
    token: String,
    /// Browser session cookie for xoxc- tokens.
    cookie: Option<String>,
}

impl SlackClient {
    pub fn new(token: &str, cookie: &str) -> Result<Self> {
        let cookie = if cookie.is_empty() {
            None
        } else {
            Some(cookie.to_string())
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .wrap_err("Failed to build HTTP client (TLS backend unavailable?)")?;
        Ok(Self {
            client,
            token: token.to_string(),
            cookie,
        })
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let builder = builder.bearer_auth(&self.token);
        if let Some(ref cookie) = self.cookie {
            // Accept both raw values ("xoxd-...") and already-prefixed
            // ("d=xoxd-...") so the user can paste either form.
            let value = if cookie.starts_with("d=") {
                cookie.clone()
            } else {
                format!("d={cookie}")
            };
            builder.header("Cookie", value)
        } else {
            builder
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{SLACK_API_BASE}/{method}");
        // Retry once on transport errors (stale pooled connections).
        let builder = self.client.get(&url).query(params);
        let resp = match self.apply_auth(builder).send().await {
            Ok(resp) => resp,
            Err(first_err) => {
                tracing::debug!("Retrying GET {method}: {first_err}");
                let builder = self.client.get(&url).query(params);
                self.apply_auth(builder).send().await?
            }
        };

        Self::decode_api_response(method, resp).await
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{SLACK_API_BASE}/{method}");
        let builder = self.client.post(&url).json(body);
        let resp = self.apply_auth(builder).send().await?;

        Self::decode_api_response(method, resp).await
    }

    async fn post_urlencoded<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{SLACK_API_BASE}/{method}");
        let body = urlencoded_body(params);
        let builder = self
            .client
            .post(&url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body);
        let resp = self.apply_auth(builder).send().await?;

        Self::decode_api_response(method, resp).await
    }

    async fn decode_api_response<T: serde::de::DeserializeOwned>(
        method: &str,
        resp: reqwest::Response,
    ) -> Result<T> {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .wrap_err_with(|| format!("Failed to read Slack API {method} response body"))?;
        if !status.is_success() {
            bail!("Slack API {method} HTTP error: {status}");
        }

        let value: serde_json::Value = serde_json::from_str(&body)
            .wrap_err_with(|| format!("Slack API {method} returned a non-JSON response"))?;
        if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            let error = Self::api_error_message(&value);
            bail!("Slack API {method} error: {error}");
        }

        let api_resp: SlackApiResponse<T> = serde_json::from_value(value)
            .wrap_err_with(|| format!("Failed to decode Slack API {method} success response"))?;
        Ok(api_resp.data)
    }

    fn api_error_message(value: &serde_json::Value) -> String {
        let error = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown_error");
        let Some(messages) = value
            .get("response_metadata")
            .and_then(|metadata| metadata.get("messages"))
            .and_then(serde_json::Value::as_array)
        else {
            return error.to_string();
        };

        let details = messages
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        if details.is_empty() {
            error.to_string()
        } else {
            format!("{error}: {details}")
        }
    }

    pub async fn auth_test(&self) -> Result<AuthInfo> {
        let body = serde_json::json!({});
        self.post("auth.test", &body).await
    }

    pub async fn list_emoji(&self) -> Result<std::collections::HashMap<String, String>> {
        let data: EmojiListData = self.get("emoji.list", &[]).await?;
        Ok(data.emoji)
    }

    pub async fn list_stars(&self) -> Result<HashSet<String>> {
        let mut starred = HashSet::new();
        let mut cursor = String::new();

        loop {
            let params = vec![("limit", "200"), ("cursor", &cursor)];
            let data: StarsListData = self.get("stars.list", &params).await?;
            starred.extend(
                data.items
                    .into_iter()
                    .filter(|item| {
                        matches!(item.item_type.as_str(), "channel" | "im" | "group" | "mpim")
                    })
                    .map(|item| item.channel)
                    .filter(|id| !id.is_empty()),
            );

            match data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty())
            {
                Some(next) => cursor = next,
                None => break,
            }
        }

        Ok(starred)
    }

    pub async fn list_channels(&self) -> Result<Vec<Channel>> {
        let mut all_channels = Vec::new();
        let mut cursor = String::new();

        loop {
            let params = vec![
                ("types", "public_channel,private_channel,im,mpim"),
                ("exclude_archived", "true"),
                ("limit", "200"),
                ("cursor", &cursor),
            ];

            let data: ConversationsListData = self.get("conversations.list", &params).await?;
            all_channels.extend(data.channels);

            match data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty())
            {
                Some(next) => cursor = next,
                None => break,
            }
        }

        let channels: Vec<Channel> = all_channels
            .into_iter()
            .filter(|c| c.is_member.unwrap_or(true))
            .collect();

        Ok(channels)
    }

    pub async fn fetch_history(&self, channel_id: &str, limit: u32) -> Result<Vec<SlackMessage>> {
        let limit_str = limit.to_string();
        let params = vec![("channel", channel_id), ("limit", &limit_str)];

        let data: ConversationsHistoryData = self.get("conversations.history", &params).await?;

        Ok(data.messages)
    }

    pub async fn fetch_replies(
        &self,
        channel_id: &str,
        thread_ts: &str,
    ) -> Result<Vec<SlackMessage>> {
        let mut all_messages = Vec::new();
        let mut cursor = String::new();

        loop {
            let params = vec![
                ("channel", channel_id),
                ("ts", thread_ts),
                ("limit", "200"),
                ("cursor", &cursor),
            ];
            let data: ConversationsHistoryData = self.get("conversations.replies", &params).await?;
            all_messages.extend(data.messages);

            match data
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty())
            {
                Some(next) => cursor = next,
                None => break,
            }
        }

        Ok(all_messages)
    }

    pub async fn post_message(
        &self,
        channel_id: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<Option<String>> {
        let mut body = serde_json::json!({
            "channel": channel_id,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let data: PostMessageData = self.post("chat.postMessage", &body).await?;
        Ok(data.ts)
    }

    pub async fn upload_file(
        &self,
        channel_id: &str,
        path: &Path,
        initial_comment: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| color_eyre::eyre::eyre!("Upload path has no valid filename"))?;
        let bytes = tokio::fs::read(path)
            .await
            .wrap_err_with(|| format!("Failed to read upload file {}", path.display()))?;
        let length = u64::try_from(bytes.len())?;
        if length == 0 {
            bail!("Cannot upload empty file {}", path.display());
        }

        let upload_ticket = self
            .request_upload_url(filename, length)
            .await
            .wrap_err("Failed to get Slack upload URL")?;
        self.upload_file_bytes(&upload_ticket.upload_url, bytes)
            .await
            .wrap_err("Failed to upload file bytes to Slack upload URL")?;
        self.complete_upload(
            channel_id,
            &upload_ticket.file_id,
            filename,
            initial_comment,
            thread_ts,
        )
        .await
        .wrap_err("Failed to complete Slack file upload")
    }

    async fn request_upload_url(&self, filename: &str, length: u64) -> Result<UploadUrlData> {
        let length = length.to_string();
        self.post_urlencoded(
            "files.getUploadURLExternal",
            &[("filename", filename), ("length", &length)],
        )
        .await
    }

    async fn upload_file_bytes(&self, upload_url: &str, bytes: Vec<u8>) -> Result<()> {
        let resp = self
            .client
            .post(upload_url)
            .timeout(FILE_TRANSFER_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Slack file upload HTTP error: {status}");
        }

        Ok(())
    }

    async fn complete_upload(
        &self,
        channel_id: &str,
        file_id: &str,
        title: &str,
        initial_comment: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "channel_id": channel_id,
            "files": [
                {
                    "id": file_id,
                    "title": title,
                }
            ],
        });
        if let Some(comment) = initial_comment.filter(|comment| !comment.is_empty()) {
            body["initial_comment"] = serde_json::Value::String(comment.to_string());
        }
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let data: CompleteUploadData = self.post("files.completeUploadExternal", &body).await?;
        if data.files.is_empty() {
            bail!("Slack file upload completed without a file response");
        }
        Ok(())
    }

    pub async fn get_user_info(&self, user_id: &str) -> Result<User> {
        let params = vec![("user", user_id)];
        let data: UserInfoData = self.get("users.info", &params).await?;

        data.user
            .ok_or_else(|| color_eyre::eyre::eyre!("No user in response"))
    }

    /// Attach the bearer token and session cookie only for Slack-hosted
    /// URLs.  File URLs come from the API response and can point at
    /// external providers (`files.remote.add`), which must never
    /// receive our credentials.
    fn apply_auth_for_host(
        &self,
        url: &str,
        builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        if is_slack_host(url) {
            self.apply_auth(builder)
        } else {
            builder
        }
    }

    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let builder = self.client.get(url);
        let resp = self.apply_auth_for_host(url, builder).send().await?;
        Self::download_response_bytes(url, resp).await
    }

    /// Download streamed straight to `out` so large files never
    /// buffer fully in memory.
    pub async fn download_to(&self, url: &str, out: &mut tokio::fs::File) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let builder = self.client.get(url).timeout(FILE_TRANSFER_TIMEOUT);
        let mut resp = self.apply_auth_for_host(url, builder).send().await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("HTTP error downloading {url}: {status}");
        }
        while let Some(chunk) = resp.chunk().await? {
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        Ok(())
    }

    async fn download_response_bytes(url: &str, resp: reqwest::Response) -> Result<Vec<u8>> {
        let status = resp.status();
        if !status.is_success() {
            bail!("HTTP error downloading {url}: {status}");
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }
}

/// True when `url` points at slack.com or a subdomain (files.slack.com,
/// workspace-name.slack.com).  Anything else is treated as external.
fn is_slack_host(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    parsed
        .host_str()
        .is_some_and(|host| host == "slack.com" || host.ends_with(".slack.com"))
}

fn urlencoded_body(params: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (idx, (key, value)) in params.iter().enumerate() {
        if idx > 0 {
            body.push('&');
        }
        push_urlencoded_component(&mut body, key);
        body.push('=');
        push_urlencoded_component(&mut body, value);
    }
    body
}

fn push_urlencoded_component(out: &mut String, value: &str) {
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            byte => {
                out.push('%');
                out.push(char::from(URLENCODE_HEX[usize::from(byte >> 4)]));
                out.push(char::from(URLENCODE_HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::slack::client::{is_slack_host, urlencoded_body};

    #[test]
    fn urlencoded_body_encodes_spaces_punctuation_and_utf8() {
        let body = urlencoded_body(&[("filename", "cat pic&é.png"), ("length", "12")]);

        assert_eq!(body, "filename=cat+pic%26%C3%A9.png&length=12");
    }

    #[test]
    fn is_slack_host_accepts_only_slack_domains_over_https() {
        assert!(is_slack_host("https://slack.com/api/x"));
        assert!(is_slack_host(
            "https://files.slack.com/files-pri/T1-F1/cat.png"
        ));
        assert!(is_slack_host("https://myteam.slack.com/x"));

        assert!(!is_slack_host("http://files.slack.com/downgraded"));
        assert!(!is_slack_host("https://evil.com/files.slack.com/x"));
        assert!(!is_slack_host("https://notslack.com/x"));
        assert!(!is_slack_host("https://files.slack.com.evil.com/x"));
        assert!(!is_slack_host("not a url"));
    }
}
