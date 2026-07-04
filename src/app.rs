use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use color_eyre::eyre::{Report, Result, WrapErr};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use tokio::sync::mpsc;

use crate::action::{Action, ErrorContext};
use crate::components::input::TextInput;
use crate::components::messages::MessageList;
use crate::components::sidebar::ChannelSidebar;
use crate::components::{Component, EventResult};
use crate::config::Config;
use crate::event::{AppEvent, EventHandler};
use crate::slack::client::SlackClient;
use crate::slack::types::{Channel, SlackFile, SlackMessage, is_slack_user_id, mentioned_user_ids};
use crate::store::Store;
use crate::tui::Tui;
use crate::ui;
use ratatui_image::picker::Picker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Messages,
}

/// Why the current foreground `load_messages` was initiated.
/// Controls whether the response should mark the channel as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadReason {
    /// User explicitly opened this channel — mark as read.
    ChannelOpen,
    /// Reloading after closing a thread — don't mark as read.
    ThreadClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalBell {
    Idle,
    Pending,
}

#[derive(Debug)]
struct UploadCommand {
    path: PathBuf,
    initial_comment: Option<String>,
}

const UPLOAD_USAGE: &str = "Usage: /upload <path> [comment]";

/// Give up fetching an inline image preview after this many failures
/// so a permanently broken URL doesn't retry forever, while transient
/// failures still recover.
const MAX_IMAGE_FETCH_ATTEMPTS: u32 = 3;

/// Progress of one `d`-key download batch.  Files finish (or fail)
/// independently; a single shared status line would let a later
/// "Saved" overwrite an earlier failure.
#[derive(Debug, Default)]
struct DownloadBatch {
    pending: usize,
    total: usize,
    failed: usize,
    last_dest: Option<PathBuf>,
    last_error: Option<String>,
}

impl DownloadBatch {
    fn summary(&self) -> String {
        if self.failed == 0 {
            match (&self.last_dest, self.total) {
                (Some(dest), 1) => format!("Saved {}", dest.display()),
                (_, n) => format!("Saved {n} files"),
            }
        } else {
            let error = self
                .last_error
                .clone()
                .unwrap_or_else(|| "Download failed".to_string());
            format!("{error} ({} of {} failed)", self.failed, self.total)
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "loading/closing_thread/thread_poll_in_flight belong to one \
              channel-view lifecycle pending a state-machine refactor"
)]
pub struct App {
    pub config: Config,
    pub mode: Mode,
    pub focus: Focus,
    pub should_quit: bool,

    pub sidebar: ChannelSidebar,
    pub messages: MessageList,
    pub input: TextInput,

    pub slack: SlackClient,
    pub current_channel_id: Option<String>,

    action_tx: mpsc::UnboundedSender<Action>,
    pub picker: Option<Picker>,
    store: Store,
    last_poll: Instant,
    /// Newest message ts the user has seen per channel.
    last_seen_ts: HashMap<String, String>,
    /// Newest message ts we know about per channel.
    latest_ts: HashMap<String, String>,
    /// Index into channels list for background polling rotation.
    poll_rotation: usize,
    /// Custom emoji name -> image URL (from emoji.list).
    custom_emoji_urls: HashMap<String, String>,
    requested_file_images: HashSet<String>,
    /// Failed preview fetch attempts per image key — bounded retry.
    failed_file_images: HashMap<String, u32>,
    /// users.info requests already issued.  A failed lookup stays in
    /// the set so it isn't re-fetched every poll cycle for the rest of
    /// the session.
    requested_users: HashSet<String>,
    /// Authenticated user id from auth.test — used to avoid flagging
    /// the user's own messages as unread.
    self_user_id: Option<String>,
    /// True while a quiet background thread poll is in flight.  Large
    /// threads paginate and can outlive the poll interval; overlapping
    /// fetches multiply the request rate.
    thread_poll_in_flight: bool,
    /// Draft captured at send time, restored into the input if the
    /// send fails (unless the user typed something new meanwhile).
    in_flight_send_text: Option<String>,
    /// Progress of the current `d`-key download batch, if any.
    download_batch: Option<DownloadBatch>,
    /// Whether the terminal reports modified keys (kitty keyboard
    /// protocol) — controls the Shift+Enter hint.
    pub keyboard_enhanced: bool,
    /// Pane rectangles from the last layout pass, for mouse routing.
    pub panes: crate::ui::Panes,
    /// Thread ts currently being fetched — thread mode enters only
    /// after replies arrive.
    pending_thread: Option<String>,
    /// True while waiting for channel history to reload after closing
    /// a thread.  The UI stays in thread mode until the channel data
    /// arrives so stale thread replies are never shown under the
    /// channel title.
    closing_thread: bool,
    pending_terminal_bell: TerminalBell,
    /// Per-channel cutoff timestamps captured at `MarkAllRead` time.
    /// Messages at or before the cutoff are considered read; messages
    /// after are genuinely new and should trigger unread.
    mark_all_read_cutoffs: HashMap<String, String>,
    /// Deferred unread removal: keeps the current channel visible in
    /// the unread list until the user navigates away.
    deferred_read_channel: Option<String>,

    pub status_message: Option<String>,
    pub loading: bool,
    /// Why the current foreground load was initiated.
    load_reason: LoadReason,
    /// Number of `send_message` calls awaiting `MessageSent`.
    /// Input is cleared only when this reaches 0.
    pending_sends: u32,
}

fn parse_upload_command(text: &str) -> Result<Option<UploadCommand>, String> {
    const COMMAND: &str = "/upload";
    let trimmed = text.trim();
    if trimmed == COMMAND {
        return Err(UPLOAD_USAGE.to_string());
    }
    let Some(rest) = trimmed.strip_prefix("/upload ") else {
        return Ok(None);
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err(UPLOAD_USAGE.to_string());
    }

    let (path, comment) = parse_upload_parts(rest)?;
    let comment = comment.trim();
    let initial_comment = if comment.is_empty() {
        None
    } else {
        Some(comment.to_string())
    };

    Ok(Some(UploadCommand {
        path: expand_tilde(&path),
        initial_comment,
    }))
}

/// Expand a leading `~` / `~/` to the home directory — the form people
/// type interactively.  Other paths pass through unchanged.
fn expand_tilde(path: &str) -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home;
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

fn format_error_chain(prefix: &str, error: &Report) -> String {
    let details = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");

    if details.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {details}")
    }
}

fn parse_upload_parts(input: &str) -> Result<(String, &str), String> {
    if input.starts_with('"') {
        parse_quoted_upload_path(input)
    } else {
        Ok(parse_plain_upload_path(input))
    }
}

#[cfg(test)]
mod upload_command_tests {
    use super::{UPLOAD_USAGE, parse_upload_command};

    #[test]
    fn parse_upload_command_ignores_regular_messages() {
        let parsed = parse_upload_command("hello /upload image.png");

        assert!(matches!(parsed, Ok(None)));
    }

    #[test]
    fn parse_upload_command_accepts_plain_path_and_comment() {
        let parsed = parse_upload_command("/upload /tmp/cat.png look at this");
        assert!(matches!(parsed, Ok(Some(_))));
        let Ok(Some(parsed)) = parsed else {
            return;
        };

        assert_eq!(parsed.path.to_string_lossy(), "/tmp/cat.png");
        assert_eq!(parsed.initial_comment.as_deref(), Some("look at this"));
    }

    #[test]
    fn parse_upload_command_accepts_quoted_path() {
        let parsed = parse_upload_command(r#"/upload "/tmp/cat pic.png" caption"#);
        assert!(matches!(parsed, Ok(Some(_))));
        let Ok(Some(parsed)) = parsed else {
            return;
        };

        assert_eq!(parsed.path.to_string_lossy(), "/tmp/cat pic.png");
        assert_eq!(parsed.initial_comment.as_deref(), Some("caption"));
    }

    #[test]
    fn parse_upload_command_rejects_missing_path() {
        let parsed = parse_upload_command("/upload");

        assert!(matches!(
            parsed,
            Err(ref message) if message == UPLOAD_USAGE
        ));
    }
}

fn parse_plain_upload_path(input: &str) -> (String, &str) {
    match input.find(char::is_whitespace) {
        Some(idx) => (input[..idx].to_string(), &input[idx..]),
        None => (input.to_string(), ""),
    }
}

fn parse_quoted_upload_path(input: &str) -> Result<(String, &str), String> {
    let mut path = String::new();
    let mut escaped = false;

    for (idx, c) in input[1..].char_indices() {
        if escaped {
            path.push(c);
            escaped = false;
            continue;
        }

        match c {
            '\\' => escaped = true,
            '"' => {
                let rest_index = idx + 1 + c.len_utf8();
                return Ok((path, &input[rest_index..]));
            }
            c => path.push(c),
        }
    }

    if escaped {
        path.push('\\');
    }
    Err("Unclosed quoted upload path".to_string())
}

impl App {
    pub fn new(
        config: Config,
        action_tx: mpsc::UnboundedSender<Action>,
        picker: Option<Picker>,
        store: Store,
    ) -> Result<Self> {
        let slack = SlackClient::new(&config.slack_token, &config.cookie)?;

        // Load persisted read state from SQLite
        let last_seen_ts = store.all_read_state().unwrap_or_default();

        Ok(Self {
            config,
            mode: Mode::Normal,
            focus: Focus::Sidebar,
            should_quit: false,
            sidebar: ChannelSidebar::new(),
            messages: MessageList::new(),
            input: TextInput::new(),
            slack,
            current_channel_id: None,
            action_tx,
            picker,
            store,
            last_poll: Instant::now(),
            last_seen_ts,
            latest_ts: HashMap::new(),
            poll_rotation: 0,
            custom_emoji_urls: HashMap::new(),
            requested_file_images: HashSet::new(),
            failed_file_images: HashMap::new(),
            requested_users: HashSet::new(),
            self_user_id: None,
            thread_poll_in_flight: false,
            in_flight_send_text: None,
            download_batch: None,
            keyboard_enhanced: false,
            panes: crate::ui::Panes::default(),
            pending_thread: None,
            closing_thread: false,
            pending_terminal_bell: TerminalBell::Idle,
            mark_all_read_cutoffs: HashMap::new(),
            deferred_read_channel: None,
            status_message: Some("Loading channels...".into()),
            loading: true,
            load_reason: LoadReason::ChannelOpen,
            pending_sends: 0,
        })
    }

    pub async fn run(
        &mut self,
        tui: &mut Tui,
        action_rx: mpsc::UnboundedReceiver<Action>,
    ) -> Result<()> {
        let mut events = EventHandler::new(self.config.tick_rate(), action_rx);
        self.keyboard_enhanced = tui.keyboard_enhanced();

        self.validate_auth();
        self.load_channels();
        self.load_stars();
        self.load_custom_emoji();

        loop {
            tui.draw(|frame| ui::render(frame, self))?;

            let action = match events.next().await? {
                AppEvent::Tick => Action::Tick,
                AppEvent::Resize => Action::Render,
                AppEvent::Key(key) => self.handle_key(key),
                AppEvent::Mouse(mouse) => self.handle_mouse(mouse),
                AppEvent::Paste(text) => self.handle_paste(&text),
                AppEvent::BackgroundAction(action) => action,
            };

            self.update(action);
            if self.take_pending_terminal_bell() {
                tui.ring_bell()?;
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Insert => self.handle_insert_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match (key.code, ctrl) {
            // Quit
            (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => Action::Quit,
            // Insert mode
            (KeyCode::Char('i'), false) => Action::EnterInsertMode,
            // Focus toggle — Tab always switches panels.
            // Right arrow from Sidebar enters Messages; from Messages
            // it falls through to the message handler (→ opens thread).
            // Left arrow from Messages with an active thread closes it.
            (KeyCode::Tab | KeyCode::BackTab, _) => match self.focus {
                Focus::Sidebar => Action::FocusMessages,
                Focus::Messages => Action::FocusSidebar,
            },
            (KeyCode::Right, _) if self.focus == Focus::Sidebar => Action::FocusMessages,
            (KeyCode::Left, _)
                if self.focus == Focus::Messages && self.messages.active_thread.is_some() =>
            {
                Action::CloseThread
            }
            (KeyCode::Left, _) if self.focus == Focus::Messages => Action::FocusSidebar,
            // Number shortcuts: direct pane access
            (KeyCode::Char('1'), false) => Action::FocusSidebar,
            (KeyCode::Char('2'), false) => Action::FocusMessages,
            _ => {
                let result = match self.focus {
                    Focus::Sidebar => self.sidebar.handle_key(key),
                    Focus::Messages => self.messages.handle_key(key),
                };
                match result {
                    EventResult::Action(action) => action,
                    _ => Action::Tick,
                }
            }
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> Action {
        let result = self.input.handle_key(key);
        match result {
            EventResult::Action(action) => action,
            _ => Action::Tick,
        }
    }

    fn handle_paste(&mut self, text: &str) -> Action {
        if self.mode == Mode::Insert {
            self.input.insert_text(text);
        }
        Action::Tick
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Action {
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_left_click(position),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if self.panes.messages.contains(position) {
                    self.messages.handle_scroll(up);
                } else if self.panes.sidebar.contains(position) {
                    self.sidebar.handle_scroll(up);
                }
                Action::Tick
            }
            MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
            | MouseEventKind::Up(_)
            | MouseEventKind::Drag(_)
            | MouseEventKind::Moved
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => Action::Tick,
        }
    }

    fn handle_left_click(&mut self, position: Position) -> Action {
        if self.panes.sidebar.contains(position) {
            self.mode = Mode::Normal;
            // A channel row returns OpenChannel (which focuses the
            // messages pane); header/empty clicks just take focus.
            if let Some(action) = self.sidebar.handle_click(position) {
                return action;
            }
            return Action::FocusSidebar;
        }
        if self.panes.messages.contains(position) {
            self.mode = Mode::Normal;
            self.messages.handle_click(position);
            return Action::FocusMessages;
        }
        if self.panes.input.contains(position) {
            return Action::EnterInsertMode;
        }
        Action::Tick
    }

    fn take_pending_terminal_bell(&mut self) -> bool {
        let should_ring = self.pending_terminal_bell == TerminalBell::Pending;
        self.pending_terminal_bell = TerminalBell::Idle;
        should_ring
    }

    fn mark_unread_and_maybe_bell(&mut self, channel_id: &str, should_bell: bool) {
        let newly_unread = self.sidebar.unread_channels.insert(channel_id.to_string());
        if newly_unread && should_bell {
            self.pending_terminal_bell = TerminalBell::Pending;
        }
    }

    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::EnterNormalMode => self.mode = Mode::Normal,
            Action::EnterInsertMode => self.mode = Mode::Insert,
            Action::FocusSidebar => self.focus = Focus::Sidebar,
            Action::FocusMessages => self.focus = Focus::Messages,
            Action::OpenChannel => self.handle_open_channel(),
            Action::MarkAllRead => self.handle_mark_all_read(),
            Action::ToggleUnreadFilter => {
                self.sidebar.filter_unread = !self.sidebar.filter_unread;
                if self.sidebar.filter_unread {
                    self.sidebar.snap_selection_to_visible();
                } else {
                    self.flush_deferred_read();
                }
            }
            Action::OpenThread(ref thread_ts) => {
                self.handle_open_thread(thread_ts);
            }
            Action::CloseThread => {
                self.handle_close_thread();
            }
            Action::SendMessage => {
                self.handle_send_message();
            }
            Action::DownloadFiles => {
                self.handle_download_files();
            }
            Action::FileDownloaded { dest } => {
                self.handle_download_finished(Ok(dest));
            }
            Action::ChannelsLoaded(channels) => {
                self.handle_channels_loaded(channels);
            }
            Action::MessagesLoaded {
                channel_id,
                messages,
                is_background,
            } => {
                self.handle_messages_loaded(&channel_id, messages, is_background);
            }
            Action::ThreadRepliesLoaded {
                channel_id,
                thread_ts,
                messages,
            } => {
                self.handle_thread_replies_loaded(&channel_id, &thread_ts, messages);
            }
            Action::ThreadPollFailed => {
                self.thread_poll_in_flight = false;
            }
            Action::MessageSent {
                channel_id,
                thread_ts,
                message_ts,
            } => {
                self.handle_message_sent(&channel_id, thread_ts.as_deref(), message_ts.as_deref());
            }
            Action::UserResolved {
                user_id,
                display_name,
                avatar_url,
            } => {
                self.handle_user_resolved(user_id, &display_name, avatar_url);
            }
            Action::AvatarDownloaded { user_id, image } => {
                self.handle_avatar_downloaded(&user_id, *image);
            }
            Action::FileImageDownloaded { image_key, image } => {
                self.handle_file_image_downloaded(&image_key, *image);
            }
            Action::FileImageFailed { image_key } => {
                self.handle_file_image_failed(&image_key);
            }
            Action::StarsLoaded(starred) => {
                self.sidebar.set_starred(starred);
            }
            Action::CustomEmojiLoaded(emoji_map) => {
                self.handle_custom_emoji_loaded(&emoji_map);
            }
            Action::AuthValidated { user_id, user_name } => {
                tracing::info!("Authenticated as {user_name}");
                self.self_user_id = Some(user_id);
            }
            Action::Error { context, message } => {
                self.handle_error(context, message);
            }
            Action::Tick => {
                self.poll_if_due();
            }
            Action::Render => {}
        }
    }

    /// Reset only the failed operation's pending state — an unrelated
    /// failure must never clear the double-send guard, cancel an
    /// in-flight thread open, or wipe the user's draft.
    fn handle_error(&mut self, context: ErrorContext, message: String) {
        tracing::error!("{message}");
        match context {
            ErrorContext::Download => {
                self.handle_download_finished(Err(message));
            }
            ErrorContext::Send => {
                self.status_message = Some(message);
                self.pending_sends = 0;
                // Restore the draft that failed to send so the user
                // can fix and retry — unless they typed a new one.
                let failed_draft = self.in_flight_send_text.take();
                if self.input.is_empty()
                    && let Some(text) = failed_draft
                {
                    self.input.insert_text(&text);
                }
            }
            ErrorContext::ThreadOpen => {
                self.status_message = Some(message);
                self.pending_thread = None;
                self.loading = false;
            }
            ErrorContext::ChannelLoad => {
                self.status_message = Some(message);
                self.closing_thread = false;
                self.loading = false;
            }
            ErrorContext::ChannelList => {
                self.status_message = Some(message);
                self.loading = false;
            }
            ErrorContext::Auth => {
                self.status_message = Some(message);
            }
        }
    }

    fn handle_download_finished(&mut self, outcome: std::result::Result<PathBuf, String>) {
        let Some(batch) = self.download_batch.as_mut() else {
            // Stray result after state reset — surface it directly.
            self.status_message = Some(match outcome {
                Ok(dest) => format!("Saved {}", dest.display()),
                Err(message) => message,
            });
            return;
        };

        batch.pending = batch.pending.saturating_sub(1);
        match outcome {
            Ok(dest) => batch.last_dest = Some(dest),
            Err(message) => {
                batch.failed += 1;
                batch.last_error = Some(message);
            }
        }

        if batch.pending == 0 {
            self.status_message = Some(batch.summary());
            self.download_batch = None;
        } else {
            let done = batch.total - batch.pending;
            self.status_message = Some(format!("Downloading... {done} of {} done", batch.total));
        }
    }

    fn handle_file_image_failed(&mut self, image_key: &str) {
        self.requested_file_images.remove(image_key);
        *self
            .failed_file_images
            .entry(image_key.to_string())
            .or_insert(0) += 1;
    }

    fn mark_self_sent_message_read(&mut self, channel_id: &str, ts: &str) {
        self.last_seen_ts
            .insert(channel_id.to_string(), ts.to_string());
        self.mark_all_read_cutoffs.remove(channel_id);
        if let Err(e) = self.store.mark_read(channel_id, ts) {
            tracing::warn!("Failed to persist read state: {e}");
        }
    }

    fn handle_message_sent(
        &mut self,
        channel_id: &str,
        thread_ts: Option<&str>,
        message_ts: Option<&str>,
    ) {
        self.pending_sends = self.pending_sends.saturating_sub(1);
        if self.pending_sends == 0 {
            self.in_flight_send_text = None;
        }
        if thread_ts.is_none()
            && let Some(ts) = message_ts
        {
            self.mark_self_sent_message_read(channel_id, ts);
        }
        // Quiet refreshes: the send already succeeded, and a loud
        // failure here would reset thread/channel state belonging to
        // whatever the user is doing now.  The 5s poll self-heals.
        if let Some(ts) = thread_ts {
            self.load_thread_replies_quiet(channel_id, ts);
        } else {
            self.load_messages_refresh(channel_id);
        }
    }

    fn flush_deferred_read(&mut self) {
        if let Some(ch) = self.deferred_read_channel.take() {
            self.sidebar.unread_channels.remove(&ch);
        }
    }

    fn handle_open_channel(&mut self) {
        let Some(channel) = self.sidebar.selected_channel() else {
            return;
        };
        let channel_id = channel.id.clone();
        let channel_name = self.sidebar.channel_label(channel);
        self.focus = Focus::Messages;
        // Cancel any in-flight thread fetch regardless of whether the
        // channel changed — re-opening the same channel while a thread
        // is loading should not drop into thread mode later.
        let cancelled_thread_open = self.pending_thread.take().is_some();
        if self.current_channel_id.as_deref() != Some(&channel_id) {
            self.flush_deferred_read();
            self.current_channel_id = Some(channel_id.clone());
            self.closing_thread = false;
            // Show the new channel name with an empty message list
            // immediately so stale content from the previous channel
            // is never visible while the fetch is in flight.
            self.messages.set_channel(Vec::new(), channel_name);
            self.loading = true;
            self.load_reason = LoadReason::ChannelOpen;
            self.load_messages(&channel_id);
        } else if cancelled_thread_open {
            // The cancelled thread fetch owned the loading flag; its
            // response will be dropped as stale and would otherwise
            // leave `loading` stuck until the next poll misread it as
            // a channel open (mark-read + scroll reset).
            self.loading = false;
        } else if self.messages.active_thread.is_some() {
            // Re-opening the current channel exits thread view — a
            // click on the channel name means "back to the channel".
            self.handle_close_thread();
        }
    }

    fn handle_mark_all_read(&mut self) {
        self.sidebar.mark_all_read();
        self.deferred_read_channel = None;
        // Persist for channels with known timestamps — one transaction
        // so the event loop isn't stalled by a WAL sync per channel.
        for (ch_id, ts) in &self.latest_ts {
            self.last_seen_ts.insert(ch_id.clone(), ts.clone());
        }
        let entries = self
            .latest_ts
            .iter()
            .map(|(id, ts)| (id.as_str(), ts.as_str()));
        if let Err(e) = self.store.mark_read_many(entries) {
            tracing::warn!("Failed to persist read state: {e}");
        }
        // For channels that haven't been hydrated yet, record a
        // cutoff so that late background polls only suppress messages
        // at or before what was known at mark-all-read time.
        // Messages arriving *after* this cutoff will correctly
        // trigger unread.  Channels with no known ts get "0" so any
        // real message is considered new.
        self.mark_all_read_cutoffs.clear();
        for ch in &self.sidebar.channels {
            let cutoff = self
                .latest_ts
                .get(&ch.id)
                .cloned()
                .unwrap_or_else(|| "0".to_string());
            self.mark_all_read_cutoffs.insert(ch.id.clone(), cutoff);
        }
    }

    fn handle_user_resolved(
        &mut self,
        user_id: String,
        display_name: &str,
        avatar_url: Option<String>,
    ) {
        self.requested_users.remove(&user_id);
        self.messages
            .user_cache
            .insert(user_id.clone(), display_name.to_string());
        self.messages.invalidate_cache();
        self.sidebar.update_dm_name(&user_id, display_name);

        // If the active conversation is a DM with this user, update
        // the title so it shows the resolved name instead of the raw
        // user ID.
        if let Some(ref ch_id) = self.current_channel_id {
            let is_dm_for_user = self
                .sidebar
                .channels
                .iter()
                .any(|c| c.id == *ch_id && c.is_im.unwrap_or(false) && c.user == user_id);
            if is_dm_for_user {
                self.messages.channel_name = display_name.to_string();
            }
        }

        if let Some(url) = avatar_url
            && self.picker.is_some()
        {
            self.download_avatar(user_id, url);
        }
    }

    fn handle_send_message(&mut self) {
        // Reject while a send is in flight to prevent double-sends.
        if self.pending_sends > 0 {
            return;
        }
        let Some(channel_id) = self.current_channel_id.clone() else {
            return;
        };
        let text = self.input.get_text();
        if text.is_empty() {
            return;
        }

        let thread_ts = self.messages.active_thread.clone();
        let upload = match parse_upload_command(&text) {
            Ok(upload) => upload,
            Err(message) => {
                self.status_message = Some(message);
                return;
            }
        };
        self.pending_sends += 1;
        // Clear immediately so keystrokes typed during the round-trip
        // start a fresh draft instead of appending to the sent text.
        // A send failure restores the draft (see handle_error).
        self.in_flight_send_text = Some(text.clone());
        self.input.clear();
        if let Some(upload) = upload {
            self.upload_file(channel_id, upload, thread_ts);
        } else {
            self.send_message(channel_id, text, thread_ts);
        }
    }

    fn handle_download_files(&mut self) {
        // One batch at a time — an accidental double-press would spawn
        // duplicate tasks and save "name (1).ext" copies.
        if self.download_batch.is_some() {
            self.status_message = Some("Download already in progress...".into());
            return;
        }
        let files = self.messages.selected_message_files().to_vec();
        if files.is_empty() {
            self.status_message = Some("Selected message has no files".into());
            return;
        }
        let batch = self.download_batch.get_or_insert_default();
        batch.pending += files.len();
        batch.total += files.len();
        self.status_message = Some(match files.as_slice() {
            [file] => format!("Downloading {}...", file.display_name()),
            files => format!("Downloading {} files...", files.len()),
        });
        let dir = crate::download::download_dir();
        for file in files {
            self.download_file(file, dir.clone());
        }
    }

    fn handle_open_thread(&mut self, thread_ts: &str) {
        let Some(ref channel_id) = self.current_channel_id else {
            return;
        };
        // Skip if we're already fetching this exact thread.
        if self.pending_thread.as_deref() == Some(thread_ts) {
            return;
        }
        // Don't enter thread mode yet — wait for replies to arrive
        // so the UI keeps showing channel messages until the fetch
        // succeeds.
        self.pending_thread = Some(thread_ts.to_string());
        self.loading = true;
        self.load_thread_replies(channel_id, thread_ts);
    }

    fn handle_close_thread(&mut self) {
        self.pending_thread = None;
        self.closing_thread = true;
        self.loading = true;
        self.load_reason = LoadReason::ThreadClose;
        if let Some(ref channel_id) = self.current_channel_id {
            self.load_messages(channel_id);
        }
    }

    fn handle_thread_replies_loaded(
        &mut self,
        channel_id: &str,
        thread_ts: &str,
        messages: Vec<SlackMessage>,
    ) {
        self.thread_poll_in_flight = false;
        if self.current_channel_id.as_deref() != Some(channel_id) {
            return;
        }
        let read_marker_ts = self
            .messages
            .read_marker_ts()
            .map(str::to_string)
            .or_else(|| self.last_seen_ts.get(channel_id).cloned());
        let is_opening_thread = self.pending_thread.as_deref() == Some(thread_ts);
        let is_stable_active_thread = self.pending_thread.is_none()
            && !self.closing_thread
            && self.messages.active_thread.as_deref() == Some(thread_ts);

        // Ignore stale responses if a different thread was requested,
        // a different thread is active, or the user navigated away.
        if !is_opening_thread && !is_stable_active_thread {
            return;
        }
        if is_opening_thread {
            self.pending_thread = None;
            self.messages.set_thread(thread_ts.to_string(), messages);
        } else {
            self.messages.refresh_messages(messages);
        }
        self.messages.set_read_marker_ts(read_marker_ts);
        self.loading = false;
        self.resolve_missing_users();
        self.resolve_file_images();
    }

    fn handle_channels_loaded(&mut self, channels: Vec<Channel>) {
        // Resolve user names for DM channels
        for ch in &channels {
            if ch.is_im.unwrap_or(false)
                && !ch.user.is_empty()
                && !self.messages.user_cache.contains_key(&ch.user)
            {
                self.resolve_user(ch.user.clone());
            }
        }

        self.sidebar.set_channels(channels);
        self.loading = false;
        self.status_message = None;

        // Prune stale cutoff entries for channels no longer in the list.
        self.mark_all_read_cutoffs
            .retain(|id, _| self.sidebar.channels.iter().any(|c| c.id == *id));

        // Try to restore last session, otherwise select first channel
        self.restore_session();
        if self.current_channel_id.is_none()
            && let Some(ch) = self.sidebar.selected_channel()
        {
            let id = ch.id.clone();
            let name = self.sidebar.channel_label(ch);
            self.current_channel_id = Some(id.clone());
            self.messages.channel_name = name;
            self.loading = true;
            self.load_reason = LoadReason::ChannelOpen;
            self.load_messages(&id);
        }

        // Seed unread state by resetting the poll rotation to 0 so
        // the regular poll_if_due cycle hydrates all channels.  We
        // don't fan out all requests at once to avoid self-429ing on
        // Slack's rate limits for workspaces with many channels.
        self.poll_rotation = 0;
        // Force the next tick to fire a poll immediately by backdating
        // last_poll by the poll interval.  Skip when the list came back
        // empty: combined with poll_if_due's empty-list retry, the
        // backdate would turn the retry into a request per tick.
        if !self.sidebar.channels.is_empty()
            && let Some(past) = Instant::now().checked_sub(self.config.poll_interval())
        {
            self.last_poll = past;
        }
    }

    fn handle_messages_loaded(
        &mut self,
        channel_id: &str,
        messages: Vec<SlackMessage>,
        is_background: bool,
    ) {
        // Track the newest message ts for unread detection
        let read_marker_ts = self.last_seen_ts.get(channel_id).cloned();
        let newest_ts = messages.first().map(|m| m.ts.clone());
        // A self-authored newest message from history is ambiguous: it
        // may have been sent by another client after unread teammate
        // messages. Confirmed sends from this app advance read state in
        // `handle_message_sent` using Slack's returned timestamp.
        let newest_is_own = self
            .self_user_id
            .as_deref()
            .is_some_and(|self_id| messages.first().is_some_and(|m| m.sender_id() == self_id));
        if let Some(ref ts) = newest_ts {
            let prev = self.latest_ts.get(channel_id);
            let had_session_baseline = prev.is_some();
            let should_bell_for_newest = had_session_baseline && !newest_is_own;
            let is_new = prev.is_none_or(|p| ts.as_str() > p.as_str());
            if is_new {
                self.latest_ts.insert(channel_id.to_string(), ts.clone());

                if let Some(cutoff) = self.mark_all_read_cutoffs.get(channel_id) {
                    // If MarkAllRead recorded a cutoff for this channel,
                    // only suppress messages at or before that cutoff.
                    // Messages genuinely newer than the cutoff trigger
                    // unread.
                    if ts.as_str() <= cutoff.as_str() {
                        self.last_seen_ts.insert(channel_id.to_string(), ts.clone());
                    } else {
                        // Genuinely new — remove the cutoff and let
                        // normal unread detection run.
                        self.mark_all_read_cutoffs.remove(channel_id);
                        let is_current = self.current_channel_id.as_deref() == Some(channel_id);
                        if !is_current {
                            self.mark_unread_and_maybe_bell(channel_id, should_bell_for_newest);
                        }
                    }
                } else {
                    let is_current = self.current_channel_id.as_deref() == Some(channel_id);
                    let last_seen = self.last_seen_ts.get(channel_id);
                    let unseen = last_seen.is_none_or(|s| ts.as_str() > s.as_str());
                    if !is_current && unseen {
                        self.mark_unread_and_maybe_bell(channel_id, should_bell_for_newest);
                    }
                }
            }
        }

        // Only update the visible message list for the current channel
        if self.current_channel_id.as_deref() != Some(channel_id) {
            return;
        }

        // Background polls only update latest_ts / unread state above.
        // They must never trigger set_channel or mark-read — they fetch
        // only 1 message and would replace the full conversation.
        if is_background {
            return;
        }

        // Only mark as read when the user explicitly opened the
        // channel.  Thread-close reloads and poll refreshes must not
        // advance last_seen_ts — the user may not have seen the
        // newest messages.
        if self.loading
            && self.load_reason == LoadReason::ChannelOpen
            && let Some(ts) = newest_ts
        {
            let already_seen = self.last_seen_ts.get(channel_id).is_some_and(|s| s == &ts);
            if !already_seen {
                self.last_seen_ts.insert(channel_id.to_string(), ts.clone());
                if let Err(e) = self.store.mark_read(channel_id, &ts) {
                    tracing::warn!("Failed to persist read state: {e}");
                }
            }
            if self.sidebar.filter_unread {
                self.deferred_read_channel = Some(channel_id.to_string());
            } else {
                self.sidebar.unread_channels.remove(channel_id);
            }
        }

        // If we were waiting for channel data after closing a thread,
        // now is when we actually exit thread mode.
        if self.closing_thread {
            self.messages.close_thread();
            self.closing_thread = false;
        }

        // Drop stale channel-history responses that arrive while the
        // user is in (or entering) thread mode — they would overwrite
        // the thread replies with channel messages.
        if self.messages.active_thread.is_some() || self.pending_thread.is_some() {
            return;
        }

        // Refresh preserves scroll; only loading=true triggers set_channel
        if self.loading {
            let name = self.messages.channel_name.clone();
            self.messages.set_channel(messages, name);
        } else {
            self.messages.refresh_messages(messages);
        }
        self.messages.set_read_marker_ts(read_marker_ts);
        self.loading = false;

        self.resolve_missing_users();
        self.resolve_file_images();
    }

    fn resolve_missing_users(&mut self) {
        let to_resolve = unresolved_user_ids(&self.messages.messages, &self.messages.user_cache);
        for user_id in to_resolve {
            self.resolve_user(user_id);
        }
    }

    fn resolve_file_images(&mut self) {
        if self.picker.is_none() {
            return;
        }

        let requests = self
            .messages
            .messages
            .iter()
            .flat_map(|message| message.files.iter())
            .filter_map(|file| {
                if !file.is_image() {
                    return None;
                }
                let image_key = file.image_key()?;
                let given_up = self
                    .failed_file_images
                    .get(&image_key)
                    .is_some_and(|attempts| *attempts >= MAX_IMAGE_FETCH_ATTEMPTS);
                if self.messages.file_image_protocols.contains_key(&image_key)
                    || self.requested_file_images.contains(&image_key)
                    || given_up
                {
                    return None;
                }
                let image_url = file.image_url()?.to_string();
                Some((image_key, image_url))
            })
            .collect::<Vec<_>>();

        for (image_key, image_url) in requests {
            self.requested_file_images.insert(image_key.clone());
            self.download_file_image(image_key, image_url);
        }
    }

    fn poll_if_due(&mut self) {
        // Background channels polled per cycle.  One per cycle
        // hydrates unread state for large workspaces far too slowly
        // (N channels -> N cycles); many more risks Slack's
        // conversations.history rate limit (~50/min).
        const BG_POLL_BATCH: usize = 3;

        if self.last_poll.elapsed() < self.config.poll_interval() {
            return;
        }
        self.last_poll = Instant::now();

        // The channel list loads once at startup; if that failed the
        // sidebar would stay empty forever — retry on the poll cadence.
        if self.sidebar.channels.is_empty() {
            if !self.loading {
                self.loading = true;
                self.load_channels();
            }
            return;
        }

        // Refresh current view (errors suppressed — user already has
        // the messages and a transient failure shouldn't disrupt reading).
        if let Some(channel_id) = self.current_channel_id.clone() {
            if let Some(thread_ts) = self.messages.active_thread.clone() {
                // Skip while the previous thread poll is in flight —
                // large threads paginate and can outlive the poll
                // interval, and overlapping fetches multiply the
                // request rate against Slack's limits.
                if !self.thread_poll_in_flight {
                    self.thread_poll_in_flight = true;
                    self.load_thread_replies_quiet(&channel_id, &thread_ts);
                }
            } else {
                self.load_messages_refresh(&channel_id);
            }
        }

        // Also poll a few background channels per cycle.
        let channel_count = self.sidebar.channels.len();
        for _ in 0..BG_POLL_BATCH.min(channel_count) {
            self.poll_rotation %= channel_count;
            let bg_channel_id = self.sidebar.channels[self.poll_rotation].id.clone();
            self.poll_rotation += 1;

            if self.current_channel_id.as_deref() != Some(&bg_channel_id) {
                self.load_messages_background(&bg_channel_id);
            }
        }
    }

    pub fn save_on_quit(&self) {
        if let Some(ref ch_id) = self.current_channel_id {
            if let Err(e) = self
                .store
                .set_session(crate::store::KEY_LAST_CHANNEL, ch_id)
            {
                tracing::warn!("Failed to save session: {e}");
            }
            // Persist only the newest timestamp the user has
            // actually seen. Background polling can advance
            // `latest_ts` even if the foreground history load fails.
            if let Some(ts) = self.last_seen_ts.get(ch_id)
                && let Err(e) = self.store.mark_read(ch_id, ts)
            {
                tracing::warn!("Failed to persist read state: {e}");
            }
        }
    }

    fn restore_session(&mut self) {
        let last_channel = self
            .store
            .get_session(crate::store::KEY_LAST_CHANNEL)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to load saved session: {e}");
                None
            });
        if let Some(ref ch_id) = last_channel
            && let Some(idx) = self.sidebar.channels.iter().position(|c| c.id == *ch_id)
        {
            self.sidebar.selected = idx;
            self.handle_open_channel();
        }
    }

    // --- Async task spawners ---

    fn validate_auth(&self) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client.auth_test().await {
                Ok(info) => {
                    let _ = tx.send(Action::AuthValidated {
                        user_id: info.user_id,
                        user_name: info.user,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::Auth,
                        message: format!("Auth failed: {e}. Check your token."),
                    });
                }
            }
        });
    }

    fn load_channels(&self) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client.list_channels().await {
                Ok(channels) => {
                    let _ = tx.send(Action::ChannelsLoaded(channels));
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::ChannelList,
                        message: format!("Failed to load channels: {e}"),
                    });
                }
            }
        });
    }

    fn load_stars(&self) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client.list_stars().await {
                Ok(starred) => {
                    let _ = tx.send(Action::StarsLoaded(starred));
                }
                Err(e) => {
                    tracing::warn!("Failed to load stars: {e}");
                }
            }
        });
    }

    fn load_messages(&self, channel_id: &str) {
        self.spawn_fetch_messages(channel_id, 50, false, false);
    }

    /// Foreground refresh: updates the message list but swallows errors
    /// so transient poll failures don't flash in the status bar.
    fn load_messages_refresh(&self, channel_id: &str) {
        self.spawn_fetch_messages(channel_id, 50, false, true);
    }

    fn load_messages_background(&self, channel_id: &str) {
        self.spawn_fetch_messages(channel_id, 1, true, true);
    }

    fn spawn_fetch_messages(&self, channel_id: &str, limit: u32, is_background: bool, quiet: bool) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        let channel_id = channel_id.to_string();
        tokio::spawn(async move {
            match client.fetch_history(&channel_id, limit).await {
                Ok(messages) => {
                    let _ = tx.send(Action::MessagesLoaded {
                        channel_id,
                        messages,
                        is_background,
                    });
                }
                Err(e) if quiet => {
                    tracing::debug!("Poll failed for {channel_id}: {e}");
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::ChannelLoad,
                        message: format!("Failed to load messages: {e}"),
                    });
                }
            }
        });
    }

    fn send_message(&self, channel_id: String, text: String, thread_ts: Option<String>) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client
                .post_message(&channel_id, &text, thread_ts.as_deref())
                .await
            {
                Ok(message_ts) => {
                    let _ = tx.send(Action::MessageSent {
                        channel_id,
                        thread_ts,
                        message_ts,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::Send,
                        message: format!("Failed to send: {e}"),
                    });
                }
            }
        });
    }

    fn upload_file(&self, channel_id: String, upload: UploadCommand, thread_ts: Option<String>) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client
                .upload_file(
                    &channel_id,
                    &upload.path,
                    upload.initial_comment.as_deref(),
                    thread_ts.as_deref(),
                )
                .await
            {
                Ok(()) => {
                    let _ = tx.send(Action::MessageSent {
                        channel_id,
                        thread_ts,
                        message_ts: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::Send,
                        message: format_error_chain("Failed to upload file", &e),
                    });
                }
            }
        });
    }

    fn load_thread_replies(&self, channel_id: &str, thread_ts: &str) {
        self.spawn_fetch_thread(channel_id, thread_ts, false);
    }

    fn load_thread_replies_quiet(&self, channel_id: &str, thread_ts: &str) {
        self.spawn_fetch_thread(channel_id, thread_ts, true);
    }

    fn spawn_fetch_thread(&self, channel_id: &str, thread_ts: &str, quiet: bool) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        let channel_id = channel_id.to_string();
        let thread_ts = thread_ts.to_string();
        tokio::spawn(async move {
            match client.fetch_replies(&channel_id, &thread_ts).await {
                Ok(messages) => {
                    let _ = tx.send(Action::ThreadRepliesLoaded {
                        channel_id,
                        thread_ts,
                        messages,
                    });
                }
                Err(e) if quiet => {
                    tracing::debug!("Thread poll failed for {channel_id}: {e}");
                    let _ = tx.send(Action::ThreadPollFailed);
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::ThreadOpen,
                        message: format!("Failed to load thread: {e}"),
                    });
                }
            }
        });
    }

    fn resolve_user(&mut self, user_id: String) {
        // One users.info per id per session: an id that is already in
        // flight, resolved, or failed must not be re-fetched by every
        // 5s poll cycle.
        if !self.requested_users.insert(user_id.clone()) {
            return;
        }
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client.get_user_info(&user_id).await {
                Ok(user) => {
                    let avatar_url = user.avatar_url().map(String::from);
                    let _ = tx.send(Action::UserResolved {
                        user_id,
                        display_name: user.best_name().to_string(),
                        avatar_url,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to resolve user \
                         {user_id}: {e}"
                    );
                }
            }
        });
    }

    fn download_avatar(&self, user_id: String, url: String) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            let data = match client.download_bytes(&url).await {
                Ok(data) => data,
                Err(e) => {
                    tracing::warn!("Failed to download avatar for {user_id}: {e}");
                    return;
                }
            };
            match decode_image(data).await {
                Ok(image) => {
                    let _ = tx.send(Action::AvatarDownloaded {
                        user_id,
                        image: Box::new(image),
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to decode avatar for {user_id}: {e}");
                }
            }
        });
    }

    fn download_file(&self, file: SlackFile, dir: PathBuf) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match crate::download::save_file(&client, &file, &dir).await {
                Ok(dest) => {
                    let _ = tx.send(Action::FileDownloaded { dest });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error {
                        context: ErrorContext::Download,
                        message: format_error_chain("Failed to download file", &e),
                    });
                }
            }
        });
    }

    fn download_file_image(&self, image_key: String, url: String) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            let data = match client.download_bytes(&url).await {
                Ok(data) => data,
                Err(e) => {
                    tracing::warn!("Failed to download file image {image_key}: {e}");
                    let _ = tx.send(Action::FileImageFailed { image_key });
                    return;
                }
            };
            match decode_image(data).await {
                Ok(image) => {
                    let _ = tx.send(Action::FileImageDownloaded {
                        image_key,
                        image: Box::new(image),
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to decode file image {image_key}: {e}");
                    let _ = tx.send(Action::FileImageFailed { image_key });
                }
            }
        });
    }

    fn handle_avatar_downloaded(&mut self, user_id: &str, image: image::DynamicImage) {
        let Some(ref mut picker) = self.picker else {
            return;
        };
        let protocol = picker.new_resize_protocol(image);
        self.messages
            .avatar_protocols
            .insert(user_id.to_string(), protocol);
    }

    fn handle_file_image_downloaded(&mut self, image_key: &str, image: image::DynamicImage) {
        let Some(ref mut picker) = self.picker else {
            return;
        };
        let protocol = picker.new_resize_protocol(image);
        self.failed_file_images.remove(image_key);
        let evicted = self
            .messages
            .insert_file_image(image_key.to_string(), protocol);
        for key in evicted {
            self.requested_file_images.remove(&key);
        }
    }

    fn load_custom_emoji(&self) {
        let tx = self.action_tx.clone();
        let client = self.slack.clone();
        tokio::spawn(async move {
            match client.list_emoji().await {
                Ok(emoji_map) => {
                    let _ = tx.send(Action::CustomEmojiLoaded(emoji_map));
                }
                Err(e) => {
                    tracing::warn!("Failed to load custom emoji: {e}");
                }
            }
        });
    }

    fn handle_custom_emoji_loaded(&mut self, emoji_map: &HashMap<String, String>) {
        // Resolve aliases and store only real image URLs
        let resolved: HashMap<String, String> = emoji_map
            .iter()
            .map(|(name, value)| {
                let url = if let Some(alias) = value.strip_prefix("alias:") {
                    emoji_map
                        .get(alias)
                        .cloned()
                        .unwrap_or_else(|| value.clone())
                } else {
                    value.clone()
                };
                (name.clone(), url)
            })
            .filter(|(_, url)| !url.starts_with("alias:"))
            .collect();

        tracing::info!("Loaded {} custom emoji", resolved.len());
        self.custom_emoji_urls = resolved;

        // Share the URL map with the message renderer
        self.messages.custom_emoji_urls = self.custom_emoji_urls.clone();
    }
}

/// Decode on the blocking pool — a large image takes hundreds of ms,
/// which would stall an async runtime worker (or the UI task, if
/// decoded in `update()`).
async fn decode_image(data: Vec<u8>) -> Result<image::DynamicImage> {
    tokio::task::spawn_blocking(move || image::load_from_memory(&data))
        .await
        .wrap_err("Image decode task failed")?
        .wrap_err("Failed to decode image")
}

fn unresolved_user_ids(
    messages: &[SlackMessage],
    user_cache: &HashMap<String, String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unresolved = Vec::new();

    for message in messages {
        push_unresolved_user_id(message.sender_id(), user_cache, &mut seen, &mut unresolved);

        for user_id in mentioned_user_ids(&message.text) {
            push_unresolved_user_id(user_id, user_cache, &mut seen, &mut unresolved);
        }
    }

    unresolved
}

fn push_unresolved_user_id(
    user_id: &str,
    user_cache: &HashMap<String, String>,
    seen: &mut HashSet<String>,
    unresolved: &mut Vec<String>,
) {
    if user_id.is_empty()
        || !is_slack_user_id(user_id)
        || user_cache.contains_key(user_id)
        || !seen.insert(user_id.to_string())
    {
        return;
    }

    unresolved.push(user_id.to_string());
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::slack::types::{Channel, SlackMessage};

    fn test_app() -> App {
        let config = Config::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let store = Store::open_in_memory().expect("test store");
        App::new(config, tx, None, store).expect("test app")
    }

    #[test]
    fn quit_sets_should_quit() {
        let mut app = test_app();
        app.update(Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn enter_insert_mode_changes_mode() {
        let mut app = test_app();
        assert_eq!(app.mode, Mode::Normal);
        app.update(Action::EnterInsertMode);
        assert_eq!(app.mode, Mode::Insert);
    }

    #[test]
    fn enter_normal_mode_from_insert() {
        let mut app = test_app();
        app.update(Action::EnterInsertMode);
        app.update(Action::EnterNormalMode);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn focus_transitions() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Sidebar);

        app.update(Action::FocusMessages);
        assert_eq!(app.focus, Focus::Messages);

        app.update(Action::FocusSidebar);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[tokio::test]
    async fn channels_loaded_populates_sidebar() {
        let mut app = test_app();
        app.update(Action::ChannelsLoaded(two_channels()));

        assert_eq!(app.sidebar.channels.len(), 2);
        assert!(app.status_message.is_none());
        // Auto-selects first channel and triggers message load
        assert_eq!(app.current_channel_id.as_deref(), Some("C1"));
        assert!(app.loading); // loading messages for auto-selected channel
    }

    #[tokio::test]
    async fn select_channel_sets_state() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());

        app.update(Action::OpenChannel);
        assert_eq!(app.current_channel_id.as_deref(), Some("C1"));
        assert!(app.loading);
        assert_eq!(app.focus, Focus::Messages);
    }

    #[tokio::test]
    async fn messages_loaded_populates_list() {
        let mut app = test_app();
        app.current_channel_id = Some("C1".into());
        app.messages.channel_name = "general".into();

        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("1234567890.000000")],
            is_background: false,
        });

        assert_eq!(app.messages.messages.len(), 1);
        assert!(!app.loading);
    }

    #[test]
    fn messages_loaded_ignored_for_wrong_channel() {
        let mut app = test_app();
        app.current_channel_id = Some("C1".into());

        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![msg("1.0")],
            is_background: false,
        });

        assert!(app.messages.messages.is_empty());
    }

    #[test]
    fn user_resolved_updates_cache() {
        let mut app = test_app();

        app.update(Action::UserResolved {
            user_id: "U1".into(),
            display_name: "Alice".into(),
            avatar_url: None,
        });

        assert_eq!(app.messages.user_cache.get("U1"), Some(&"Alice".into()));
    }

    #[test]
    fn error_sets_status_message() {
        let mut app = test_app();
        app.loading = true;

        app.update(Action::Error {
            context: ErrorContext::ChannelLoad,
            message: "oops".into(),
        });

        assert_eq!(app.status_message.as_deref(), Some("oops"));
        assert!(!app.loading);
    }

    #[test]
    fn unrelated_error_does_not_reset_send_or_thread_state() {
        let mut app = test_app();
        app.pending_sends = 1;
        app.pending_thread = Some("100.0".into());

        // A download failure must not clear the double-send guard or
        // cancel the in-flight thread open.
        app.update(Action::Error {
            context: ErrorContext::Download,
            message: "download blew up".into(),
        });

        assert_eq!(app.pending_sends, 1);
        assert_eq!(app.pending_thread.as_deref(), Some("100.0"));
    }

    #[test]
    fn send_error_resets_guard_and_restores_draft() {
        let mut app = test_app();
        app.pending_sends = 1;
        app.in_flight_send_text = Some("hello there".into());

        app.update(Action::Error {
            context: ErrorContext::Send,
            message: "Failed to send: boom".into(),
        });

        assert_eq!(app.pending_sends, 0);
        assert_eq!(app.input.get_text(), "hello there");
    }

    #[test]
    fn send_error_keeps_newer_draft_typed_during_flight() {
        let mut app = test_app();
        app.pending_sends = 1;
        app.in_flight_send_text = Some("old message".into());
        app.input.insert_text("new draft");

        app.update(Action::Error {
            context: ErrorContext::Send,
            message: "Failed to send: boom".into(),
        });

        assert_eq!(app.input.get_text(), "new draft");
        assert!(app.in_flight_send_text.is_none());
    }

    #[test]
    fn thread_open_error_clears_only_thread_state() {
        let mut app = test_app();
        app.pending_sends = 1;
        app.pending_thread = Some("100.0".into());
        app.loading = true;

        app.update(Action::Error {
            context: ErrorContext::ThreadOpen,
            message: "Failed to load thread: boom".into(),
        });

        assert!(app.pending_thread.is_none());
        assert!(!app.loading);
        assert_eq!(app.pending_sends, 1);
    }

    #[test]
    fn confirmed_self_sent_message_advances_read_state_without_unread_or_bell() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        app.self_user_id = Some("U_ME".into());
        // Baseline so a newer ts would normally trigger unread + bell.
        app.latest_ts.insert("C_OTHER".into(), "1.0".into());
        app.mark_self_sent_message_read("C_OTHER", "2.0");

        let mut own = msg("2.0");
        own.user = "U_ME".into();
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![own],
            is_background: true,
        });

        assert!(!app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(!app.take_pending_terminal_bell());
        assert_eq!(
            app.last_seen_ts.get("C_OTHER").map(String::as_str),
            Some("2.0")
        );
        let read_state = app.store.all_read_state().expect("read state");
        assert_eq!(read_state.get("C_OTHER").map(String::as_str), Some("2.0"));
    }

    #[tokio::test]
    async fn unconfirmed_full_history_own_newest_message_does_not_advance_read_state() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        app.self_user_id = Some("U_ME".into());
        app.latest_ts.insert("C_OTHER".into(), "1.0".into());
        app.last_seen_ts.insert("C_OTHER".into(), "1.0".into());
        app.store.mark_read("C_OTHER", "1.0").expect("seed read");

        let mut own = msg("2.0");
        own.user = "U_ME".into();
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![own],
            is_background: false,
        });

        assert!(app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(!app.take_pending_terminal_bell());
        assert_eq!(
            app.last_seen_ts.get("C_OTHER").map(String::as_str),
            Some("1.0")
        );
        let read_state = app.store.all_read_state().expect("read state");
        assert_eq!(read_state.get("C_OTHER").map(String::as_str), Some("1.0"));
    }

    #[tokio::test]
    async fn background_own_newest_message_does_not_advance_read_state() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        app.self_user_id = Some("U_ME".into());
        app.latest_ts.insert("C_OTHER".into(), "1.0".into());
        app.last_seen_ts.insert("C_OTHER".into(), "1.0".into());
        app.store.mark_read("C_OTHER", "1.0").expect("seed read");

        let mut own = msg("3.0");
        own.user = "U_ME".into();
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![own],
            is_background: true,
        });

        assert!(app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(!app.take_pending_terminal_bell());
        assert_eq!(
            app.last_seen_ts.get("C_OTHER").map(String::as_str),
            Some("1.0")
        );
        let read_state = app.store.all_read_state().expect("read state");
        assert_eq!(read_state.get("C_OTHER").map(String::as_str), Some("1.0"));
    }

    #[tokio::test]
    async fn foreign_newest_message_still_marks_unread() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        app.self_user_id = Some("U_ME".into());
        app.latest_ts.insert("C_OTHER".into(), "1.0".into());

        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![msg("2.0")],
            is_background: true,
        });

        assert!(app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(app.take_pending_terminal_bell());
    }

    #[test]
    fn download_batch_masks_nothing_on_mixed_results() {
        let mut app = test_app();
        app.download_batch = Some(DownloadBatch {
            pending: 2,
            total: 2,
            ..DownloadBatch::default()
        });

        app.update(Action::Error {
            context: ErrorContext::Download,
            message: "Failed to download file: nope".into(),
        });
        app.update(Action::FileDownloaded {
            dest: PathBuf::from("/tmp/ok.png"),
        });

        // The later success must not mask the earlier failure.
        assert_eq!(
            app.status_message.as_deref(),
            Some("Failed to download file: nope (1 of 2 failed)")
        );
        assert!(app.download_batch.is_none());
    }

    #[test]
    fn download_files_refused_while_batch_pending() {
        let mut app = test_app();
        app.download_batch = Some(DownloadBatch {
            pending: 1,
            total: 1,
            ..DownloadBatch::default()
        });

        app.update(Action::DownloadFiles);

        assert_eq!(
            app.status_message.as_deref(),
            Some("Download already in progress...")
        );
        let batch = app.download_batch.as_ref().expect("batch still pending");
        assert_eq!((batch.pending, batch.total), (1, 1));
    }

    #[test]
    fn file_image_failure_allows_bounded_retries() {
        let mut app = test_app();
        app.requested_file_images.insert("F1".into());

        for _ in 0..MAX_IMAGE_FETCH_ATTEMPTS {
            app.update(Action::FileImageFailed {
                image_key: "F1".into(),
            });
        }

        // No longer marked in-flight, but permanently given up.
        assert!(!app.requested_file_images.contains("F1"));
        assert_eq!(app.failed_file_images.get("F1"), Some(&3));
    }

    #[test]
    fn resolve_user_requests_each_id_once() {
        let mut app = test_app();
        // First call registers the id; nothing observable to assert on
        // the spawn itself, so assert the guard set.
        app.requested_users.insert("U_DUP".into());

        app.resolve_user("U_DUP".into());

        assert_eq!(app.requested_users.len(), 1);
    }

    #[tokio::test]
    async fn reopening_current_channel_cancels_pending_thread_and_loading() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.update(Action::OpenChannel);
        app.loading = false;
        // Thread open in flight: owns the loading flag.
        app.update(Action::OpenThread("100.0".into()));
        assert!(app.loading);
        assert_eq!(app.pending_thread.as_deref(), Some("100.0"));

        // Re-opening the same channel (click or Enter) cancels the
        // fetch AND releases loading — otherwise the stale flag makes
        // the next poll masquerade as a channel open.
        app.update(Action::OpenChannel);

        assert!(app.pending_thread.is_none());
        assert!(!app.loading);
    }

    #[tokio::test]
    async fn reopening_current_channel_closes_open_thread() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.update(Action::OpenChannel);
        app.loading = false;
        app.messages.set_thread("100.0".into(), vec![msg("100.0")]);

        app.update(Action::OpenChannel);

        // Clicking the channel name means "back to the channel view".
        assert!(app.closing_thread);
        assert!(app.loading);
    }

    #[test]
    fn mouse_routes_by_pane() {
        use ratatui::layout::Rect;

        fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }
        }

        let mut app = test_app();
        app.panes = crate::ui::Panes {
            sidebar: Rect::new(0, 0, 20, 20),
            messages: Rect::new(20, 0, 60, 15),
            input: Rect::new(20, 15, 60, 5),
        };

        // Click on the input pane enters insert mode.
        let action = app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 16));
        assert!(matches!(action, Action::EnterInsertMode));

        // Click on the sidebar (nothing rendered yet, so no channel
        // hit) leaves insert mode and takes focus.
        app.mode = Mode::Insert;
        let action = app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 5));
        assert!(matches!(action, Action::FocusSidebar));
        assert_eq!(app.mode, Mode::Normal);

        // Wheel over the messages pane scrolls history.
        let action = app.handle_mouse(mouse(MouseEventKind::ScrollUp, 30, 5));
        assert!(matches!(action, Action::Tick));
        assert_eq!(app.messages.scroll_offset, 3);

        // Mouse events outside every pane are ignored.
        let action = app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 90, 30));
        assert!(matches!(action, Action::Tick));
    }

    #[test]
    fn upload_command_expands_tilde() {
        let parsed = parse_upload_command("/upload ~/shot.png hi");
        assert!(matches!(parsed, Ok(Some(_))));
        let Ok(Some(parsed)) = parsed else {
            return;
        };

        if let Some(home) = dirs::home_dir() {
            assert_eq!(parsed.path, home.join("shot.png"));
        }
        assert_eq!(parsed.initial_comment.as_deref(), Some("hi"));
    }

    fn two_channels() -> Vec<Channel> {
        vec![
            Channel {
                id: "C1".into(),
                name: Some("general".into()),
                is_channel: Some(true),
                is_im: Some(false),
                is_member: Some(true),
                user: String::new(),
            },
            Channel {
                id: "C2".into(),
                name: Some("random".into()),
                is_channel: Some(true),
                is_im: Some(false),
                is_member: Some(true),
                user: String::new(),
            },
        ]
    }

    fn msg(ts: &str) -> SlackMessage {
        SlackMessage {
            ts: ts.into(),
            user: "U1".into(),
            text: "hello".into(),
            thread_ts: None,
            reply_count: None,
            reactions: Vec::new(),
            files: Vec::new(),
            bot_id: String::new(),
            username: String::new(),
        }
    }

    #[tokio::test]
    async fn unread_filter_defers_removal_until_navigate_away() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.sidebar.unread_channels.insert("C1".into());
        app.sidebar.unread_channels.insert("C2".into());
        app.sidebar.filter_unread = true;

        // Simulate opening C1 (set state as handle_open_channel would)
        app.sidebar.selected = 0;
        app.current_channel_id = Some("C1".into());
        app.messages.channel_name = "general".into();
        app.loading = true;
        app.load_reason = LoadReason::ChannelOpen;

        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.0")],
            is_background: false,
        });

        // C1 should still be visible (deferred) while viewing it
        assert!(app.sidebar.unread_channels.contains("C1"));
        assert_eq!(app.deferred_read_channel.as_deref(), Some("C1"));

        // Navigate to C2
        app.sidebar.selected = 1;
        app.update(Action::OpenChannel);

        // C1 should now be flushed
        assert!(!app.sidebar.unread_channels.contains("C1"));
        // C2 still unread
        assert!(app.sidebar.unread_channels.contains("C2"));
    }

    #[test]
    fn toggle_unread_filter_off_flushes_deferred() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.sidebar.unread_channels.insert("C1".into());
        app.sidebar.filter_unread = true;
        app.current_channel_id = Some("C1".into());
        app.deferred_read_channel = Some("C1".into());

        // Toggle filter OFF
        app.update(Action::ToggleUnreadFilter);

        assert!(!app.sidebar.filter_unread);
        assert!(!app.sidebar.unread_channels.contains("C1"));
        assert!(app.deferred_read_channel.is_none());
    }

    #[test]
    fn toggle_unread_filter_on_snaps_selection() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        // Only C2 (index 1) is unread; selection is on C1 (index 0)
        app.sidebar.unread_channels.insert("C2".into());
        app.sidebar.selected = 0;

        app.update(Action::ToggleUnreadFilter);

        assert!(app.sidebar.filter_unread);
        // Selection should snap to C2 (the only unread channel)
        assert_eq!(app.sidebar.selected, 1);
    }

    #[test]
    fn download_files_without_files_sets_status() {
        let mut app = test_app();
        app.messages.refresh_messages(vec![msg("1.0")]);

        app.update(Action::DownloadFiles);

        assert_eq!(
            app.status_message.as_deref(),
            Some("Selected message has no files")
        );
    }

    #[tokio::test]
    async fn download_files_reports_selected_file_name() {
        let mut app = test_app();
        let mut message = msg("1.0");
        message.files.push(SlackFile {
            id: "F1".into(),
            name: "report.pdf".into(),
            title: String::new(),
            size: 1,
            mimetype: "application/pdf".into(),
            url_private: String::new(),
            url_private_download: String::new(),
            thumb_360: String::new(),
            thumb_480: String::new(),
            thumb_720: String::new(),
            thumb_1024: String::new(),
        });
        app.messages.refresh_messages(vec![message]);

        app.update(Action::DownloadFiles);

        assert_eq!(
            app.status_message.as_deref(),
            Some("Downloading report.pdf...")
        );
    }

    #[test]
    fn file_downloaded_sets_saved_status() {
        let mut app = test_app();

        app.update(Action::FileDownloaded {
            dest: PathBuf::from("/tmp/report.pdf"),
        });

        assert_eq!(app.status_message.as_deref(), Some("Saved /tmp/report.pdf"));
    }

    #[test]
    fn save_on_quit_persists_only_confirmed_seen_ts() {
        let mut app = test_app();
        let channel_id = "C_SAVE_ON_QUIT_SEEN_ONLY".to_string();

        app.current_channel_id = Some(channel_id.clone());
        app.last_seen_ts.insert(channel_id.clone(), "10.0".into());
        app.latest_ts.insert(channel_id.clone(), "20.0".into());

        app.save_on_quit();

        let read_state = app.store.all_read_state().expect("read state");
        assert_eq!(
            read_state.get(&channel_id).map(String::as_str),
            Some("10.0")
        );
    }

    #[test]
    fn unresolved_user_ids_include_senders_and_mentions() {
        let user_cache = HashMap::from([(String::from("U1"), String::from("Alice"))]);
        let messages = vec![
            SlackMessage {
                ts: "1.0".into(),
                user: "U1".into(),
                text: "hi <@U2> and <@U3|carol>".into(),
                thread_ts: None,
                reply_count: None,
                reactions: Vec::new(),
                files: Vec::new(),
                bot_id: String::new(),
                username: String::new(),
            },
            SlackMessage {
                ts: "2.0".into(),
                user: "W2".into(),
                text: "ping <@W3>".into(),
                thread_ts: None,
                reply_count: None,
                reactions: Vec::new(),
                files: Vec::new(),
                bot_id: String::new(),
                username: String::new(),
            },
        ];

        let unresolved = unresolved_user_ids(&messages, &user_cache);

        assert_eq!(
            unresolved,
            vec![
                String::from("U2"),
                String::from("U3"),
                String::from("W2"),
                String::from("W3")
            ]
        );
    }
}
