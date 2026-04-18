use std::collections::{HashMap, HashSet};
use std::time::Instant;

use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use crate::action::Action;
use crate::components::input::TextInput;
use crate::components::messages::MessageList;
use crate::components::sidebar::ChannelSidebar;
use crate::components::{Component, EventResult};
use crate::config::Config;
use crate::event::{AppEvent, EventHandler};
use crate::slack::client::SlackClient;
use crate::slack::types::{Channel, SlackMessage, is_slack_user_id, mentioned_user_ids};
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
    /// Thread ts currently being fetched — thread mode enters only
    /// after replies arrive.
    pending_thread: Option<String>,
    /// True while waiting for channel history to reload after closing
    /// a thread.  The UI stays in thread mode until the channel data
    /// arrives so stale thread replies are never shown under the
    /// channel title.
    closing_thread: bool,
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

impl App {
    pub fn new(
        config: Config,
        action_tx: mpsc::UnboundedSender<Action>,
        picker: Option<Picker>,
        store: Store,
    ) -> Self {
        let slack = SlackClient::new(&config.slack_token, &config.cookie);

        // Load persisted read state from SQLite
        let last_seen_ts = store.all_read_state().unwrap_or_default();

        Self {
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
            pending_thread: None,
            closing_thread: false,
            mark_all_read_cutoffs: HashMap::new(),
            deferred_read_channel: None,
            status_message: Some("Loading channels...".into()),
            loading: true,
            load_reason: LoadReason::ChannelOpen,
            pending_sends: 0,
        }
    }

    pub async fn run(
        &mut self,
        tui: &mut Tui,
        action_rx: mpsc::UnboundedReceiver<Action>,
    ) -> Result<()> {
        let mut events = EventHandler::new(self.config.tick_rate(), action_rx);

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
                AppEvent::BackgroundAction(action) => action,
            };

            self.update(action);

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
            Action::MessageSent {
                channel_id,
                thread_ts,
            } => {
                self.pending_sends = self.pending_sends.saturating_sub(1);
                if self.pending_sends == 0 {
                    self.input.clear();
                }
                if let Some(ref ts) = thread_ts {
                    self.load_thread_replies(&channel_id, ts);
                } else {
                    self.load_messages(&channel_id);
                }
            }
            Action::UserResolved {
                user_id,
                display_name,
                avatar_url,
            } => {
                self.handle_user_resolved(user_id, &display_name, avatar_url);
            }
            Action::AvatarDownloaded {
                user_id,
                image_data,
            } => {
                self.handle_avatar_downloaded(&user_id, &image_data);
            }
            Action::StarsLoaded(starred) => {
                self.sidebar.set_starred(starred);
            }
            Action::CustomEmojiLoaded(emoji_map) => {
                self.handle_custom_emoji_loaded(&emoji_map);
            }
            Action::AuthValidated { user_name } => {
                tracing::info!("Authenticated as {user_name}");
            }
            Action::Error(msg) => {
                tracing::error!("{msg}");
                self.status_message = Some(msg);
                self.loading = false;
                // Clear pending states so the user can retry the
                // failed operation (thread open, thread close, send).
                self.pending_thread = None;
                self.closing_thread = false;
                self.pending_sends = 0;
            }
            Action::Tick => {
                self.poll_if_due();
            }
            Action::Render => {}
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
        self.pending_thread = None;
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
        }
    }

    fn handle_mark_all_read(&mut self) {
        self.sidebar.mark_all_read();
        self.deferred_read_channel = None;
        // Persist for channels with known timestamps.
        for (ch_id, ts) in &self.latest_ts {
            self.last_seen_ts.insert(ch_id.clone(), ts.clone());
            if let Err(e) = self.store.mark_read(ch_id, ts) {
                tracing::warn!("Failed to persist read state: {e}");
            }
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
        if !text.is_empty() {
            let thread_ts = self.messages.active_thread.clone();
            self.pending_sends += 1;
            self.send_message(channel_id, text, thread_ts);
            // Don't clear yet — cleared in MessageSent handler on success.
            // On failure the draft stays intact for retry.
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
        if self.current_channel_id.as_deref() != Some(channel_id) {
            return;
        }
        // Ignore stale responses if a different thread was requested
        // or the user navigated away.
        if self.pending_thread.as_deref() != Some(thread_ts) {
            return;
        }
        self.pending_thread = None;
        self.messages.set_thread(thread_ts.to_string(), messages);
        self.loading = false;
        self.resolve_missing_users();
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
        // last_poll by the poll interval.
        if let Some(past) = Instant::now().checked_sub(self.config.poll_interval()) {
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
        let newest_ts = messages.first().map(|m| m.ts.clone());
        if let Some(ref ts) = newest_ts {
            let prev = self.latest_ts.get(channel_id);
            let is_new = prev.is_none_or(|p| ts.as_str() > p.as_str());
            if is_new {
                self.latest_ts.insert(channel_id.to_string(), ts.clone());

                // If MarkAllRead recorded a cutoff for this channel,
                // only suppress messages at or before that cutoff.
                // Messages genuinely newer than the cutoff trigger unread.
                if let Some(cutoff) = self.mark_all_read_cutoffs.get(channel_id) {
                    if ts.as_str() <= cutoff.as_str() {
                        self.last_seen_ts.insert(channel_id.to_string(), ts.clone());
                    } else {
                        // Genuinely new — remove the cutoff and let
                        // normal unread detection run.
                        self.mark_all_read_cutoffs.remove(channel_id);
                        let is_current = self.current_channel_id.as_deref() == Some(channel_id);
                        if !is_current {
                            self.sidebar.unread_channels.insert(channel_id.to_string());
                        }
                    }
                } else {
                    let is_current = self.current_channel_id.as_deref() == Some(channel_id);
                    let last_seen = self.last_seen_ts.get(channel_id);
                    let unseen = last_seen.is_none_or(|s| ts.as_str() > s.as_str());
                    if !is_current && unseen {
                        self.sidebar.unread_channels.insert(channel_id.to_string());
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
        self.loading = false;

        self.resolve_missing_users();
    }

    fn resolve_missing_users(&mut self) {
        let to_resolve = unresolved_user_ids(&self.messages.messages, &self.messages.user_cache);
        for user_id in to_resolve {
            self.resolve_user(user_id);
        }
    }

    fn poll_if_due(&mut self) {
        if self.last_poll.elapsed() < self.config.poll_interval() {
            return;
        }
        self.last_poll = Instant::now();

        // Refresh current view (errors suppressed — user already has
        // the messages and a transient failure shouldn't disrupt reading).
        if let Some(ref channel_id) = self.current_channel_id {
            if let Some(ref thread_ts) = self.messages.active_thread {
                self.load_thread_replies_quiet(channel_id, thread_ts);
            } else {
                self.load_messages_refresh(channel_id);
            }
        }

        // Also poll one background channel per cycle
        let channels = &self.sidebar.channels;
        if channels.is_empty() {
            return;
        }
        self.poll_rotation %= channels.len();
        let bg_channel_id = channels[self.poll_rotation].id.clone();
        self.poll_rotation += 1;

        if self.current_channel_id.as_deref() != Some(&bg_channel_id) {
            self.load_messages_background(&bg_channel_id);
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
            .ok()
            .flatten();
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
                        user_name: info.user,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!(
                        "Auth failed: {e}. Check your token."
                    )));
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
                    let _ = tx.send(Action::Error(format!("Failed to load channels: {e}")));
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
                    let _ = tx.send(Action::Error(format!("Failed to load messages: {e}")));
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
                Ok(()) => {
                    let _ = tx.send(Action::MessageSent {
                        channel_id,
                        thread_ts,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!("Failed to send: {e}")));
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
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!("Failed to load thread: {e}")));
                }
            }
        });
    }

    fn resolve_user(&self, user_id: String) {
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
            match client.download_bytes(&url).await {
                Ok(data) => {
                    let _ = tx.send(Action::AvatarDownloaded {
                        user_id,
                        image_data: data,
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to download avatar for {user_id}: {e}");
                }
            }
        });
    }

    fn handle_avatar_downloaded(&mut self, user_id: &str, image_data: &[u8]) {
        let Some(ref mut picker) = self.picker else {
            return;
        };
        let Ok(img) = image::load_from_memory(image_data) else {
            tracing::warn!("Failed to decode avatar for {user_id}");
            return;
        };
        let protocol = picker.new_resize_protocol(img);
        self.messages
            .avatar_protocols
            .insert(user_id.to_string(), protocol);
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
    use super::*;
    use crate::slack::types::{Channel, SlackMessage};

    fn test_app() -> App {
        let config = Config::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let store = Store::open_in_memory().expect("test store");
        App::new(config, tx, None, store)
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

        app.update(Action::Error("oops".into()));

        assert_eq!(app.status_message.as_deref(), Some("oops"));
        assert!(!app.loading);
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
