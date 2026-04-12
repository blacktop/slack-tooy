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
    /// Bot messages omit `user` and provide these instead.
    #[serde(default)]
    pub bot_id: String,
    #[serde(default)]
    pub username: String,
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
pub struct UserInfoData {
    pub user: Option<User>,
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
