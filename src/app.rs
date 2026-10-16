use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use color_eyre::eyre::{Report, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use futures::FutureExt;
use futures::future::{BoxFuture, OptionFuture};
use ratatui::layout::Position;
use tokio::sync::mpsc;

use crate::action::{Action, ErrorContext, ReactionTarget};
use crate::activity::{ActivityInbox, MAX_ACTIVITY_ITEMS, ThreadTarget};
use crate::components::activity::ActivityView;
use crate::components::help::HelpPopup;
use crate::components::input::TextInput;
use crate::components::link_picker::LinkPicker;
use crate::components::messages::MessageList;
use crate::components::reaction_picker::ReactionPicker;
use crate::components::reactions::ReactionsPopup;
use crate::components::sidebar::ChannelSidebar;
use crate::components::{Component, EventResult};
use crate::config::Config;
use crate::event::{AppEvent, EventHandler};
use crate::image_pipeline::{ImagePipeline, Target as ImageTarget};
use crate::slack::client::SlackClient;
use crate::slack::types::{Channel, SlackFile, SlackMessage, is_slack_user_id, mentioned_user_ids};
use crate::store::Store;
use crate::terminal_status::ChatNotification;
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

/// How many messages back a channel load pulls from `conversations.history`.
const HISTORY_LIMIT: u32 = 100;

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
    pub activity: ActivityView,
    pub activity_open: bool,
    activity_request: Option<BoxFuture<'static, Action>>,
    thread_read_request: Option<BoxFuture<'static, Action>>,
    thread_read_queue: VecDeque<(ThreadTarget, String)>,
    last_activity_poll: Option<Instant>,
    latest_notified_thread: Option<ThreadTarget>,
    /// Who-reacted overlay for the selected message, when open.
    pub reactions_popup: Option<ReactionsPopup>,
    pub reaction_picker: Option<ReactionPicker>,
    pub link_picker: Option<LinkPicker>,
    pub help_popup: Option<HelpPopup>,
    /// The pending mutation is polled by the UI loop so its result is observed.
    reaction_request: Option<BoxFuture<'static, Action>>,
    browser_request: Option<BoxFuture<'static, Action>>,
    preview_request: Option<BoxFuture<'static, Action>>,
    image_requests: ImagePipeline,

    pub slack: SlackClient,
    pub current_channel_id: Option<String>,

    action_tx: mpsc::UnboundedSender<Action>,
    pub picker: Option<Picker>,
    store: Store,
    last_poll: Instant,
    /// Newest message ts the user has seen per channel.
    last_seen_ts: HashMap<String, String>,
    /// Newest known message ts, or "0" after a successful empty history.
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
    /// Lazily initialized system clipboard. Keeping the handle alive is
    /// required by some clipboard backends after setting its contents.
    clipboard: Option<arboard::Clipboard>,
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
    pending_terminal_notification: Option<ChatNotification>,
    latest_notified_channel: Option<String>,
    terminal_focused: bool,
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
    use crate::app::{UPLOAD_USAGE, parse_upload_command};

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
            activity: ActivityView::default(),
            activity_open: false,
            activity_request: None,
            thread_read_request: None,
            thread_read_queue: VecDeque::new(),
            last_activity_poll: None,
            latest_notified_thread: None,
            reactions_popup: None,
            reaction_picker: None,
            link_picker: None,
            help_popup: None,
            reaction_request: None,
            browser_request: None,
            preview_request: None,
            image_requests: ImagePipeline::default(),
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
            clipboard: None,
            panes: crate::ui::Panes::default(),
            pending_thread: None,
            closing_thread: false,
            pending_terminal_notification: None,
            latest_notified_channel: None,
            // An app may start in an inactive tab. Wait for a focus report
            // or user input before suppressing current-channel alerts.
            terminal_focused: false,
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
        tui.enable_focus_reporting()?;
        let mut events = EventHandler::new(self.config.tick_rate(), action_rx);

        self.validate_auth();
        self.load_channels();
        self.load_stars();
        self.load_custom_emoji();

        loop {
            tui.draw(|frame| ui::render(frame, self))?;

            let event = tokio::select! {
                event = events.next() => event?,
                Some(action) = OptionFuture::from(self.reaction_request.as_mut()) => {
                    self.reaction_request = None;
                    AppEvent::BackgroundAction(action)
                }
                Some(action) = OptionFuture::from(self.activity_request.as_mut()) => {
                    self.activity_request = None;
                    AppEvent::BackgroundAction(action)
                }
                Some(action) = OptionFuture::from(self.thread_read_request.as_mut()) => {
                    self.thread_read_request = None;
                    AppEvent::BackgroundAction(action)
                }
                Some(action) = OptionFuture::from(self.browser_request.as_mut()) => {
                    self.browser_request = None;
                    AppEvent::BackgroundAction(action)
                }
                Some(action) = OptionFuture::from(self.preview_request.as_mut()) => {
                    self.preview_request = None;
                    AppEvent::BackgroundAction(action)
                }
                Some(action) = self.image_requests.next() => AppEvent::BackgroundAction(action),
            };
            if matches!(
                &event,
                AppEvent::Key(_) | AppEvent::Mouse(_) | AppEvent::Paste(_) | AppEvent::Focus(true)
            ) {
                self.terminal_focused = true;
                tui.acknowledge_notification()?;
            }
            let action = match event {
                AppEvent::Tick => Action::Tick,
                AppEvent::Resize => Action::Render,
                AppEvent::Key(key) => self.handle_key(key),
                AppEvent::Mouse(mouse) => self.handle_mouse(mouse),
                AppEvent::Paste(text) => self.handle_paste(&text),
                AppEvent::Focus(focused) => {
                    self.terminal_focused = focused;
                    Action::Render
                }
                AppEvent::BackgroundAction(action) => action,
            };

            self.update(action);
            if let Some(notification) = self.pending_terminal_notification.take() {
                tui.notify_chat(&notification)?;
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if let Some(help) = self.help_popup.as_mut() {
            return help.handle_key(key);
        }
        if let Some(picker) = self.link_picker.as_mut() {
            return picker.handle_key(key);
        }
        if let Some(picker) = self.reaction_picker.as_mut() {
            return picker.handle_key(key);
        }
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Insert => self.handle_insert_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // The reactions popup is modal: keys it doesn't own are
        // swallowed so Esc/q can't fall through to quit or
        // close-thread underneath it.  Only the global quit chord
        // pierces the overlay.
        if let Some(popup) = self.reactions_popup.as_mut() {
            return match popup.handle_key(key) {
                EventResult::Action(action) => action,
                EventResult::Consumed => Action::Tick,
                EventResult::Ignored => match (key.code, ctrl) {
                    (KeyCode::Char('c'), true) => Action::Quit,
                    _ => Action::Tick,
                },
            };
        }

        match (key.code, ctrl) {
            (KeyCode::Char('?'), false) => Action::OpenHelp,
            // Quit
            (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => Action::Quit,
            // Insert mode
            (KeyCode::Char('i'), false) if !self.activity_open => Action::EnterInsertMode,
            (KeyCode::Char('a'), false) => Action::OpenActivity,
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
                if self.focus == Focus::Messages
                    && !self.activity_open
                    && self.messages.active_thread.is_some() =>
            {
                Action::CloseThread
            }
            (KeyCode::Left, _) if self.focus == Focus::Messages => Action::FocusSidebar,
            // Number shortcuts: direct pane access
            (KeyCode::Char('1'), false) => Action::FocusSidebar,
            (KeyCode::Char('2'), false) => Action::FocusMessages,
            (KeyCode::Char('N'), false) => Action::OpenLatestNotification,
            _ => {
                let result = match self.focus {
                    Focus::Sidebar => self.sidebar.handle_key(key),
                    Focus::Messages if self.activity_open => return self.activity.handle_key(key),
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
        if self.help_popup.is_some() || self.link_picker.is_some() {
            return Action::Render;
        }
        if let Some(picker) = self.reaction_picker.as_mut() {
            picker.insert_text(text);
            return Action::Render;
        }
        if self.mode == Mode::Insert {
            self.input.insert_text(text);
        }
        Action::Tick
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Action {
        if let Some(help) = self.help_popup.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => help.scroll(true, 3),
                MouseEventKind::ScrollDown => help.scroll(false, 3),
                _ => {}
            }
            return Action::Render;
        }
        if let Some(picker) = self.link_picker.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => picker.move_selection(true),
                MouseEventKind::ScrollDown => picker.move_selection(false),
                _ => {}
            }
            return Action::Render;
        }
        if let Some(picker) = self.reaction_picker.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => picker.move_selection(true),
                MouseEventKind::ScrollDown => picker.move_selection(false),
                _ => {}
            }
            return Action::Render;
        }
        if let Some(popup) = self.reactions_popup.as_mut() {
            return match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => Action::ToggleReactions,
                MouseEventKind::ScrollUp => {
                    popup.scroll_up();
                    Action::Tick
                }
                MouseEventKind::ScrollDown => {
                    popup.scroll_down();
                    Action::Tick
                }
                MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
                | MouseEventKind::Up(_)
                | MouseEventKind::Drag(_)
                | MouseEventKind::Moved
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight => Action::Tick,
            };
        }
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_left_click(position),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if self.panes.messages.contains(position) {
                    if self.activity_open {
                        self.activity.move_selection(up);
                    } else {
                        self.messages.handle_scroll(up);
                    }
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
        if self.panes.activity.contains(position) {
            return Action::OpenActivity;
        }
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
            if self.activity_open {
                self.focus = Focus::Messages;
                return self.activity.handle_click(position);
            }
            self.messages.handle_click(position);
            return Action::FocusMessages;
        }
        if self.panes.input.contains(position) {
            return Action::EnterInsertMode;
        }
        Action::Tick
    }

    fn channel_has_unseen_messages(&self, channel_id: &str) -> bool {
        self.latest_ts.get(channel_id).is_some_and(|latest| {
            latest != "0"
                && self
                    .last_seen_ts
                    .get(channel_id)
                    .is_none_or(|seen| latest > seen)
        })
    }

    fn update_channel_unread(&mut self, channel_id: &str) {
        // Keep the open channel in its existing section until navigation
        // away, so reading does not reshuffle the sidebar mid-session.
        if self.current_channel_id.as_deref() == Some(channel_id) {
            return;
        }
        if self.channel_has_unseen_messages(channel_id) {
            self.sidebar.unread_channels.insert(channel_id.to_string());
        } else {
            self.sidebar.unread_channels.remove(channel_id);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "central action dispatch keeps state transitions explicit"
    )]
    pub fn update(&mut self, action: Action) {
        match action {
            Action::OpenHelp => self.help_popup = Some(HelpPopup::default()),
            Action::CloseHelp => self.help_popup = None,
            Action::Quit => self.should_quit = true,
            Action::EnterNormalMode => self.mode = Mode::Normal,
            Action::EnterInsertMode if !self.activity_open => self.mode = Mode::Insert,
            Action::FocusSidebar => self.focus = Focus::Sidebar,
            Action::FocusMessages => self.focus = Focus::Messages,
            Action::OpenChannel => self.handle_open_channel(),
            Action::OpenLatestNotification => self.handle_open_latest_notification(),
            action @ (Action::OpenActivity
            | Action::RefreshActivity
            | Action::ReadActivity
            | Action::OpenActivityThread(_)
            | Action::ActivityLoaded(_)
            | Action::ThreadReadFinished { .. }
            | Action::ThreadOpenFailed { .. }) => self.handle_activity_action(action),
            Action::MarkAllRead => self.handle_mark_all_read(),
            Action::ToggleUnreadFilter => {
                self.sidebar.filter_unread = !self.sidebar.filter_unread;
                if self.sidebar.filter_unread {
                    self.sidebar.snap_selection_to_visible();
                }
            }
            Action::OpenThread(ref thread_ts) => self.handle_open_thread(thread_ts),
            Action::CloseThread => self.handle_close_thread(),
            Action::SendMessage => self.handle_send_message(),
            Action::DownloadFiles => self.handle_download_files(),
            Action::PreviewMedia => self.handle_preview_media(),
            Action::PreviewFinished(result) => {
                self.status_message = Some(match result {
                    Ok(summary) if summary.unavailable > 0 => format!(
                        "Previewed {} item(s); {} unavailable",
                        summary.files, summary.unavailable
                    ),
                    Ok(_) => "Media preview closed".into(),
                    Err(error) => format!("Media preview failed: {error}"),
                });
            }
            Action::CopyMessage => self.handle_copy_message(),
            Action::OpenMessageLink => self.handle_open_message_link(),
            Action::CloseLinkPicker => self.link_picker = None,
            Action::OpenUrl(url) => self.open_url(url),
            Action::BrowserFinished(result) => {
                self.status_message = Some(match result {
                    Ok(()) => "Opened link in browser".into(),
                    Err(error) => error,
                });
            }
            Action::ToggleReactions => self.handle_toggle_reactions(),
            Action::OpenReactionPicker => self.handle_open_reaction_picker(),
            Action::CloseReactionPicker => self.reaction_picker = None,
            Action::SubmitReaction => self.handle_submit_reaction(),
            Action::ReactionAdded { target, name } => {
                self.handle_reaction_added(&target, &name);
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
                self.handle_auth_validated(user_id, &user_name);
            }
            Action::Error { context, message } => {
                self.handle_error(context, message);
            }
            Action::Tick => {
                self.poll_if_due();
            }
            Action::EnterInsertMode | Action::Render => {}
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
            ErrorContext::ChannelLoad => {
                self.status_message = Some(message);
                self.closing_thread = false;
                self.loading = false;
            }
            ErrorContext::ChannelList => {
                self.status_message = Some(message);
                self.loading = false;
            }
            ErrorContext::Auth | ErrorContext::Reaction => {
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

    fn handle_copy_message(&mut self) {
        let Some(text) = self.messages.selected_message_text() else {
            self.status_message = Some("No message selected".into());
            return;
        };
        if text.is_empty() {
            self.status_message = Some("Selected message has no text".into());
            return;
        }

        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => {
                    self.status_message = Some(format!("Failed to access clipboard: {error}"));
                    return;
                }
            }
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            return;
        };

        self.status_message = Some(
            match clipboard.set_text(crate::message_links::copy_text(text)) {
                Ok(()) => "Copied message".into(),
                Err(error) => format!("Failed to copy message: {error}"),
            },
        );
    }

    fn handle_open_message_link(&mut self) {
        if self.browser_request.is_some() {
            self.status_message = Some("Opening link...".into());
            return;
        }
        let Some(text) = self.messages.selected_message_text() else {
            self.status_message = Some("No message selected".into());
            return;
        };
        let mut links = crate::message_links::links(text);
        match links.len() {
            0 => self.status_message = Some("Selected message has no web links".into()),
            1 => {
                if let Some(url) = links.pop() {
                    self.open_url(url);
                }
            }
            _ => {
                self.link_picker = Some(LinkPicker::new(links));
                self.status_message = None;
            }
        }
    }

    fn handle_preview_media(&mut self) {
        if self.preview_request.is_some() {
            self.status_message =
                Some("Close Quick Look or quit the preview's QuickTime instance first".into());
            return;
        }
        let Some(text) = self.messages.selected_message_text() else {
            self.status_message = Some("No message selected".into());
            return;
        };
        let sources = crate::media_preview::sources(self.messages.selected_message_files(), text);
        if sources.is_empty() {
            self.status_message =
                Some("Selected message has no photos, videos, or web links".into());
            return;
        }
        let client = self.slack.clone();
        self.status_message = Some("Opening media preview...".into());
        self.preview_request =
            Some(
                async move {
                    Action::PreviewFinished(crate::media_preview::open(client, sources).await)
                }
                .boxed(),
            );
    }

    fn open_url(&mut self, url: reqwest::Url) {
        if self.browser_request.is_some() {
            return;
        }
        self.link_picker = None;
        self.status_message = Some("Opening link...".into());
        self.browser_request =
            Some(async move { Action::BrowserFinished(crate::browser::open(url).await) }.boxed());
    }

    fn handle_open_reaction_picker(&mut self) {
        if self.reaction_picker.is_some() {
            return;
        }
        if self.reaction_request.is_some() {
            self.status_message = Some("Adding reaction...".into());
            return;
        }
        let (Some(channel_id), Some(message_ts)) = (
            self.current_channel_id.clone(),
            self.messages.selected_message_ts(),
        ) else {
            self.status_message = Some("No message selected".into());
            return;
        };
        let target = ReactionTarget {
            channel_id,
            message_ts: message_ts.to_string(),
        };
        let names = self.custom_emoji_urls.keys().map(String::as_str).chain(
            self.messages
                .selected_message_reactions()
                .iter()
                .map(|reaction| reaction.name.as_str()),
        );
        self.reaction_picker = Some(ReactionPicker::new(target, names));
        self.reactions_popup = None;
        self.status_message = None;
    }

    fn handle_submit_reaction(&mut self) {
        if self.reaction_request.is_some() {
            return;
        }
        let Some(picker) = self.reaction_picker.as_ref() else {
            return;
        };
        let Some(name) = picker.selected_name().map(str::to_string) else {
            self.status_message = Some("Enter a Slack emoji name".into());
            return;
        };
        let target = picker.target.clone();
        let client = self.slack.clone();
        self.status_message = Some(format!("Adding :{name}:..."));
        self.reaction_picker = None;
        self.reaction_request = Some(
            async move {
                match client
                    .add_reaction(&target.channel_id, &target.message_ts, &name)
                    .await
                {
                    Ok(()) => Action::ReactionAdded { target, name },
                    Err(error) => Action::Error {
                        context: ErrorContext::Reaction,
                        message: format!("Failed to add :{name}: {error}"),
                    },
                }
            }
            .boxed(),
        );
    }

    fn handle_reaction_added(&mut self, target: &ReactionTarget, name: &str) {
        self.status_message = Some(format!("Added :{name}:"));
        if self.current_channel_id.as_deref() != Some(target.channel_id.as_str()) || self.loading {
            return;
        }
        if let Some(thread_ts) = self.messages.active_thread.as_deref() {
            self.load_thread_replies_quiet(&target.channel_id, thread_ts);
        } else {
            self.load_messages_refresh(&target.channel_id);
        }
    }

    fn handle_toggle_reactions(&mut self) {
        if self.reactions_popup.take().is_some() {
            return;
        }
        let reactions = self.messages.selected_message_reactions().to_vec();
        if reactions.is_empty() {
            self.status_message = Some("Selected message has no reactions".into());
            return;
        }

        let popup = ReactionsPopup::new(reactions);
        // Reactors may never have posted in the loaded history, so
        // their names can be missing from the cache — resolve them now;
        // the popup picks names up live as responses arrive.
        let mut seen = HashSet::new();
        let mut unresolved = Vec::new();
        for user_id in popup.user_ids() {
            push_unresolved_user_id(
                user_id,
                &self.messages.user_cache,
                &mut seen,
                &mut unresolved,
            );
        }
        for user_id in unresolved {
            self.resolve_user(user_id);
        }
        self.reactions_popup = Some(popup);
    }

    fn handle_file_image_failed(&mut self, image_key: &str) {
        self.requested_file_images.remove(image_key);
        *self
            .failed_file_images
            .entry(image_key.to_string())
            .or_insert(0) += 1;
    }

    fn mark_channel_read(&mut self, channel_id: &str, ts: &str) {
        if self
            .last_seen_ts
            .get(channel_id)
            .is_some_and(|seen| seen.as_str() >= ts)
        {
            return;
        }
        self.last_seen_ts
            .insert(channel_id.to_string(), ts.to_string());
        if let Err(e) = self.store.mark_read(channel_id, ts) {
            tracing::warn!("Failed to persist read state: {e}");
        }
        self.update_channel_unread(channel_id);
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
            self.mark_channel_read(channel_id, ts);
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

    fn handle_open_channel(&mut self) {
        let Some(channel) = self.sidebar.selected_channel() else {
            return;
        };
        let channel_id = channel.id.clone();
        let channel_name = self.sidebar.channel_label(channel).to_string();
        self.focus = Focus::Messages;
        self.activity_open = false;
        // Cancel any in-flight thread fetch regardless of whether the
        // channel changed — re-opening the same channel while a thread
        // is loading should not drop into thread mode later.
        let cancelled_thread_open = self.pending_thread.take().is_some();
        if self.current_channel_id.as_deref() != Some(&channel_id) {
            if let Some(previous) = self.current_channel_id.replace(channel_id.clone()) {
                self.update_channel_unread(&previous);
            }
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

    fn handle_open_latest_notification(&mut self) {
        if let Some(target) = self.latest_notified_thread.clone() {
            self.open_activity_thread(&target);
            return;
        }
        let Some(channel_id) = self.latest_notified_channel.as_deref() else {
            self.status_message = Some("No new-chat notification yet".into());
            return;
        };
        if self.sidebar.select_channel(channel_id) {
            self.mode = Mode::Normal;
            self.handle_open_channel();
        } else {
            self.status_message = Some("Notified conversation is no longer available".into());
        }
    }

    fn handle_auth_validated(&mut self, user_id: String, user_name: &str) {
        tracing::info!("Authenticated as {user_name}");
        match self
            .store
            .get_session(&format!("thread_activity/{user_id}"))
        {
            Ok(Some(saved)) if saved.len() <= 2 * 1024 * 1024 => {
                if let Ok(mut inbox) = serde_json::from_str::<ActivityInbox>(&saved) {
                    inbox.entries.truncate(MAX_ACTIVITY_ITEMS);
                    self.activity.inbox = inbox;
                }
            }
            Err(error) => tracing::warn!("Failed to restore Activity: {error}"),
            _ => {}
        }
        self.self_user_id = Some(user_id);
        self.refresh_activity();
    }

    fn handle_activity_action(&mut self, action: Action) {
        match action {
            Action::OpenActivity => {
                self.activity_open = true;
                self.mode = Mode::Normal;
                self.focus = Focus::Messages;
                self.reactions_popup = None;
                self.reaction_picker = None;
                self.link_picker = None;
                if self.last_activity_poll.is_none() {
                    self.refresh_activity();
                }
            }
            Action::RefreshActivity => self.refresh_activity(),
            Action::ReadActivity => {
                if let Some(entry) = self.activity.selected_entry() {
                    let target = entry.target.clone();
                    let ts = entry.latest_ts.clone();
                    self.mark_activity_read(target, ts, true);
                }
            }
            Action::OpenActivityThread(target) => self.open_activity_thread(&target),
            Action::ActivityLoaded(result) => self.activity_loaded(result),
            Action::ThreadReadFinished { target, ts, result } => {
                match result {
                    Ok(()) => {
                        self.activity.inbox.confirm_read(&target, &ts);
                        self.persist_activity();
                    }
                    Err(error) => {
                        self.status_message =
                            Some(format!("Thread read locally; Slack sync failed: {error}"));
                    }
                }
                self.start_thread_read();
            }
            Action::ThreadOpenFailed { target, message }
                if self.current_channel_id.as_deref() == Some(&target.channel_id)
                    && self.pending_thread.as_deref() == Some(&target.thread_ts) =>
            {
                self.status_message = Some(message);
                self.pending_thread = None;
                self.loading = false;
            }
            _ => {}
        }
    }

    fn refresh_activity(&mut self) {
        if self.activity_request.is_some() {
            return;
        }
        let Some(self_id) = self.self_user_id.clone() else {
            return;
        };
        let client = self.slack.clone();
        self.last_activity_poll = Some(Instant::now());
        self.activity.refreshing = true;
        let read_revision = self.activity.inbox.read_revision();
        self.activity_request = Some(
            async move {
                let result = client
                    .fetch_thread_activity(&self_id)
                    .await
                    .map(|mut snapshot| {
                        snapshot.read_revision = read_revision;
                        snapshot
                    })
                    .map_err(|error| format_error_chain("Could not refresh replies", &error));
                Action::ActivityLoaded(result)
            }
            .boxed(),
        );
    }

    fn activity_loaded(&mut self, result: Result<crate::activity::ActivitySnapshot, String>) {
        self.activity.refreshing = false;
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.activity.error = Some(crate::activity::preview(&error));
                return;
            }
        };
        let alerts = self.activity.merge(snapshot);
        self.persist_activity();
        // One terminal alert per sweep; the badge retains every unread thread.
        if let Some(target) = alerts.into_iter().find(|target| {
            !(self.terminal_focused
                && !self.activity_open
                && self.messages.scroll_offset == 0
                && self.current_channel_id.as_deref() == Some(&target.channel_id)
                && self.messages.active_thread.as_deref() == Some(&target.thread_ts))
        }) {
            let channel = self
                .sidebar
                .channels
                .iter()
                .find(|channel| channel.id == target.channel_id);
            let label = channel.map_or(target.channel_id.as_str(), |channel| {
                self.sidebar.channel_label(channel)
            });
            self.pending_terminal_notification = Some(ChatNotification::thread_reply(
                &target.channel_id,
                label,
                channel.is_some_and(|channel| channel.is_im == Some(true)),
            ));
            self.latest_notified_channel = Some(target.channel_id.clone());
            self.latest_notified_thread = Some(target);
        }
        let users: Vec<_> = self
            .activity
            .inbox
            .entries
            .iter()
            .flat_map(|entry| [&entry.root_user, &entry.reply_user])
            .filter(|id| {
                is_slack_user_id(id)
                    && !self.messages.user_cache.contains_key(*id)
                    && !self.requested_users.contains(*id)
            })
            .take(8)
            .cloned()
            .collect();
        for user in users {
            self.resolve_user(user);
        }
    }

    fn persist_activity(&self) {
        let Some(user_id) = &self.self_user_id else {
            return;
        };
        let key = format!("thread_activity/{user_id}");
        let result = serde_json::to_string(&self.activity.inbox)
            .map_err(Report::from)
            .and_then(|saved| self.store.set_session(&key, &saved));
        if let Err(error) = result {
            tracing::warn!("Failed to save Activity: {error}");
        }
    }

    fn open_activity_thread(&mut self, target: &ThreadTarget) {
        self.activity_open = false;
        self.mode = Mode::Normal;
        self.focus = Focus::Messages;
        self.status_message = None;
        self.sidebar.select_channel(&target.channel_id);
        if self.current_channel_id.as_deref() != Some(&target.channel_id) {
            let channel = self
                .sidebar
                .channels
                .iter()
                .find(|channel| channel.id == target.channel_id);
            let name = channel
                .map_or(target.channel_id.as_str(), |channel| {
                    self.sidebar.channel_label(channel)
                })
                .to_string();
            if let Some(previous) = self.current_channel_id.replace(target.channel_id.clone()) {
                self.update_channel_unread(&previous);
            }
            self.messages.set_channel(Vec::new(), name);
        }
        self.pending_thread = None;
        self.closing_thread = false;
        self.load_reason = LoadReason::ThreadClose;
        self.handle_open_thread(&target.thread_ts);
    }

    fn mark_activity_read(&mut self, target: ThreadTarget, ts: String, force: bool) {
        if self
            .activity
            .inbox
            .entries
            .iter()
            .any(|entry| entry.target == target && ts <= entry.server_read_ts)
        {
            return;
        }
        if !self.activity.inbox.mark_read(&target, &ts) && !force {
            return;
        }
        self.persist_activity();
        if let Some((_, queued_ts)) = self
            .thread_read_queue
            .iter_mut()
            .find(|(queued, _)| *queued == target)
        {
            if ts > *queued_ts {
                *queued_ts = ts;
            }
        } else if self.thread_read_queue.len() < MAX_ACTIVITY_ITEMS {
            self.thread_read_queue.push_back((target, ts));
        }
        self.start_thread_read();
    }

    fn start_thread_read(&mut self) {
        if self.thread_read_request.is_some() {
            return;
        }
        let Some((target, ts)) = self.thread_read_queue.pop_front() else {
            return;
        };
        let client = self.slack.clone();
        self.thread_read_request = Some(
            async move {
                let result = client
                    .mark_thread_read(&target, &ts)
                    .await
                    .map_err(|error| crate::activity::preview(&error.to_string()));
                Action::ThreadReadFinished { target, ts, result }
            }
            .boxed(),
        );
    }

    fn handle_mark_all_read(&mut self) {
        self.sidebar.mark_all_read();
        // Persist for channels with known timestamps — one transaction
        // so the event loop isn't stalled by a WAL sync per channel.
        for (ch_id, ts) in &self.latest_ts {
            let seen = self
                .last_seen_ts
                .entry(ch_id.clone())
                .or_insert_with(|| ts.clone());
            if ts > seen {
                seen.clone_from(ts);
            }
        }
        let entries = self.latest_ts.keys().filter_map(|id| {
            self.last_seen_ts
                .get(id)
                .map(|ts| (id.as_str(), ts.as_str()))
        });
        if let Err(e) = self.store.mark_read_many(entries) {
            tracing::warn!("Failed to persist read state: {e}");
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
        // Rebuild the visual lines only when this user is actually on
        // screen — resolutions for reaction-popup users who never
        // posted would otherwise force a full re-render each.
        let referenced = self.messages.messages.iter().any(|message| {
            message.sender_id() == user_id
                || mentioned_user_ids(&message.text).contains(&user_id.as_str())
        });
        if referenced {
            self.messages.invalidate_cache();
        }
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
            if self.image_requests.remaining_capacity() == 0 {
                // The regular visible-user resolution pass will retry after
                // backpressure clears. Keep the sidebar's resolved DM label.
                self.messages.user_cache.remove(&user_id);
                tracing::debug!("Avatar image deferred because the image queue is full");
            } else if !self
                .image_requests
                .enqueue(&self.slack, ImageTarget::Avatar(user_id), &url)
            {
                tracing::debug!("Avatar image URL is invalid");
            }
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
            .activity
            .inbox
            .entries
            .iter()
            .find(|entry| {
                entry.target.channel_id == channel_id && entry.target.thread_ts == thread_ts
            })
            .map(|entry| std::cmp::max(&entry.local_read_ts, &entry.server_read_ts).clone())
            .filter(|ts| !ts.is_empty());
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
        if !self.activity_open
            && (is_opening_thread || (self.terminal_focused && self.messages.scroll_offset == 0))
            && let Some(ts) = self
                .messages
                .messages
                .iter()
                .map(|message| &message.ts)
                .max()
                .cloned()
        {
            self.mark_activity_read(
                ThreadTarget {
                    channel_id: channel_id.to_string(),
                    thread_ts: thread_ts.to_string(),
                },
                ts,
                false,
            );
        }
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

        // Try to restore last session, otherwise select first channel
        self.restore_session();
        if self.current_channel_id.is_none()
            && let Some(ch) = self.sidebar.selected_channel()
        {
            let id = ch.id.clone();
            let name = self.sidebar.channel_label(ch).to_string();
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
        // A successful empty response also establishes a baseline, so the
        // first real message in an empty channel can trigger an alert.
        let observed_ts = newest_ts.as_deref().unwrap_or("0");
        let prev = self.latest_ts.get(channel_id);
        let had_session_baseline = prev.is_some();
        let is_new = prev.is_none_or(|p| observed_ts > p.as_str());
        if is_new {
            self.latest_ts
                .insert(channel_id.to_string(), observed_ts.to_string());
        }
        let viewing_channel = self.terminal_focused
            && !self.activity_open
            && self.current_channel_id.as_deref() == Some(channel_id)
            && self.messages.active_thread.is_none();
        if is_new
            && had_session_baseline
            && !newest_is_own
            && !viewing_channel
            && self.channel_has_unseen_messages(channel_id)
        {
            let channel = self.sidebar.channels.iter().find(|ch| ch.id == channel_id);
            let label = channel.map_or(channel_id, |ch| self.sidebar.channel_label(ch));
            self.pending_terminal_notification = Some(ChatNotification::new(
                channel_id,
                label,
                channel.is_some_and(|ch| ch.is_im == Some(true)),
            ));
            // Slack conversation IDs are short. Keep this extra navigation
            // target bounded even if an unexpected API response is oversized.
            self.latest_notified_thread = None;
            self.latest_notified_channel =
                (channel_id.len() <= 128).then(|| channel_id.to_string());
        }
        self.update_channel_unread(channel_id);

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
            && self.pending_thread.is_none()
            && self.messages.active_thread.is_none()
            && self.load_reason == LoadReason::ChannelOpen
            && let Some(ts) = newest_ts
        {
            self.mark_channel_read(channel_id, &ts);
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
            .take(self.image_requests.remaining_capacity())
            .collect::<Vec<_>>();

        for (image_key, image_url) in requests {
            if self.image_requests.enqueue(
                &self.slack,
                ImageTarget::File(image_key.clone()),
                &image_url,
            ) {
                self.requested_file_images.insert(image_key);
            }
        }
    }

    fn poll_if_due(&mut self) {
        // Background channels polled per cycle.  One per cycle
        // hydrates unread state for large workspaces far too slowly
        // (N channels -> N cycles); many more risks Slack's
        // conversations.history rate limit (~50/min).
        const BG_POLL_BATCH: usize = 3;

        if self
            .last_activity_poll
            .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(60))
        {
            self.refresh_activity();
        }
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
        self.spawn_fetch_messages(channel_id, HISTORY_LIMIT, false, false);
    }

    /// Foreground refresh: updates the message list but swallows errors
    /// so transient poll failures don't flash in the status bar.
    fn load_messages_refresh(&self, channel_id: &str) {
        self.spawn_fetch_messages(channel_id, HISTORY_LIMIT, false, true);
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
                    let _ = tx.send(Action::ThreadOpenFailed {
                        target: ThreadTarget {
                            channel_id,
                            thread_ts,
                        },
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
    use crate::app::*;
    use crate::slack::types::{Channel, SlackMessage};

    fn test_app() -> App {
        let config = Config::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let store = Store::open_in_memory().expect("test store");
        App::new(config, tx, None, store).expect("test app")
    }

    #[test]
    fn help_is_modal_preserves_context_and_allows_question_marks_in_drafts() {
        let mut app = test_app();
        app.focus = Focus::Messages;
        app.current_channel_id = Some("C1".into());
        app.messages.set_thread("1.0".into(), vec![msg("1.0")]);
        app.input.insert_text("draft");

        let open = app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert!(matches!(open, Action::OpenHelp));
        app.update(open);
        for code in [
            KeyCode::Tab,
            KeyCode::Left,
            KeyCode::Char('i'),
            KeyCode::Char('a'),
        ] {
            assert!(matches!(
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)),
                Action::Render
            ));
        }
        assert!(matches!(app.handle_paste("ignored"), Action::Render));
        assert!(matches!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            Action::Render
        ));
        let close = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(close, Action::CloseHelp));
        app.update(close);
        assert!(app.help_popup.is_none());
        assert!(!app.should_quit);
        assert_eq!(app.focus, Focus::Messages);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.current_channel_id.as_deref(), Some("C1"));
        assert_eq!(app.messages.active_thread.as_deref(), Some("1.0"));
        assert_eq!(app.input.get_text(), "draft");

        app.update(Action::EnterInsertMode);
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert_eq!(app.input.get_text(), "draft?");
        assert!(app.help_popup.is_none());
        app.update(Action::EnterNormalMode);
        app.update(Action::OpenHelp);
        assert!(matches!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        ));
    }

    #[test]
    fn footer_keeps_help_visible_when_status_is_long() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = test_app();
        app.status_message = Some("Long status message ".repeat(20));
        for width in [20, 52, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).expect("terminal");
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let footer: String = (0..width)
                .filter_map(|x| buffer.cell((x, 19)))
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            assert!(footer.ends_with(" ?:help "), "{footer}");
            assert!(footer.starts_with(" NORMAL "), "{footer}");
        }
    }

    #[test]
    fn space_previews_the_selected_message_and_stays_text_in_insert_mode() {
        let mut app = test_app();
        app.focus = Focus::Messages;
        let action = app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(action, Action::PreviewMedia));
        app.update(action);
        assert!(app.preview_request.is_none());

        let mut message = msg("100.1");
        message.text = "https://example.com/image.png".into();
        app.messages.set_channel(vec![message], "general".into());
        app.update(Action::PreviewMedia);
        assert!(app.preview_request.is_some());
        app.messages.set_channel(vec![], "other".into());
        app.update(Action::PreviewMedia);
        assert_eq!(
            app.status_message.as_deref(),
            Some("Close Quick Look or quit the preview's QuickTime instance first")
        );
        drop(app.preview_request.take());

        app.update(Action::EnterInsertMode);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.input.get_text(), " ");
        assert!(app.preview_request.is_none());
    }

    #[test]
    fn full_image_queue_leaves_visible_avatars_resolvable() {
        let mut app = test_app();
        app.picker = Some(Picker::halfblocks());
        app.messages.set_channel(vec![msg("1.0")], "general".into());
        while app.image_requests.remaining_capacity() > 0 {
            assert!(app.image_requests.enqueue(
                &app.slack,
                ImageTarget::File("queued".into()),
                "https://example.com/image.png"
            ));
        }
        app.handle_user_resolved(
            "U1".into(),
            "Alice",
            Some("https://example.com/avatar.png".into()),
        );
        assert_eq!(
            unresolved_user_ids(&app.messages.messages, &app.messages.user_cache),
            vec!["U1"]
        );

        app.image_requests = ImagePipeline::default();
        let capacity = app.image_requests.remaining_capacity();
        app.handle_user_resolved(
            "U1".into(),
            "Alice",
            Some("https://example.com/avatar.png".into()),
        );
        assert_eq!(
            app.messages.user_cache.get("U1").map(String::as_str),
            Some("Alice")
        );
        assert_eq!(app.image_requests.remaining_capacity(), capacity - 1);
    }

    fn activity_snapshot(
        entries: Vec<crate::activity::ActivityEntry>,
        now: &str,
    ) -> crate::activity::ActivitySnapshot {
        crate::activity::ActivitySnapshot {
            entries,
            complete: true,
            started_ts: now.into(),
            read_revision: 0,
        }
    }

    #[tokio::test]
    #[expect(clippy::panic_in_result_fn, reason = "test assertions")]
    async fn activity_notifies_renders_and_opens_exact_thread() -> color_eyre::Result<()> {
        use crate::activity::test_entry;
        use ratatui::{Terminal, backend::TestBackend};
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(Config::default(), tx, None, Store::open_in_memory()?)?;
        app.messages
            .user_cache
            .insert("UROOT".into(), "Alice".into());
        app.messages
            .user_cache
            .insert("UOTHER".into(), "Bob".into());
        app.self_user_id = Some("test".into());
        app.sidebar.set_channels(two_channels());
        app.update(Action::ActivityLoaded(Ok(activity_snapshot(
            vec![],
            "1700000100.000000",
        ))));
        let entry = test_entry("CARCHIVED", "1700000101.000000");
        app.update(Action::ActivityLoaded(Ok(activity_snapshot(
            vec![entry.clone()],
            "1700000102.000000",
        ))));
        assert!(app.pending_terminal_notification.take().is_some());
        assert_eq!(app.latest_notified_thread.as_ref(), Some(&entry.target));
        assert!(app.store.get_session("thread_activity/test")?.is_some());
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.update(action);
        app.update(Action::EnterInsertMode);
        assert_eq!(app.mode, Mode::Normal);
        let mut terminal = Terminal::new(TestBackend::new(100, 20))?;
        terminal.draw(|frame| crate::ui::render(frame, &mut app))?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Activity · Replies"));
        assert!(rendered.contains("Review is ready"));
        assert!(app.panes.input.is_empty());
        assert!(
            matches!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Action::OpenActivityThread(target) if target == entry.target)
        );

        let (client, server) = reaction_test_server(vec![serde_json::json!({"ok":true,"messages":[{"ts":entry.target.thread_ts,"text":"Root"},{"ts":entry.latest_ts,"text":"Reply"}]}).to_string()]).await;
        app.slack = client;
        app.update(Action::OpenLatestNotification);
        let action = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await?
            .expect("thread result");
        app.update(action);
        assert!(!app.activity_open);
        assert_eq!(app.current_channel_id.as_deref(), Some("CARCHIVED"));
        assert_eq!(
            app.messages.active_thread.as_deref(),
            Some(entry.target.thread_ts.as_str())
        );
        assert_eq!(app.activity.inbox.unread_count(), 0);
        assert!(app.thread_read_request.is_some());
        let requests = server.await?;
        assert!(
            requests[0]
                .0
                .contains("channel=CARCHIVED&ts=1700000000.000001")
        );
        Ok(())
    }

    #[tokio::test]
    async fn activity_errors_and_scrolled_thread_refresh_keep_unread_work() {
        use crate::activity::test_entry;
        let mut app = test_app();
        app.messages
            .user_cache
            .insert("UROOT".into(), "Alice".into());
        app.messages
            .user_cache
            .insert("UOTHER".into(), "Bob".into());
        let entry = test_entry("CARCHIVED", "1700000101.000000");
        app.update(Action::ActivityLoaded(Ok(activity_snapshot(
            vec![entry.clone()],
            "1700000102.000000",
        ))));
        app.update(Action::ActivityLoaded(Err("offline".into())));
        assert_eq!(app.activity.inbox.unread_count(), 1);
        assert_eq!(app.activity.error.as_deref(), Some("offline"));
        app.current_channel_id = Some(entry.target.channel_id.clone());
        app.pending_thread = Some("1700000001.000001".into());
        app.loading = true;
        app.update(Action::ThreadOpenFailed {
            target: entry.target.clone(),
            message: "stale".into(),
        });
        assert!(app.loading);
        assert_eq!(app.pending_thread.as_deref(), Some("1700000001.000001"));
        app.pending_thread = None;
        app.messages
            .set_thread(entry.target.thread_ts.clone(), Vec::new());
        app.messages.scroll_offset = 5;
        app.terminal_focused = true;
        app.update(Action::ThreadRepliesLoaded {
            channel_id: entry.target.channel_id,
            thread_ts: entry.target.thread_ts,
            messages: vec![msg(&entry.latest_ts)],
        });
        assert_eq!(app.activity.inbox.unread_count(), 1);
        assert!(app.thread_read_request.is_none());
    }

    async fn reaction_test_server(
        responses: Vec<String>,
    ) -> (
        SlackClient,
        tokio::task::JoinHandle<Vec<(String, serde_json::Value)>>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let address = listener.local_addr().expect("address");
        let client = SlackClient::with_api_base(format!("http://{address}")).expect("client");
        let server = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async move {
                let mut requests = Vec::new();
                for response in responses {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let mut stream = BufReader::new(stream);
                    let mut request_line = String::new();
                    stream.read_line(&mut request_line).await.expect("request line");
                    let mut content_length = 0;
                    let mut header_bytes = request_line.len();
                    loop {
                        let mut line = String::new();
                        assert!(stream.read_line(&mut line).await.expect("header") > 0);
                        header_bytes += line.len();
                        assert!(header_bytes < 8192, "oversized request headers");
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            content_length = value.trim().parse::<usize>().expect("length");
                        }
                    }
                    assert!(content_length <= 4096, "oversized request body");
                    let mut body = vec![0; content_length];
                    stream.read_exact(&mut body).await.expect("body");
                    let body = if body.is_empty() { serde_json::Value::Null } else { serde_json::from_slice(&body).expect("JSON body") };
                    requests.push((request_line, body));
                    let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len());
                    stream.get_mut().write_all(reply.as_bytes()).await.expect("reply");
                }
                requests
            }).await.expect("server timeout")
        });
        (client, server)
    }

    async fn next_reaction_result(app: &mut App) -> Action {
        let request = app.reaction_request.take().expect("pending reaction");
        tokio::time::timeout(std::time::Duration::from_secs(5), request)
            .await
            .expect("reaction timeout")
    }

    #[test]
    fn reaction_picker_is_modal_and_requires_a_selected_message() {
        let mut app = test_app();
        app.focus = Focus::Messages;
        let open = app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert!(matches!(open, Action::OpenReactionPicker));
        app.update(open);
        assert!(app.reaction_picker.is_none());
        assert_eq!(app.status_message.as_deref(), Some("No message selected"));

        app.current_channel_id = Some("C1".into());
        app.messages.set_channel(vec![msg("1.0")], "general".into());
        app.input.insert_text("draft");
        app.update(Action::OpenReactionPicker);
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.update(action);
        assert!(!app.should_quit);
        assert_eq!(
            app.reaction_picker
                .as_ref()
                .and_then(ReactionPicker::selected_name),
            Some("q")
        );
        assert_eq!(app.input.get_text(), "draft");
        let close = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.update(close);
        assert!(app.reaction_picker.is_none());
        assert!(app.reaction_request.is_none());
    }

    #[tokio::test]
    async fn reactions_add_exact_message_and_refresh_channel_or_thread() {
        for in_thread in [false, true] {
            let expected_ts = if in_thread { "200.2" } else { "100.1" };
            let refreshed = serde_json::json!({"ok": true, "messages": [{
                "ts": expected_ts, "user": "U1", "text": "hello",
                "reactions": [{"name": "team_party", "count": 1, "users": ["U1"]}]
            }]});
            let (client, server) = reaction_test_server(vec![
                serde_json::json!({"ok": true}).to_string(),
                refreshed.to_string(),
            ])
            .await;
            let mut app = test_app();
            let (tx, mut rx) = mpsc::unbounded_channel();
            app.action_tx = tx;
            app.slack = client;
            app.current_channel_id = Some("C1".into());
            app.loading = false;
            app.messages.user_cache.insert("U1".into(), "me".into());
            if in_thread {
                let mut reply = msg("200.2");
                reply.thread_ts = Some("100.1".into());
                app.messages
                    .set_thread("100.1".into(), vec![msg("100.1"), reply]);
                app.messages.selected_message = 1;
            } else {
                app.messages
                    .set_channel(vec![msg("200.2"), msg("100.1")], "general".into());
                app.messages.selected_message = 0;
            }
            app.update(Action::OpenReactionPicker);
            app.handle_paste(":team_party:");
            // A poll/selection change after opening must not retarget the write.
            app.messages.selected_message = usize::from(!in_thread);
            app.update(Action::SubmitReaction);
            app.update(Action::SubmitReaction);
            app.update(Action::OpenReactionPicker);
            assert!(app.reaction_picker.is_none());
            assert!(app.reaction_request.is_some());

            let result = next_reaction_result(&mut app).await;
            assert!(matches!(&result, Action::ReactionAdded { target, name }
                if target.channel_id == "C1" && target.message_ts == expected_ts && name == "team_party"));
            app.update(result);
            let refresh = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("refresh timeout")
                .expect("refresh action");
            app.update(refresh);
            assert!(app.messages.messages.iter().any(|message| {
                message.ts == expected_ts
                    && message
                        .reactions
                        .iter()
                        .any(|reaction| reaction.name == "team_party")
            }));
            assert_eq!(app.status_message.as_deref(), Some("Added :team_party:"));

            let requests = server.await.expect("server task");
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].0.trim(), "POST /reactions.add HTTP/1.1");
            assert_eq!(
                requests[0].1,
                serde_json::json!({"channel": "C1", "timestamp": expected_ts, "name": "team_party"})
            );
            let path = requests[1]
                .0
                .split_whitespace()
                .nth(1)
                .expect("request path");
            let url = reqwest::Url::parse(&format!("http://localhost{path}")).expect("refresh URL");
            assert_eq!(
                url.path(),
                if in_thread {
                    "/conversations.replies"
                } else {
                    "/conversations.history"
                }
            );
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "channel" && value == "C1")
            );
            if in_thread {
                assert!(
                    url.query_pairs()
                        .any(|(key, value)| key == "ts" && value == "100.1")
                );
            }
        }
    }

    #[tokio::test]
    async fn reaction_failure_is_reported_and_does_not_reset_other_operations() {
        for error in ["already_reacted", "missing_scope"] {
            let (client, server) = reaction_test_server(vec![
                serde_json::json!({"ok": false, "error": error}).to_string(),
            ])
            .await;
            let mut app = test_app();
            app.slack = client;
            app.current_channel_id = Some("C1".into());
            app.messages.set_channel(vec![msg("1.0")], "general".into());
            app.pending_sends = 1;
            app.pending_thread = Some("other_thread".into());
            app.input.insert_text("unsent draft");
            app.update(Action::OpenReactionPicker);
            app.handle_paste("thumbsup::skin-tone-4");
            app.update(Action::SubmitReaction);
            let result = next_reaction_result(&mut app).await;
            assert!(
                matches!(&result, Action::Error { context: ErrorContext::Reaction, message } if message.contains(error))
            );
            app.update(result);
            assert_eq!(app.pending_sends, 1);
            assert_eq!(app.pending_thread.as_deref(), Some("other_thread"));
            assert_eq!(app.input.get_text(), "unsent draft");
            assert!(app.reaction_request.is_none());
            app.update(Action::OpenReactionPicker);
            assert!(app.reaction_picker.is_some());
            assert_eq!(server.await.expect("server task").len(), 1);
        }
    }

    #[tokio::test]
    async fn reaction_completion_does_not_replace_another_channel() {
        let (client, server) =
            reaction_test_server(vec![serde_json::json!({"ok": true}).to_string()]).await;
        let mut app = test_app();
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.action_tx = tx;
        app.slack = client;
        app.current_channel_id = Some("C1".into());
        app.messages.set_channel(vec![msg("1.0")], "general".into());
        app.update(Action::OpenReactionPicker);
        app.handle_paste("eyes");
        app.update(Action::SubmitReaction);
        app.current_channel_id = Some("C2".into());
        app.messages.set_channel(vec![msg("9.0")], "random".into());
        let result = next_reaction_result(&mut app).await;
        app.update(result);
        assert_eq!(app.messages.selected_message_ts(), Some("9.0"));
        assert!(rx.try_recv().is_err());
        let requests = server.await.expect("server task");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1["channel"], "C1");
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

        app.current_channel_id = Some("C1".into());
        app.update(Action::ThreadOpenFailed {
            target: ThreadTarget {
                channel_id: "C1".into(),
                thread_ts: "100.0".into(),
            },
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
        app.mark_channel_read("C_OTHER", "2.0");

        let mut own = msg("2.0");
        own.user = "U_ME".into();
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![own],
            is_background: true,
        });

        assert!(!app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(app.pending_terminal_notification.take().is_none());
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
        assert!(app.pending_terminal_notification.take().is_none());
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
        assert!(app.pending_terminal_notification.take().is_none());
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
        assert!(app.pending_terminal_notification.take().is_some());
    }

    #[test]
    fn chat_notifications_skip_history_and_repeat_only_for_new_messages() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        for (ts, expected) in [("1.0", false), ("2.0", true), ("2.0", false), ("3.0", true)] {
            app.update(Action::MessagesLoaded {
                channel_id: "C_OTHER".into(),
                messages: vec![msg(ts)],
                is_background: true,
            });
            assert_eq!(
                app.pending_terminal_notification.take().is_some(),
                expected,
                "timestamp {ts}"
            );
        }
    }

    #[test]
    fn first_message_after_empty_history_notifies() {
        let mut app = test_app();
        app.current_channel_id = Some("C_CURRENT".into());
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: Vec::new(),
            is_background: true,
        });
        assert!(!app.sidebar.unread_channels.contains("C_OTHER"));
        assert!(app.pending_terminal_notification.take().is_none());
        app.update(Action::MessagesLoaded {
            channel_id: "C_OTHER".into(),
            messages: vec![msg("1.0")],
            is_background: true,
        });
        assert!(app.pending_terminal_notification.take().is_some());
    }

    #[tokio::test]
    async fn notification_names_and_opens_the_original_channel_or_dm() {
        let (client, server) = reaction_test_server(vec![
            r#"{"ok":true,"messages":[]}"#.into(),
            r#"{"ok":true,"messages":[]}"#.into(),
        ])
        .await;
        let mut app = test_app();
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.action_tx = tx;
        app.slack = client;
        app.current_channel_id = Some("C1".into());
        let mut channels = two_channels();
        channels.push(Channel {
            id: "D1".into(),
            name: None,
            is_channel: Some(false),
            is_im: Some(true),
            is_member: Some(true),
            user: "U_ALICE".into(),
        });
        app.sidebar.set_channels(channels);
        app.sidebar.update_dm_name("U_ALICE", "Alice");

        for (id, label, is_dm) in [("C2", "random", false), ("D1", "Alice", true)] {
            app.latest_ts.insert(id.into(), "1.0".into());
            app.update(Action::MessagesLoaded {
                channel_id: id.into(),
                messages: vec![msg("2.0")],
                is_background: true,
            });
            assert_eq!(
                app.pending_terminal_notification.take(),
                Some(ChatNotification::new(id, label, is_dm))
            );
            assert!(app.pending_terminal_notification.take().is_none());
            app.sidebar.unread_channels.remove(id);
            app.sidebar.filter_unread = true;
            let action = app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
            assert!(matches!(action, Action::OpenLatestNotification));
            app.update(action);
            assert_eq!(app.current_channel_id.as_deref(), Some(id));
            assert_eq!(
                app.sidebar.selected_channel().map(|ch| ch.id.as_str()),
                Some(id)
            );
            assert_eq!(app.focus, Focus::Messages);
            let loaded = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("history response timeout")
                .expect("history response");
            assert!(
                matches!(&loaded, Action::MessagesLoaded { channel_id, .. } if channel_id == id)
            );
            app.update(loaded);
        }
        let requests = server.await.expect("server task");
        assert_eq!(requests.len(), 2);
        for ((request, _), id) in requests.iter().zip(["C2", "D1"]) {
            let path = request.split_whitespace().nth(1).expect("request path");
            let url = reqwest::Url::parse(&format!("http://localhost{path}")).expect("history URL");
            assert_eq!(url.path(), "/conversations.history");
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "channel" && value == id)
            );
        }
    }

    #[test]
    fn current_channel_notifies_when_terminal_loses_focus() {
        let mut app = test_app();
        app.current_channel_id = Some("C1".into());
        app.latest_ts.insert("C1".into(), "1.0".into());
        for (ts, focused) in [("2.0", true), ("3.0", false)] {
            app.terminal_focused = focused;
            app.update(Action::MessagesLoaded {
                channel_id: "C1".into(),
                messages: vec![msg(ts)],
                is_background: true,
            });
            assert_eq!(app.pending_terminal_notification.take().is_some(), !focused);
        }
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
            activity: Rect::default(),
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

        // Navigate to C2
        app.sidebar.selected = 1;
        app.update(Action::OpenChannel);

        // C1 should now be flushed
        assert!(!app.sidebar.unread_channels.contains("C1"));
        // C2 still unread
        assert!(app.sidebar.unread_channels.contains("C2"));
    }

    #[tokio::test]
    async fn leaving_channel_retains_unread_messages_after_its_read_marker() {
        let mut app = test_app();
        app.terminal_focused = true;
        app.sidebar.set_channels(two_channels());
        app.messages.user_cache.insert("U1".into(), "Alice".into());
        app.current_channel_id = Some("C1".into());
        app.loading = true;
        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.000001")],
            is_background: false,
        });
        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.000002")],
            is_background: false,
        });

        app.sidebar.selected = 1;
        app.update(Action::OpenChannel);
        assert!(app.sidebar.unread_channels.contains("C1"));

        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.000002")],
            is_background: true,
        });
        assert!(app.sidebar.unread_channels.contains("C1"));
        assert!(app.pending_terminal_notification.take().is_none());
        assert_eq!(
            app.last_seen_ts.get("C1").map(String::as_str),
            Some("100.000001")
        );
    }

    #[tokio::test]
    async fn leaving_unloaded_channel_restores_known_unread_messages() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.current_channel_id = Some("C1".into());
        app.loading = true;
        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.000002")],
            is_background: true,
        });

        app.sidebar.selected = 1;
        app.update(Action::OpenChannel);

        assert!(app.sidebar.unread_channels.contains("C1"));
        assert!(app.pending_terminal_notification.take().is_none());
    }

    #[test]
    fn mark_all_read_does_not_regress_a_confirmed_send() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.current_channel_id = Some("C2".into());
        app.latest_ts.insert("C1".into(), "100.000001".into());
        app.mark_channel_read("C1", "100.000003");

        app.update(Action::MarkAllRead);
        app.update(Action::MessagesLoaded {
            channel_id: "C1".into(),
            messages: vec![msg("100.000002")],
            is_background: true,
        });

        assert!(!app.sidebar.unread_channels.contains("C1"));
        assert_eq!(
            app.store
                .all_read_state()
                .expect("read state")
                .get("C1")
                .map(String::as_str),
            Some("100.000003")
        );
    }

    #[test]
    fn toggle_unread_filter_off_keeps_deferred_read() {
        let mut app = test_app();
        app.sidebar.set_channels(two_channels());
        app.sidebar.unread_channels.insert("C1".into());
        app.sidebar.filter_unread = true;
        app.current_channel_id = Some("C1".into());

        // Toggle filter OFF — the open channel stays in the sidebar's
        // Unread section until the user navigates away.
        app.update(Action::ToggleUnreadFilter);

        assert!(!app.sidebar.filter_unread);
        assert!(app.sidebar.unread_channels.contains("C1"));
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
    fn toggle_reactions_without_reactions_sets_status() {
        let mut app = test_app();
        app.messages.refresh_messages(vec![msg("1.0")]);

        app.update(Action::ToggleReactions);

        assert!(app.reactions_popup.is_none());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Selected message has no reactions")
        );
    }

    #[tokio::test]
    async fn toggle_reactions_opens_then_closes_popup() {
        use crate::slack::types::Reaction;

        let mut app = test_app();
        let mut message = msg("1.0");
        message.reactions.push(Reaction {
            name: "thumbsup".into(),
            count: 2,
            users: vec!["U2".into(), "U3".into()],
        });
        app.messages.refresh_messages(vec![message]);

        app.update(Action::ToggleReactions);
        assert!(app.reactions_popup.is_some());
        // Reactor names get resolution requests issued.
        assert!(app.requested_users.contains("U2"));
        assert!(app.requested_users.contains("U3"));

        app.update(Action::ToggleReactions);
        assert!(app.reactions_popup.is_none());
    }

    #[test]
    fn popup_captures_keys_and_closes_without_quitting() {
        use crate::components::reactions::ReactionsPopup;
        use crate::slack::types::Reaction;

        let mut app = test_app();
        app.focus = Focus::Messages;
        app.reactions_popup = Some(ReactionsPopup::new(vec![Reaction {
            name: "tada".into(),
            count: 1,
            users: vec!["U2".into()],
        }]));

        // 'q' closes the popup instead of quitting the app.
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(action, Action::ToggleReactions));
        app.update(action);
        assert!(!app.should_quit);
        assert!(app.reactions_popup.is_none());
    }

    #[test]
    fn reactions_key_in_messages_panel_requests_toggle() {
        let mut app = test_app();
        app.focus = Focus::Messages;

        let action = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

        assert!(matches!(action, Action::ToggleReactions));
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

    #[test]
    fn copy_without_selected_message_sets_status() {
        let mut app = test_app();

        app.update(Action::CopyMessage);

        assert_eq!(app.status_message.as_deref(), Some("No message selected"));
    }

    #[test]
    fn copy_message_without_text_sets_status() {
        let mut app = test_app();
        let mut message = msg("1.0");
        message.text.clear();
        app.messages.refresh_messages(vec![message]);

        app.update(Action::CopyMessage);

        assert_eq!(
            app.status_message.as_deref(),
            Some("Selected message has no text")
        );
    }

    #[test]
    fn open_link_key_uses_message_focus_and_handles_missing_links() {
        let mut app = test_app();
        app.focus = Focus::Messages;
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert!(matches!(action, Action::OpenMessageLink));
        app.update(action);
        assert_eq!(app.status_message.as_deref(), Some("No message selected"));

        app.messages.refresh_messages(vec![msg("1.0")]);
        app.update(Action::OpenMessageLink);
        assert_eq!(
            app.status_message.as_deref(),
            Some("Selected message has no web links")
        );
        assert!(app.browser_request.is_none());
    }

    #[test]
    fn link_picker_keeps_destinations_stable_across_message_refreshes() {
        let mut app = test_app();
        app.focus = Focus::Messages;
        let mut message = msg("1.0");
        message.text = "<https://example.com/first|one> <https://example.com/second|two>".into();
        app.messages.set_thread("1.0".into(), vec![message]);
        app.update(Action::OpenMessageLink);
        assert!(app.link_picker.is_some());

        app.messages.refresh_messages(vec![msg("2.0")]);
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.update(action);
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&action, Action::OpenUrl(url) if url.as_str() == "https://example.com/second")
        );
        app.update(action);
        assert!(app.link_picker.is_none());
        // Constructing the request does not launch a browser until the UI polls it.
        assert!(app.browser_request.is_some());
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
