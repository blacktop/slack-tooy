use crate::slack::types::{Channel, SlackMessage};

/// Every state mutation expressed as data.
/// `App::update` pattern-matches on these.
#[derive(Debug, Clone)]
pub enum Action {
    Tick,
    Render,
    Quit,

    // Mode transitions
    EnterNormalMode,
    EnterInsertMode,

    // Focus transitions
    FocusSidebar,
    FocusMessages,

    // Enter on a channel: open + focus messages + mark read
    OpenChannel,
    // Mark all channels as read
    MarkAllRead,
    // Toggle between showing all channels vs only unread
    ToggleUnreadFilter,

    // Thread: open thread for the selected message
    OpenThread(String),
    // Thread: close thread view, return to channel
    CloseThread,

    // Message sending (from input component)
    SendMessage,

    // Async results from Slack API
    ChannelsLoaded(Vec<Channel>),
    MessagesLoaded {
        channel_id: String,
        messages: Vec<SlackMessage>,
        /// True for background poll responses (1-message fetches).
        /// These should never trigger `set_channel` or mark-read.
        is_background: bool,
    },
    MessageSent {
        channel_id: String,
        thread_ts: Option<String>,
    },
    ThreadRepliesLoaded {
        channel_id: String,
        thread_ts: String,
        messages: Vec<SlackMessage>,
    },
    UserResolved {
        user_id: String,
        display_name: String,
        avatar_url: Option<String>,
    },
    AvatarDownloaded {
        user_id: String,
        image_data: Vec<u8>,
    },
    CustomEmojiLoaded(std::collections::HashMap<String, String>),
    AuthValidated {
        user_name: String,
    },

    Error(String),
}
