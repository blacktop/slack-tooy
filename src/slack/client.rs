use std::time::Duration;

use color_eyre::eyre::{Result, bail};
use reqwest::Client;

use crate::slack::types::{
    AuthInfo, Channel, ConversationsHistoryData, ConversationsListData, EmojiListData,
    PostMessageData, SlackApiResponse, SlackMessage, User, UserInfoData,
};

const SLACK_API_BASE: &str = "https://slack.com/api";

#[derive(Clone)]
pub struct SlackClient {
    client: Client,
    token: String,
    /// Browser session cookie for xoxc- tokens.
    cookie: Option<String>,
}

impl SlackClient {
    pub fn new(token: &str, cookie: &str) -> Self {
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
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            token: token.to_string(),
            cookie,
        }
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
        let builder = self.client.get(&url).query(params);
        let resp = self.apply_auth(builder).send().await?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Slack API HTTP error: {status}");
        }

        let api_resp: SlackApiResponse<T> = resp.json().await?;
        if !api_resp.ok {
            bail!("Slack API error: {}", api_resp.error.unwrap_or_default());
        }

        Ok(api_resp.data)
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{SLACK_API_BASE}/{method}");
        let builder = self.client.post(&url).json(body);
        let resp = self.apply_auth(builder).send().await?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Slack API HTTP error: {status}");
        }

        let api_resp: SlackApiResponse<T> = resp.json().await?;
        if !api_resp.ok {
            bail!("Slack API error: {}", api_resp.error.unwrap_or_default());
        }

        Ok(api_resp.data)
    }

    pub async fn auth_test(&self) -> Result<AuthInfo> {
        let body = serde_json::json!({});
        self.post("auth.test", &body).await
    }

    pub async fn list_emoji(&self) -> Result<std::collections::HashMap<String, String>> {
        let data: EmojiListData = self.get("emoji.list", &[]).await?;
        Ok(data.emoji)
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
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "channel": channel_id,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let _data: PostMessageData = self.post("chat.postMessage", &body).await?;
        Ok(())
    }

    pub async fn get_user_info(&self, user_id: &str) -> Result<User> {
        let params = vec![("user", user_id)];
        let data: UserInfoData = self.get("users.info", &params).await?;

        data.user
            .ok_or_else(|| color_eyre::eyre::eyre!("No user in response"))
    }

    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("HTTP error downloading {url}: {status}");
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }
}
