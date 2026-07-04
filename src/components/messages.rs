use std::collections::{HashMap, VecDeque};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::action::Action;
use crate::components::{Component, EventResult};
use crate::emoji;
use crate::slack::types::{Reaction, SlackFile, SlackMessage, replace_user_mentions};

const AVATAR_WIDTH: u16 = 4;
const IMAGE_PREVIEW_HEIGHT: u16 = 8;
const IMAGE_PREVIEW_WIDTH: u16 = 56;
/// Right-side padding inside the messages panel (matches the 1-column
/// gap the left border already provides).
const MSG_RIGHT_PAD: u16 = 1;

/// Subtle background for the selected message highlight.
const SELECTED_MSG_BG: Color = Color::Rgb(40, 40, 50);

/// Cap on cached decoded image previews — each holds a decoded RGB
/// buffer (several MB for a 1024px thumb), so an unbounded cache grows
/// by hundreds of MB over a long session.
const MAX_FILE_IMAGES: usize = 48;

pub struct MessageList {
    pub messages: Vec<SlackMessage>,
    pub channel_name: String,
    pub scroll_offset: u16,
    /// Index into the *display-order* message list (0 = top-most).
    pub selected_message: usize,
    pub user_cache: HashMap<String, String>,
    pub avatar_protocols: HashMap<String, StatefulProtocol>,
    pub file_image_protocols: HashMap<String, StatefulProtocol>,
    pub custom_emoji_urls: HashMap<String, String>,
    pub active_thread: Option<String>,
    read_marker_ts: Option<String>,
    line_cache: Option<(usize, Vec<VisualLine>)>,
    /// One-shot request to scroll the selected message into view on
    /// the next render.  Set by selection changes only, so the free
    /// line-scroll keys (j/k, Ctrl+u/d/…) are never overridden.
    scroll_to_selected: bool,
    /// Inner height of the most recently rendered viewport; used to
    /// size half-page / full-page scrolling.
    viewport_rows: u16,
    /// Insertion order of `file_image_protocols`, oldest first, for
    /// bounded eviction.
    file_image_order: VecDeque<String>,
    /// Inner (borderless) area of the last render, for mouse
    /// hit-testing.  Zero-sized before the first render.
    last_inner: Rect,
    /// Index into the line cache of the first rendered row.
    viewport_start: usize,
}

impl MessageList {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            channel_name: String::new(),
            scroll_offset: 0,
            selected_message: 0,
            user_cache: HashMap::new(),
            avatar_protocols: HashMap::new(),
            file_image_protocols: HashMap::new(),
            custom_emoji_urls: HashMap::new(),
            active_thread: None,
            read_marker_ts: None,
            line_cache: None,
            scroll_to_selected: true,
            viewport_rows: 10,
            file_image_order: VecDeque::new(),
            last_inner: Rect::default(),
            viewport_start: 0,
        }
    }

    pub fn set_channel(&mut self, messages: Vec<SlackMessage>, channel_name: String) {
        self.messages = messages;
        self.channel_name = channel_name;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.active_thread = None;
        self.read_marker_ts = None;
        self.line_cache = None;
        self.scroll_to_selected = true;
    }

    pub fn refresh_messages(&mut self, messages: Vec<SlackMessage>) {
        self.messages = messages;
        let clamped = self
            .selected_message
            .min(self.message_count().saturating_sub(1));
        if clamped != self.selected_message {
            // The list shrank under the selection — follow it so the
            // highlight doesn't sit outside the viewport.
            self.selected_message = clamped;
            self.scroll_to_selected = true;
        }
        self.line_cache = None;
    }

    pub fn set_thread(&mut self, thread_ts: String, messages: Vec<SlackMessage>) {
        self.active_thread = Some(thread_ts);
        self.messages = messages;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.line_cache = None;
        self.scroll_to_selected = true;
    }

    pub fn close_thread(&mut self) {
        self.active_thread = None;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.line_cache = None;
        self.scroll_to_selected = true;
    }

    pub fn set_read_marker_ts(&mut self, read_marker_ts: Option<String>) {
        self.read_marker_ts = read_marker_ts;
        self.line_cache = None;
    }

    pub fn read_marker_ts(&self) -> Option<&str> {
        self.read_marker_ts.as_deref()
    }

    /// Number of messages in display order.
    fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Default selection: newest message (bottom of view).
    /// Channel view is reversed (newest = index 0 in storage,
    /// but last in display order). Thread view is oldest-first.
    fn default_selection(&self) -> usize {
        self.message_count().saturating_sub(1)
    }

    fn select_prev_message(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        self.selected_message = self.selected_message.saturating_sub(1);
        self.scroll_to_selected = true;
    }

    fn select_next_message(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        let max = self.message_count().saturating_sub(1);
        self.selected_message = (self.selected_message + 1).min(max);
        self.scroll_to_selected = true;
    }

    fn half_page(&self) -> u16 {
        (self.viewport_rows / 2).max(1)
    }

    fn full_page(&self) -> u16 {
        self.viewport_rows.max(1)
    }

    /// Left click: select the message under the cursor.  The clicked
    /// row is visible by definition, so the selection-follow scroll is
    /// deliberately not armed.
    pub fn handle_click(&mut self, position: Position) {
        if !self.last_inner.contains(position) {
            return;
        }
        let row = usize::from(position.y - self.last_inner.y);
        let Some((_, lines)) = &self.line_cache else {
            return;
        };
        let Some(vline) = lines.get(self.viewport_start + row) else {
            return;
        };
        if vline.msg_index != usize::MAX {
            self.selected_message = vline.msg_index;
        }
    }

    /// Mouse wheel: free line scrolling, like j/k.
    pub fn handle_scroll(&mut self, up: bool) {
        const WHEEL_LINES: u16 = 3;
        self.scroll_offset = if up {
            self.scroll_offset.saturating_add(WHEEL_LINES)
        } else {
            self.scroll_offset.saturating_sub(WHEEL_LINES)
        };
    }

    pub fn invalidate_cache(&mut self) {
        self.line_cache = None;
    }

    /// Cache a decoded image preview, evicting the oldest entries past
    /// [`MAX_FILE_IMAGES`].  Returns the evicted keys so the caller can
    /// clear its requested-set and let them be re-fetched on revisit.
    ///
    /// Keys still referenced by the loaded messages are never evicted —
    /// evicting a visible preview would make the next poll re-download
    /// it, evicting another visible one, looping forever.  The cache
    /// may therefore exceed the cap, bounded by the images in view.
    pub fn insert_file_image(
        &mut self,
        image_key: String,
        protocol: StatefulProtocol,
    ) -> Vec<String> {
        if self
            .file_image_protocols
            .insert(image_key.clone(), protocol)
            .is_none()
        {
            self.file_image_order.push_back(image_key);
        }

        let mut evicted = Vec::new();
        let excess = self
            .file_image_protocols
            .len()
            .saturating_sub(MAX_FILE_IMAGES);
        if excess > 0 {
            let referenced: std::collections::HashSet<String> = self
                .messages
                .iter()
                .flat_map(|message| message.files.iter())
                .filter_map(SlackFile::image_key)
                .collect();
            let mut kept = VecDeque::with_capacity(self.file_image_order.len());
            for key in std::mem::take(&mut self.file_image_order) {
                if evicted.len() < excess && !referenced.contains(&key) {
                    self.file_image_protocols.remove(&key);
                    evicted.push(key);
                } else {
                    kept.push_back(key);
                }
            }
            self.file_image_order = kept;
        }
        self.line_cache = None;
        evicted
    }

    /// The currently selected message.  In channel view messages are
    /// stored newest-first but displayed oldest-first, so we
    /// reverse-index.  In thread view order matches.
    fn selected_display_message(&self) -> Option<&SlackMessage> {
        if self.active_thread.is_some() {
            self.messages.get(self.selected_message)
        } else {
            // display is reversed: display 0 = last storage element
            self.messages
                .len()
                .checked_sub(self.selected_message + 1)
                .and_then(|idx| self.messages.get(idx))
        }
    }

    /// Get the `thread_ts` for the currently selected message.
    pub fn selected_message_thread_ts(&self) -> Option<&str> {
        let msg = self.selected_display_message()?;
        if msg.reply_count.unwrap_or(0) > 0 {
            Some(msg.ts.as_str())
        } else {
            msg.thread_ts.as_deref()
        }
    }

    /// Files attached to the currently selected message.
    pub fn selected_message_files(&self) -> &[SlackFile] {
        self.selected_display_message()
            .map_or(&[][..], |msg| msg.files.as_slice())
    }

    fn get_or_build_lines(&mut self, text_width: usize) -> &[VisualLine] {
        if self
            .line_cache
            .as_ref()
            .is_none_or(|(w, _)| *w != text_width)
        {
            let lines = self.build_lines(text_width);
            self.line_cache = Some((text_width, lines));
        }
        self.line_cache.as_ref().map_or(&[][..], |(_, v)| v)
    }

    fn build_lines(&self, text_width: usize) -> Vec<VisualLine> {
        // Channel history is newest-first — reverse for display.
        // Thread replies are already oldest-first — use as-is.
        let display: Vec<&SlackMessage> = if self.active_thread.is_some() {
            self.messages.iter().collect()
        } else {
            self.messages.iter().rev().collect()
        };
        let mut lines: Vec<VisualLine> = Vec::new();
        let mut prev_user: Option<&str> = None;
        let unread_count = self.unread_count(&display);
        let first_unread_index = self.first_unread_index(&display);

        for (i, msg) in display.iter().enumerate() {
            let starts_unread_section = first_unread_index == Some(i);
            if starts_unread_section {
                let separator = unread_separator_line(unread_count, text_width);
                lines.push(VisualLine::separator(separator));
                prev_user = None;
            }

            let sender = msg.sender_id();
            let user_changed = prev_user != Some(sender);

            if user_changed {
                let ts = format_slack_ts(&msg.ts);
                // For bot messages, prefer username as display name
                // since bot_id is an opaque identifier.
                let bot_name = if msg.username.is_empty() {
                    None
                } else {
                    Some(msg.username.as_str())
                };
                let display_name = self
                    .user_cache
                    .get(sender)
                    .map(String::as_str)
                    .or(bot_name)
                    .unwrap_or(sender);

                if i > 0 && !starts_unread_section {
                    // Separator belongs to the *new* message so it
                    // highlights together with the header.
                    lines.push(VisualLine::text(Line::from(""), i));
                }

                lines.push(VisualLine::header(
                    Line::from(vec![
                        Span::from(format!("[{ts}] ")).dim(),
                        Span::from(display_name.to_string()).bold().cyan(),
                    ]),
                    sender.to_string(),
                    i,
                ));
            }
            prev_user = Some(sender);

            if !msg.text.is_empty() {
                let rendered_text = replace_user_mentions(&msg.text, |user_id| {
                    self.user_cache.get(user_id).cloned()
                });
                for wrapped in wrap_text(&rendered_text, text_width) {
                    lines.push(VisualLine::text(Line::from(Span::from(wrapped)), i));
                }
            }

            for file in &msg.files {
                let size = format_file_size(file.size);
                let kind = if file.is_image() { "image" } else { "file" };
                let name = file.display_name();
                let label = if name.is_empty() {
                    format!("[{kind}] ({size})")
                } else {
                    format!("[{kind}] {name} ({size})")
                };
                for wrapped in wrap_text(&label, text_width) {
                    lines.push(VisualLine::text(
                        Line::from(Span::from(wrapped).fg(Color::Rgb(130, 170, 210))),
                        i,
                    ));
                }

                if let Some(image_key) = ready_image_key(file, &self.file_image_protocols) {
                    lines.push(VisualLine::image(image_key, i));
                    for _ in 1..IMAGE_PREVIEW_HEIGHT {
                        lines.push(VisualLine::text(Line::from(""), i));
                    }
                }
            }

            if !msg.reactions.is_empty() {
                for line in format_reactions(&msg.reactions, &self.custom_emoji_urls, text_width) {
                    lines.push(VisualLine::text(line, i));
                }
            }

            if self.active_thread.is_none()
                && let Some(count) = msg.reply_count
                && count > 0
            {
                let label = if count == 1 {
                    "\u{21B3} 1 reply".to_string()
                } else {
                    format!("\u{21B3} {count} replies")
                };
                lines.push(VisualLine::text(
                    Line::from(Span::from(label).fg(Color::Blue)),
                    i,
                ));
            }
        }

        lines
    }

    fn unread_count(&self, display: &[&SlackMessage]) -> usize {
        display
            .iter()
            .filter(|message| self.is_unread(message))
            .count()
    }

    fn first_unread_index(&self, display: &[&SlackMessage]) -> Option<usize> {
        display.iter().position(|message| self.is_unread(message))
    }

    fn is_unread(&self, message: &SlackMessage) -> bool {
        self.read_marker_ts
            .as_deref()
            .is_some_and(|read_marker_ts| message.ts.as_str() > read_marker_ts)
    }
}

struct VisualLine {
    line: Line<'static>,
    show_avatar: bool,
    user_id: String,
    image_key: Option<String>,
    /// Display-order message index this line belongs to.
    msg_index: usize,
}

impl VisualLine {
    fn text(line: Line<'static>, msg_index: usize) -> Self {
        Self {
            line,
            show_avatar: false,
            user_id: String::new(),
            image_key: None,
            msg_index,
        }
    }

    fn header(line: Line<'static>, user_id: String, msg_index: usize) -> Self {
        Self {
            line,
            show_avatar: true,
            user_id,
            image_key: None,
            msg_index,
        }
    }

    fn separator(line: Line<'static>) -> Self {
        Self {
            line,
            show_avatar: false,
            user_id: String::new(),
            image_key: None,
            msg_index: usize::MAX,
        }
    }

    fn image(image_key: String, msg_index: usize) -> Self {
        Self {
            line: Line::from(""),
            show_avatar: false,
            user_id: String::new(),
            image_key: Some(image_key),
            msg_index,
        }
    }
}

impl Component for MessageList {
    fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            // -- Line-level scroll --
            (KeyCode::Char('k') | KeyCode::Up, false) => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                EventResult::Consumed
            }
            (KeyCode::Char('j') | KeyCode::Down, false) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                EventResult::Consumed
            }
            (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => {
                self.scroll_offset = self.scroll_offset.saturating_add(self.half_page());
                EventResult::Consumed
            }
            (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(self.half_page());
                EventResult::Consumed
            }
            (KeyCode::Char('b'), true) => {
                self.scroll_offset = self.scroll_offset.saturating_add(self.full_page());
                EventResult::Consumed
            }
            (KeyCode::Char('f'), true) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(self.full_page());
                EventResult::Consumed
            }
            (KeyCode::Char('g'), false) => {
                self.scroll_offset = u16::MAX;
                EventResult::Consumed
            }
            (KeyCode::Char('G'), false) => {
                self.scroll_offset = 0;
                EventResult::Consumed
            }

            // -- Message-level navigation (Shift+j / Shift+k) --
            (KeyCode::Char('K'), false) => {
                self.select_prev_message();
                EventResult::Consumed
            }
            (KeyCode::Char('J'), false) => {
                self.select_next_message();
                EventResult::Consumed
            }

            // -- Download files on the selected message --
            (KeyCode::Char('d'), false) => EventResult::Action(Action::DownloadFiles),

            // -- Thread: enter with l / → / Enter --
            (KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter, false) => {
                if let Some(ts) = self.selected_message_thread_ts().map(String::from) {
                    EventResult::Action(Action::OpenThread(ts))
                } else {
                    EventResult::Consumed
                }
            }

            // -- Thread: close with h / ← / Esc --
            (KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc, false) => {
                if self.active_thread.is_some() {
                    EventResult::Action(Action::CloseThread)
                } else {
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            Style::default().cyan()
        } else {
            Style::default().dim()
        };

        let title = if self.active_thread.is_some() {
            format!("Thread in #{}", self.channel_name)
        } else if self.channel_name.is_empty() {
            String::new()
        } else {
            format!("#{}", self.channel_name)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title.bold().cyan());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.last_inner = inner;

        if self.messages.is_empty() {
            return;
        }

        let show_avatars = !self.avatar_protocols.is_empty();
        let avatar_col = if show_avatars { AVATAR_WIDTH } else { 0 };
        let content_width = inner.width.saturating_sub(MSG_RIGHT_PAD);
        let text_width = content_width.saturating_sub(avatar_col) as usize;
        let visible_rows = inner.height as usize;
        self.viewport_rows = inner.height;

        // Build / retrieve cached lines.  Scroll the selected message
        // into view only when a selection change requested it —
        // unconditional snapping would override the free line-scroll
        // keys on every frame.
        let total = self.get_or_build_lines(text_width).len();
        if self.scroll_to_selected {
            self.scroll_to_selected = false;
            let lines = self.line_cache.as_ref().map_or(&[][..], |(_, v)| v);
            self.scroll_offset = ensure_selected_visible(
                lines,
                self.selected_message,
                self.scroll_offset,
                visible_rows,
            );
        }

        // Clamp scroll offset
        let max_offset = total.saturating_sub(visible_rows);
        let offset = (self.scroll_offset as usize).min(max_offset);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to visual line count"
        )]
        {
            self.scroll_offset = offset as u16;
        }

        // Scroll: offset 0 = bottom (newest), higher = further back
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(visible_rows);
        self.viewport_start = start;

        let selected = self.selected_message;

        // Render visible lines (need split borrow: cache vs avatar_protocols)
        let cached = self.line_cache.as_ref();
        let lines = cached.map_or(&[][..], |(_, v)| v.as_slice());

        for (i, vline) in lines[start..end].iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "bounded by terminal height"
            )]
            let y = inner.y + (i as u16);
            if y >= inner.y + inner.height {
                break;
            }

            let is_selected = vline.msg_index == selected;
            let row = Rect::new(inner.x, y, content_width, 1);

            let image_area = if show_avatars {
                let [avatar_area, text_area] =
                    Layout::horizontal([Constraint::Length(avatar_col), Constraint::Fill(1)])
                        .areas(row);

                if vline.show_avatar
                    && let Some(protocol) = self.avatar_protocols.get_mut(&vline.user_id)
                {
                    let img = StatefulImage::default();
                    frame.render_stateful_widget(img, avatar_area, protocol);
                }

                render_visual_line_text(frame, text_area, &vline.line, is_selected);
                image_preview_area(text_area, inner)
            } else {
                render_visual_line_text(frame, row, &vline.line, is_selected);
                image_preview_area(row, inner)
            };

            fill_selected_row_background(frame, inner, y, is_selected);
            render_file_image_preview(
                frame,
                image_area,
                vline.image_key.as_deref(),
                &mut self.file_image_protocols,
            );
        }

        render_scrollbar(frame, area, total, visible_rows, offset);
    }
}

fn render_visual_line_text(frame: &mut Frame, area: Rect, line: &Line<'static>, is_selected: bool) {
    let mut paragraph = Paragraph::new(line.clone());
    if is_selected {
        paragraph = paragraph.style(Style::default().bg(SELECTED_MSG_BG));
    }
    frame.render_widget(paragraph, area);
}

fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total: usize,
    visible_rows: usize,
    offset: usize,
) {
    if total <= visible_rows {
        return;
    }

    let mut scrollbar_state = ScrollbarState::new(total).position(offset);
    let scrollbar = Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn fill_selected_row_background(frame: &mut Frame, inner: Rect, y: u16, is_selected: bool) {
    if !is_selected {
        return;
    }

    let row = Rect::new(inner.x, y, inner.width, 1);
    let buf = frame.buffer_mut();
    for x in row.left()..row.right() {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_bg(SELECTED_MSG_BG);
        }
    }
}

fn render_file_image_preview(
    frame: &mut Frame,
    area: Option<Rect>,
    image_key: Option<&str>,
    image_protocols: &mut HashMap<String, StatefulProtocol>,
) {
    let (Some(area), Some(image_key)) = (area, image_key) else {
        return;
    };
    let Some(protocol) = image_protocols.get_mut(image_key) else {
        return;
    };

    frame.render_stateful_widget(
        StatefulImage::new().resize(Resize::Fit(None)),
        area,
        protocol,
    );
}

fn ready_image_key(
    file: &SlackFile,
    image_protocols: &HashMap<String, StatefulProtocol>,
) -> Option<String> {
    let image_key = file.image_key()?;
    if file.is_image() && image_protocols.contains_key(&image_key) {
        Some(image_key)
    } else {
        None
    }
}

fn image_preview_area(row: Rect, bounds: Rect) -> Option<Rect> {
    if row.width == 0 || row.y >= bounds.y.saturating_add(bounds.height) {
        return None;
    }

    let bottom = bounds.y.saturating_add(bounds.height);
    let height = IMAGE_PREVIEW_HEIGHT.min(bottom.saturating_sub(row.y));
    if height == 0 {
        return None;
    }

    Some(Rect::new(
        row.x,
        row.y,
        row.width.min(IMAGE_PREVIEW_WIDTH),
        height,
    ))
}

/// Wrap text to fit within the given display-column width.
///
/// Uses Unicode display width so that emoji, CJK, and other wide
/// characters are measured correctly.  Words longer than `width` are
/// broken at character boundaries.  Leading indentation and
/// consecutive spaces are preserved.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        wrap_paragraph(paragraph, width, &mut lines);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Wrap a single paragraph (no embedded newlines) preserving
/// whitespace runs.  Breaks prefer space boundaries; falls back to
/// character breaking for words wider than `width`.
fn wrap_paragraph(text: &str, width: usize, out: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_width: usize = 0;

    for segment in split_segments(text) {
        let mut remainder = segment;
        let is_space = segment.starts_with([' ', '\t']);

        while !remainder.is_empty() {
            let seg_width = UnicodeWidthStr::width(remainder);

            if current_width + seg_width <= width {
                current.push_str(remainder);
                current_width += seg_width;
                break;
            }

            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current_width = 0;
                continue;
            }

            let (prefix, rest, prefix_width) = split_to_width(remainder, width);
            current.push_str(prefix);
            current_width = prefix_width;
            remainder = rest;

            if !remainder.is_empty() || !is_space {
                out.push(std::mem::take(&mut current));
                current_width = 0;
            }
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
}

/// Split a string into alternating runs of whitespace and
/// non-whitespace, preserving the original characters.
fn split_segments(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(start, ch)) = chars.peek() {
        let is_ws = ch == ' ' || ch == '\t';
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if (c == ' ' || c == '\t') != is_ws {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        result.push(&s[start..end]);
    }
    result
}

/// Split `text` into the longest prefix that fits within `width`
/// columns plus the remaining suffix.  If the first character is
/// already wider than `width`, it is returned as a one-character
/// prefix so progress is always made.
fn split_to_width(text: &str, width: usize) -> (&str, &str, usize) {
    let mut end = 0;
    let mut used_width = 0;

    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used_width + ch_width > width {
            if end == 0 {
                let next = idx + ch.len_utf8();
                return (&text[..next], &text[next..], ch_width);
            }
            return (&text[..end], &text[end..], used_width);
        }
        end = idx + ch.len_utf8();
        used_width += ch_width;
    }

    (text, "", used_width)
}

fn unread_separator_line(unread_count: usize, width: usize) -> Line<'static> {
    let label = if unread_count == 1 {
        " New Message "
    } else {
        " New Messages "
    };
    let label_width = UnicodeWidthStr::width(label);

    if width <= label_width {
        return Line::from(Span::from(label.to_string()).fg(Color::Red));
    }

    let dash_count = width - label_width;
    let left = dash_count / 2;
    let right = dash_count - left;
    let text = format!("{}{}{}", "-".repeat(left), label, "-".repeat(right));

    Line::from(Span::from(text).fg(Color::Red))
}

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    #[expect(clippy::cast_precision_loss, reason = "display-only rounding")]
    match bytes {
        0..KB => format!("{bytes} B"),
        KB..MB => format!("{:.1} KB", bytes as f64 / KB as f64),
        MB..GB => format!("{:.1} MB", bytes as f64 / MB as f64),
        _ => format!("{:.1} GB", bytes as f64 / GB as f64),
    }
}

/// Convert Slack timestamp "1234567890.123456" to "HH:MM" in local time.
fn format_slack_ts(ts: &str) -> String {
    ts.split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .map_or_else(
            || "??:??".to_string(),
            |epoch| {
                let mut tm: libc::tm = unsafe { std::mem::zeroed() };
                let time = epoch as libc::time_t;
                // SAFETY: localtime_r is thread-safe and writes into
                // our stack-allocated `tm`.
                let ok = unsafe { libc::localtime_r(&raw const time, &raw mut tm) };
                if ok.is_null() {
                    return "??:??".to_string();
                }
                format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
            },
        )
}

/// Render reactions, wrapping to multiple lines if needed:
/// 👍 3  🔥 1  :custom: 2
fn format_reactions(
    reactions: &[Reaction],
    custom_urls: &HashMap<String, String>,
    max_width: usize,
) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for r in reactions {
        let unicode = emoji::lookup(&r.name);
        let label = match unicode {
            Some(e) => format!("{e}\u{00A0}{}", r.count),
            None => format!(":{}:\u{00A0}{}", r.name, r.count),
        };
        let label_width = UnicodeWidthStr::width(label.as_str());
        let sep_width = if current_spans.is_empty() { 0 } else { 2 };

        if current_width + sep_width + label_width > max_width && !current_spans.is_empty() {
            result.push(Line::from(current_spans));
            current_spans = Vec::new();
            current_width = 0;
        }

        if !current_spans.is_empty() {
            current_spans.push(Span::from("  "));
            current_width += 2;
        }

        let span = if unicode.is_some() {
            Span::from(label).dim()
        } else if custom_urls.contains_key(&r.name) {
            Span::from(label).fg(ratatui::style::Color::Magenta)
        } else {
            Span::from(label).dim()
        };
        current_width += label_width;
        current_spans.push(span);
    }

    if !current_spans.is_empty() {
        result.push(Line::from(current_spans));
    }
    result
}

/// Adjust `scroll_offset` so lines with `msg_index == selected` are
/// within the viewport.  Returns the (possibly updated) offset.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to visual line count"
)]
fn ensure_selected_visible(
    lines: &[VisualLine],
    selected: usize,
    scroll_offset: u16,
    visible_rows: usize,
) -> u16 {
    let total = lines.len();
    if total == 0 || visible_rows == 0 {
        return scroll_offset;
    }

    let first = lines.iter().position(|vl| vl.msg_index == selected);
    let last = lines.iter().rposition(|vl| vl.msg_index == selected);

    let (Some(first), Some(last)) = (first, last) else {
        return scroll_offset;
    };

    let max_offset = total.saturating_sub(visible_rows);
    let offset = (scroll_offset as usize).min(max_offset);
    let end = total.saturating_sub(offset);
    let start = end.saturating_sub(visible_rows);

    if first < start {
        // Header above viewport — scroll up.
        let new_end = first + visible_rows;
        total.saturating_sub(new_end) as u16
    } else if last >= end {
        // Last line below viewport — scroll down.
        total.saturating_sub(last + 1) as u16
    } else {
        scroll_offset
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::components::messages::{MessageList, wrap_text};
    use crate::slack::types::{SlackFile, SlackMessage};

    fn message(ts: &str, user: &str, text: &str) -> SlackMessage {
        SlackMessage {
            ts: ts.into(),
            user: user.into(),
            text: text.into(),
            thread_ts: None,
            reply_count: None,
            reactions: Vec::new(),
            files: Vec::new(),
            bot_id: String::new(),
            username: String::new(),
        }
    }

    #[test]
    fn wrap_text_preserves_whitespace_runs_when_wrapping() {
        let text = "  foo    bar";

        let wrapped = wrap_text(text, 4);

        assert_eq!(wrapped.concat(), text);
        assert_eq!(wrapped.first().map(String::as_str), Some("  "));
    }

    #[test]
    fn wrap_text_keeps_spaces_at_line_boundaries() {
        let text = "a    b";

        let wrapped = wrap_text(text, 3);

        assert_eq!(wrapped.concat(), text);
        assert_eq!(
            wrapped,
            vec!["a".to_string(), "   ".to_string(), " b".to_string()]
        );
    }

    #[test]
    fn selected_message_files_uses_display_order() {
        let mut list = MessageList::new();
        let mut with_file = message("2.0", "U1", "here you go");
        with_file.files.push(SlackFile {
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
        // Channel storage is newest-first: file message is newest.
        list.set_channel(
            vec![with_file, message("1.0", "U2", "old")],
            "general".into(),
        );

        // Default selection is the newest message (last in display).
        assert_eq!(list.selected_message_files().len(), 1);
        assert_eq!(list.selected_message_files()[0].name, "report.pdf");

        // Moving up selects the older message, which has no files.
        list.select_prev_message();
        assert!(list.selected_message_files().is_empty());
    }

    #[test]
    fn scroll_keys_do_not_snap_back_to_selection() {
        use crate::components::Component;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut list = MessageList::new();
        list.set_channel(
            vec![message("2.0", "U1", "new"), message("1.0", "U1", "old")],
            "general".into(),
        );
        // Opening a channel requests one follow-scroll…
        assert!(list.scroll_to_selected);
        list.scroll_to_selected = false;

        // …after which free scrolling must not re-arm it.
        list.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(list.scroll_offset, 1);
        assert!(!list.scroll_to_selected);

        // Selection changes re-arm the follow-scroll.
        list.select_prev_message();
        assert!(list.scroll_to_selected);
    }

    #[test]
    fn page_scroll_uses_viewport_height() {
        use crate::components::Component;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut list = MessageList::new();
        list.set_channel(vec![message("1.0", "U1", "hi")], "general".into());
        list.viewport_rows = 30;

        list.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(list.scroll_offset, 15);
        list.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(list.scroll_offset, 45);
    }

    #[test]
    fn click_selects_message_under_cursor() {
        use ratatui::layout::{Position, Rect};

        let mut list = MessageList::new();
        // Display order: msg 0 = "old" (U2), msg 1 = "new" (U1).
        list.set_channel(
            vec![message("2.0", "U1", "new"), message("1.0", "U2", "old")],
            "general".into(),
        );
        let _ = list.get_or_build_lines(32);
        list.last_inner = Rect::new(1, 1, 32, 10);
        list.viewport_start = 0;
        list.scroll_to_selected = false;
        assert_eq!(list.selected_message, 1); // newest selected by default

        // Row 1 is "old"'s text line (row 0 is its header).
        list.handle_click(Position::new(2, 2));

        assert_eq!(list.selected_message, 0);
        // The clicked row is visible — no follow-scroll snap.
        assert!(!list.scroll_to_selected);

        // Clicks outside the inner area are ignored.
        list.handle_click(Position::new(0, 0));
        assert_eq!(list.selected_message, 0);
    }

    #[test]
    fn wheel_scroll_moves_offset_without_snap() {
        let mut list = MessageList::new();
        list.set_channel(vec![message("1.0", "U1", "hi")], "general".into());
        list.scroll_to_selected = false;

        list.handle_scroll(true);
        assert_eq!(list.scroll_offset, 3);
        assert!(!list.scroll_to_selected);

        list.handle_scroll(false);
        assert_eq!(list.scroll_offset, 0);
        // Clamped at the bottom.
        list.handle_scroll(false);
        assert_eq!(list.scroll_offset, 0);
    }

    #[test]
    fn insert_file_image_evicts_oldest_beyond_cap() {
        let mut list = MessageList::new();
        let picker = ratatui_image::picker::Picker::halfblocks();

        for i in 0..=super::MAX_FILE_IMAGES {
            let img = image::DynamicImage::new_rgb8(1, 1);
            let evicted = list.insert_file_image(format!("k{i}"), picker.new_resize_protocol(img));
            if i < super::MAX_FILE_IMAGES {
                assert!(evicted.is_empty());
            } else {
                assert_eq!(evicted, vec!["k0".to_string()]);
            }
        }
        assert_eq!(list.file_image_protocols.len(), super::MAX_FILE_IMAGES);
        assert!(!list.file_image_protocols.contains_key("k0"));
    }

    #[test]
    fn file_size_formatting() {
        use super::format_file_size;

        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1_048_576), "1.0 MB");
        assert_eq!(format_file_size(2_621_440), "2.5 MB");
        assert_eq!(format_file_size(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn build_lines_replaces_user_mentions_with_cached_names() {
        let mut list = MessageList::new();
        list.user_cache = HashMap::from([(String::from("U2"), String::from("Alice"))]);
        list.messages = vec![message("1.0", "U1", "hello <@U2>")];

        let lines = list.build_lines(80);

        assert!(
            lines
                .iter()
                .any(|line| line.line.to_string() == "hello @Alice")
        );
    }

    #[test]
    fn build_lines_inserts_unread_separator_before_first_unread_message() {
        let mut list = MessageList::new();
        // Channel storage is newest-first; display order becomes 1, 2, 3.
        list.messages = vec![
            message("3.0", "U1", "newer"),
            message("2.0", "U1", "new"),
            message("1.0", "U1", "old"),
        ];
        list.set_read_marker_ts(Some("1.0".to_string()));

        let lines = list.build_lines(32);
        let separator = lines
            .iter()
            .position(|line| line.line.to_string().contains("New Messages"));
        let new_header = separator.and_then(|sep| {
            lines
                .iter()
                .enumerate()
                .skip(sep + 1)
                .find(|(_, line)| line.line.to_string().contains("U1"))
                .map(|(idx, _)| idx)
        });
        let new_text = lines.iter().position(|line| line.line.to_string() == "new");

        assert!(separator.is_some());
        assert!(new_header.is_some_and(|idx| separator.is_some_and(|sep| idx > sep)));
        assert!(new_text.is_some_and(|idx| new_header.is_some_and(|header| idx > header)));
    }

    #[test]
    fn build_lines_uses_singular_unread_separator_for_one_message() {
        let mut list = MessageList::new();
        list.messages = vec![message("2.0", "U1", "new"), message("1.0", "U2", "old")];
        list.set_read_marker_ts(Some("1.0".to_string()));

        let lines = list.build_lines(32);

        assert!(
            lines
                .iter()
                .any(|line| line.line.to_string().contains("New Message"))
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.line.to_string().contains("New Messages"))
        );
    }
}
