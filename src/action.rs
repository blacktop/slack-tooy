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
    /// conversations.replies fetch while opening a thread.
    ThreadOpen,
    /// chat.postMessage or file upload.
    Send,
    /// Per-file save-to-disk download.
    Download,
}

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

    // Save files attached to the selected message to disk
    DownloadFiles,

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
