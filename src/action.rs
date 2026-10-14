use crate::activity::{ActivitySnapshot, ThreadTarget};
use crate::slack::types::{Channel, SlackMessage};

/// Which async operation an [`Action::Error`] belongs to.
/// `App::update` resets only the failed operation's pending state, so
/// an unrelated failure (e.g. a file download) can never clear the
/// double-send guard or cancel an in-flight thread open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorContext {
    /// auth.test validation at startup.
    Auth,
    /// conversations.list — the sidebar channel list.
    ChannelList,
    /// Foreground channel-history load (open or thread-close reload).
    ChannelLoad,
    /// chat.postMessage or file upload.
    Send,
    /// Per-file save-to-disk download.
    Download,
    /// reactions.add for an exact message.
    Reaction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTarget {
    pub channel_id: String,
    pub message_ts: String,
}

/// Every state mutation expressed as data.
/// `App::update` pattern-matches on these.
#[derive(Debug, Clone)]
pub enum Action {
    Tick,
    Render,
    Quit,
    OpenHelp,
    CloseHelp,

    // Mode transitions
    EnterNormalMode,
    EnterInsertMode,

    // Focus transitions
    FocusSidebar,
    FocusMessages,

    // Enter on a channel: open + focus messages + mark read
    OpenChannel,
    // Open the conversation named by the most recent new-chat notification.
    OpenLatestNotification,
    OpenActivity,
    RefreshActivity,
    ReadActivity,
    OpenActivityThread(ThreadTarget),
    ActivityLoaded(Result<ActivitySnapshot, String>),
    ThreadReadFinished {
        target: ThreadTarget,
        ts: String,
        result: Result<(), String>,
    },
    ThreadOpenFailed {
        target: ThreadTarget,
        message: String,
    },
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

    // Toggle the who-reacted popup for the selected message
    ToggleReactions,
    OpenReactionPicker,
    CloseReactionPicker,
    SubmitReaction,

    // Save files attached to the selected message to disk
    DownloadFiles,
    // Copy message text with Slack link markup replaced by its destination
    CopyMessage,
    OpenMessageLink,
    CloseLinkPicker,
    OpenUrl(reqwest::Url),
    BrowserFinished(Result<(), String>),

    // Async results from Slack API
    ReactionAdded {
        target: ReactionTarget,
        name: String,
    },
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
        /// Timestamp returned by `chat.postMessage`; file uploads do
        /// not currently expose a channel message timestamp here.
        message_ts: Option<String>,
    },
    ThreadRepliesLoaded {
        channel_id: String,
        thread_ts: String,
        messages: Vec<SlackMessage>,
    },
    /// A quiet background thread poll finished without data (error).
    /// Only clears the in-flight guard so the next poll can start.
    ThreadPollFailed,
    UserResolved {
        user_id: String,
        display_name: String,
        avatar_url: Option<String>,
    },
    AvatarDownloaded {
        user_id: String,
        /// Decoded off the UI task — decoding multi-megapixel images
        /// in `update()` would freeze input and rendering.
        image: Box<image::DynamicImage>,
    },
    FileImageDownloaded {
        image_key: String,
        image: Box<image::DynamicImage>,
    },
    /// Download or decode of an inline image preview failed; allows a
    /// bounded number of retries instead of suppressing it forever.
    FileImageFailed {
        image_key: String,
    },
    FileDownloaded {
        dest: std::path::PathBuf,
    },
    StarsLoaded(std::collections::HashSet<String>),
    CustomEmojiLoaded(std::collections::HashMap<String, String>),
    AuthValidated {
        user_id: String,
        user_name: String,
    },

    Error {
        context: ErrorContext,
        message: String,
    },
}
