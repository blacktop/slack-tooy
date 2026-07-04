use serde::Deserialize;

/// Slack channel/conversation from `conversations.list`.
#[derive(Debug, Clone, Deserialize)]
#[expect(clippy::struct_field_names, reason = "Slack API field names")]
pub struct Channel {
    pub id: String,
    pub name: Option<String>,
    /// Needed by serde for API responses.
    pub is_channel: Option<bool>,
    pub is_im: Option<bool>,
    pub is_member: Option<bool>,
    #[serde(default)]
    pub user: String,
}

impl Channel {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("dm")
    }
}

/// Slack message from `conversations.history`.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackMessage {
    pub ts: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub text: String,
    pub thread_ts: Option<String>,
    pub reply_count: Option<u32>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
    /// Bot messages omit `user` and provide these instead.
    #[serde(default)]
    pub bot_id: String,
    #[serde(default)]
    pub username: String,
}

/// File attached to a Slack message.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackFile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mimetype: String,
    #[serde(default)]
    pub url_private: String,
    #[serde(default)]
    pub url_private_download: String,
    #[serde(default)]
    pub thumb_360: String,
    #[serde(default)]
    pub thumb_480: String,
    #[serde(default)]
    pub thumb_720: String,
    #[serde(default)]
    pub thumb_1024: String,
}

impl SlackFile {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.title
        } else {
            &self.name
        }
    }

    pub fn is_image(&self) -> bool {
        self.mimetype.starts_with("image/")
    }

    pub fn image_url(&self) -> Option<&str> {
        [
            self.thumb_1024.as_str(),
            self.thumb_720.as_str(),
            self.thumb_480.as_str(),
            self.thumb_360.as_str(),
            self.url_private_download.as_str(),
            self.url_private.as_str(),
        ]
        .into_iter()
        .find(|url| !url.is_empty())
    }

    /// Full-file URL for saving to disk (never a thumbnail).
    pub fn download_url(&self) -> Option<&str> {
        [
            self.url_private_download.as_str(),
            self.url_private.as_str(),
        ]
        .into_iter()
        .find(|url| !url.is_empty())
    }

    pub fn image_key(&self) -> Option<String> {
        if self.id.is_empty() {
            self.image_url().map(String::from)
        } else {
            Some(self.id.clone())
        }
    }
}

impl SlackMessage {
    /// Best available sender identifier — prefers `user`, falls back
    /// to `bot_id`, then `username`.
    pub fn sender_id(&self) -> &str {
        if !self.user.is_empty() {
            &self.user
        } else if !self.bot_id.is_empty() {
            &self.bot_id
        } else {
            &self.username
        }
    }
}

pub fn mentioned_user_ids(text: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    let mut remainder = text;

    while let Some(start) = remainder.find("<@") {
        let after_start = &remainder[start + 2..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        let mention = &after_start[..end];
        if let Some((user_id, _)) = parse_user_mention(mention) {
            ids.push(user_id);
        }
        remainder = &after_start[end + 1..];
    }

    ids
}

pub fn replace_user_mentions(
    text: &str,
    mut resolve_name: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut rendered = String::with_capacity(text.len());
    let mut remainder = text;

    while let Some(start) = remainder.find("<@") {
        rendered.push_str(&remainder[..start]);

        let after_start = &remainder[start + 2..];
        let Some(end) = after_start.find('>') else {
            rendered.push_str(&remainder[start..]);
            return rendered;
        };

        let mention = &after_start[..end];
        if let Some((user_id, fallback)) = parse_user_mention(mention) {
            let display_name = resolve_name(user_id).unwrap_or_else(|| fallback.to_string());
            rendered.push('@');
            rendered.push_str(&display_name);
        } else {
            rendered.push_str("<@");
            rendered.push_str(mention);
            rendered.push('>');
        }

        remainder = &after_start[end + 1..];
    }

    rendered.push_str(remainder);
    rendered
}

pub fn is_slack_user_id(user_id: &str) -> bool {
    matches!(user_id.chars().next(), Some('U' | 'W'))
}

fn parse_user_mention(mention: &str) -> Option<(&str, &str)> {
    let (user_id, fallback) = mention
        .split_once('|')
        .map_or((mention, mention), |(user_id, label)| {
            (user_id, label.strip_prefix('@').unwrap_or(label))
        });

    if user_id.is_empty() || !is_slack_user_id(user_id) {
        return None;
    }

    Some((user_id, fallback))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reaction {
    pub name: String,
    pub count: u32,
}

/// Slack user from `users.info`.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub real_name: Option<String>,
    pub profile: Option<UserProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserProfile {
    pub display_name: Option<String>,
    pub image_32: Option<String>,
}

impl User {
    pub fn best_name(&self) -> &str {
        self.profile
            .as_ref()
            .and_then(|p| p.display_name.as_deref())
            .filter(|s| !s.is_empty())
            .or(self.real_name.as_deref())
            .unwrap_or(&self.name)
    }

    pub fn avatar_url(&self) -> Option<&str> {
        self.profile.as_ref().and_then(|p| p.image_32.as_deref())
    }
}

/// Auth test result from `auth.test`.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthInfo {
    pub user_id: String,
    pub user: String,
    pub team: Option<String>,
    pub team_id: Option<String>,
}

// --- API response wrappers ---

#[derive(Debug, Deserialize)]
pub struct SlackApiResponse<T> {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsListData {
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsHistoryData {
    #[serde(default)]
    pub messages: Vec<SlackMessage>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct PostMessageData {
    pub ts: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadUrlData {
    pub upload_url: String,
    pub file_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteUploadData {
    #[serde(default)]
    pub files: Vec<CompletedUploadFile>,
}

#[derive(Debug, Deserialize)]
pub struct CompletedUploadFile {
    pub id: String,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserInfoData {
    pub user: Option<User>,
}

#[derive(Debug, Deserialize)]
pub struct StarsListData {
    #[serde(default)]
    pub items: Vec<StarItem>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct StarItem {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub channel: String,
}

#[derive(Debug, Deserialize)]
pub struct EmojiListData {
    #[serde(default)]
    pub emoji: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMetadata {
    pub next_cursor: Option<String>,
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn decode_conversations_list() {
        let json = r#"{
            "ok": true,
            "channels": [
                {
                    "id": "C1",
                    "name": "general",
                    "is_channel": true,
                    "is_im": false,
                    "is_member": true,
                    "user": ""
                },
                {
                    "id": "D1",
                    "name": null,
                    "is_channel": false,
                    "is_im": true,
                    "is_member": true,
                    "user": "U123"
                }
            ],
            "response_metadata": {
                "next_cursor": "abc123"
            }
        }"#;
        let resp: SlackApiResponse<ConversationsListData> =
            serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(resp.data.channels.len(), 2);
        assert_eq!(resp.data.channels[0].display_name(), "general");
        assert_eq!(resp.data.channels[1].display_name(), "dm");
        assert_eq!(
            resp.data
                .response_metadata
                .as_ref()
                .and_then(|m| m.next_cursor.as_deref()),
            Some("abc123")
        );
    }

    #[test]
    fn decode_conversations_history() {
        let json = r#"{
            "ok": true,
            "messages": [
                {
                    "ts": "1712500000.000100",
                    "user": "U1",
                    "text": "Hello, world!",
                    "thread_ts": null,
                    "reply_count": null
                },
                {
                    "ts": "1712500060.000200",
                    "user": "U2",
                    "text": "Hi there",
                    "thread_ts": "1712500000.000100",
                    "reply_count": 3
                }
            ]
        }"#;
        let resp: SlackApiResponse<ConversationsHistoryData> =
            serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(resp.data.messages.len(), 2);
        assert_eq!(resp.data.messages[0].text, "Hello, world!");
        assert_eq!(resp.data.messages[1].reply_count, Some(3));
    }

    #[test]
    fn decode_file_share_message() {
        let json = r#"{
            "ok": true,
            "messages": [
                {
                    "ts": "1712500100.000300",
                    "user": "U1",
                    "text": "",
                    "files": [
                        {
                            "id": "F123",
                            "name": "report.pdf",
                            "size": 2621440,
                            "mimetype": "application/pdf"
                        }
                    ]
                }
            ]
        }"#;
        let resp: SlackApiResponse<ConversationsHistoryData> =
            serde_json::from_str(json).expect("decode");
        assert_eq!(resp.data.messages.len(), 1);
        let msg = &resp.data.messages[0];
        assert!(msg.text.is_empty());
        assert_eq!(msg.files.len(), 1);
        assert_eq!(msg.files[0].name, "report.pdf");
        assert_eq!(msg.files[0].size, 2_621_440);
        assert_eq!(msg.files[0].mimetype, "application/pdf");
    }

    #[test]
    fn decode_post_message() {
        let json = r#"{
            "ok": true,
            "ts": "1712500100.000300"
        }"#;
        let resp: SlackApiResponse<PostMessageData> = serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(resp.data.ts.as_deref(), Some("1712500100.000300"));
    }

    #[test]
    fn decode_get_upload_url_external() {
        let json = r#"{
            "ok": true,
            "upload_url": "https://files.slack.com/upload/v1/ABC123",
            "file_id": "F123ABC456"
        }"#;
        let resp: SlackApiResponse<UploadUrlData> = serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(
            resp.data.upload_url,
            "https://files.slack.com/upload/v1/ABC123"
        );
        assert_eq!(resp.data.file_id, "F123ABC456");
    }

    #[test]
    fn decode_complete_upload_external() {
        let json = r#"{
            "ok": true,
            "files": [
                {
                    "id": "F123ABC456",
                    "title": "cat.png"
                }
            ]
        }"#;
        let resp: SlackApiResponse<CompleteUploadData> =
            serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(resp.data.files.len(), 1);
        assert_eq!(resp.data.files[0].id, "F123ABC456");
        assert_eq!(resp.data.files[0].title.as_deref(), Some("cat.png"));
    }

    #[test]
    fn decode_users_info() {
        let json = r#"{
            "ok": true,
            "user": {
                "id": "U1",
                "name": "alice",
                "real_name": "Alice Smith",
                "profile": {
                    "display_name": "aliceS"
                }
            }
        }"#;
        let resp: SlackApiResponse<UserInfoData> = serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        let user = resp.data.user.expect("user present");
        assert_eq!(user.best_name(), "aliceS");
    }

    #[test]
    fn decode_auth_test() {
        let json = r#"{
            "ok": true,
            "user_id": "U1",
            "user": "alice",
            "team": "T1",
            "team_id": "T123"
        }"#;
        let resp: SlackApiResponse<AuthInfo> = serde_json::from_str(json).expect("decode");
        assert!(resp.ok);
        assert_eq!(resp.data.user_id, "U1");
        assert_eq!(resp.data.user, "alice");
    }

    #[test]
    fn decode_error_response() {
        let json = r#"{
            "ok": false,
            "error": "invalid_auth"
        }"#;
        let resp: SlackApiResponse<PostMessageData> = serde_json::from_str(json).expect("decode");
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("invalid_auth"));
    }

    #[test]
    fn user_best_name_fallback_chain() {
        let with_display = User {
            id: "U1".into(),
            name: "alice".into(),
            real_name: Some("Alice Smith".into()),
            profile: Some(UserProfile {
                display_name: Some("aliceS".into()),
                image_32: None,
            }),
        };
        assert_eq!(with_display.best_name(), "aliceS");

        let with_real_name = User {
            id: "U1".into(),
            name: "alice".into(),
            real_name: Some("Alice Smith".into()),
            profile: Some(UserProfile {
                display_name: Some(String::new()),
                image_32: None,
            }),
        };
        assert_eq!(with_real_name.best_name(), "Alice Smith");

        let name_only = User {
            id: "U1".into(),
            name: "alice".into(),
            real_name: None,
            profile: None,
        };
        assert_eq!(name_only.best_name(), "alice");
    }

    #[test]
    fn mentioned_user_ids_extract_all_mentions() {
        let ids = mentioned_user_ids("hi <@U1> and <@U2|alice.smith>");

        assert_eq!(ids, vec!["U1", "U2"]);
    }

    #[test]
    fn replace_user_mentions_uses_resolved_names() {
        let rendered =
            replace_user_mentions(
                "hello <@U1> and <@U2|alice.smith>",
                |user_id| match user_id {
                    "U1" => Some("Alice".to_string()),
                    "U2" => Some("Bob".to_string()),
                    _ => None,
                },
            );

        assert_eq!(rendered, "hello @Alice and @Bob");
    }

    #[test]
    fn replace_user_mentions_falls_back_to_label_or_user_id() {
        let rendered = replace_user_mentions("hello <@U1|alice.smith> and <@U2>", |_| None);

        assert_eq!(rendered, "hello @alice.smith and @U2");
    }

    #[test]
    fn replace_user_mentions_leaves_non_user_mentions_unchanged() {
        let rendered = replace_user_mentions("hello <@B1|deploy-bot>", |_| None);

        assert_eq!(rendered, "hello <@B1|deploy-bot>");
    }

    #[test]
    fn channel_display_name_fallback() {
        let named = Channel {
            id: "C1".into(),
            name: Some("general".into()),
            is_channel: Some(true),
            is_im: Some(false),
            is_member: Some(true),
            user: String::new(),
        };
        assert_eq!(named.display_name(), "general");

        let dm = Channel {
            id: "D1".into(),
            name: None,
            is_channel: Some(false),
            is_im: Some(true),
            is_member: Some(true),
            user: "U123".into(),
        };
        assert_eq!(dm.display_name(), "dm");
    }
}
