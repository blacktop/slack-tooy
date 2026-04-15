use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui_image::StatefulImage;
use ratatui_image::protocol::StatefulProtocol;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::action::Action;
use crate::components::{Component, EventResult};
use crate::emoji;
use crate::slack::types::{Reaction, SlackMessage};

const AVATAR_WIDTH: u16 = 4;
/// Right-side padding inside the messages panel (matches the 1-column
/// gap the left border already provides).
const MSG_RIGHT_PAD: u16 = 1;

/// Subtle background for the selected message highlight.
const SELECTED_MSG_BG: Color = Color::Rgb(40, 40, 50);

pub struct MessageList {
    pub messages: Vec<SlackMessage>,
    pub channel_name: String,
    pub scroll_offset: u16,
    /// Index into the *display-order* message list (0 = top-most).
    pub selected_message: usize,
    pub user_cache: HashMap<String, String>,
    pub avatar_protocols: HashMap<String, StatefulProtocol>,
    pub custom_emoji_urls: HashMap<String, String>,
    pub active_thread: Option<String>,
    line_cache: Option<(usize, Vec<VisualLine>)>,
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
            custom_emoji_urls: HashMap::new(),
            active_thread: None,
            line_cache: None,
        }
    }

    pub fn set_channel(&mut self, messages: Vec<SlackMessage>, channel_name: String) {
        self.messages = messages;
        self.channel_name = channel_name;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.active_thread = None;
        self.line_cache = None;
    }

    pub fn refresh_messages(&mut self, messages: Vec<SlackMessage>) {
        self.messages = messages;
        self.selected_message = self
            .selected_message
            .min(self.message_count().saturating_sub(1));
        self.line_cache = None;
    }

    pub fn set_thread(&mut self, thread_ts: String, messages: Vec<SlackMessage>) {
        self.active_thread = Some(thread_ts);
        self.messages = messages;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.line_cache = None;
    }

    pub fn close_thread(&mut self) {
        self.active_thread = None;
        self.scroll_offset = 0;
        self.selected_message = self.default_selection();
        self.line_cache = None;
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
    }

    fn select_next_message(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        let max = self.message_count().saturating_sub(1);
        self.selected_message = (self.selected_message + 1).min(max);
    }

    pub fn invalidate_cache(&mut self) {
        self.line_cache = None;
    }

    /// Get the `thread_ts` for the currently selected message.
    /// In channel view messages are stored newest-first but displayed
    /// oldest-first, so we reverse-index.  In thread view order matches.
    pub fn selected_message_thread_ts(&self) -> Option<&str> {
        let display_idx = self.selected_message;
        let msg = if self.active_thread.is_some() {
            self.messages.get(display_idx)
        } else {
            // display is reversed: display 0 = last storage element
            let len = self.messages.len();
            if display_idx < len {
                self.messages.get(len - 1 - display_idx)
            } else {
                None
            }
        }?;
        if msg.reply_count.unwrap_or(0) > 0 {
            Some(msg.ts.as_str())
        } else {
            msg.thread_ts.as_deref()
        }
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

        for (i, msg) in display.iter().enumerate() {
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

                if i > 0 {
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
                for wrapped in wrap_text(&msg.text, text_width) {
                    lines.push(VisualLine::text(Line::from(Span::from(wrapped)), i));
                }
            }

            for file in &msg.files {
                let size = format_file_size(file.size);
                let label = if file.name.is_empty() {
                    format!("[file] ({size})")
                } else {
                    format!("[file] {} ({size})", file.name)
                };
                for wrapped in wrap_text(&label, text_width) {
                    lines.push(VisualLine::text(
                        Line::from(Span::from(wrapped).fg(Color::Rgb(130, 170, 210))),
                        i,
                    ));
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
}

struct VisualLine {
    line: Line<'static>,
    show_avatar: bool,
    user_id: String,
    /// Display-order message index this line belongs to.
    msg_index: usize,
}

impl VisualLine {
    fn text(line: Line<'static>, msg_index: usize) -> Self {
        Self {
            line,
            show_avatar: false,
            user_id: String::new(),
            msg_index,
        }
    }

    fn header(line: Line<'static>, user_id: String, msg_index: usize) -> Self {
        Self {
            line,
            show_avatar: true,
            user_id,
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
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                EventResult::Consumed
            }
            (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                EventResult::Consumed
            }
            (KeyCode::Char('b'), true) => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
                EventResult::Consumed
            }
            (KeyCode::Char('f'), true) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
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

        if self.messages.is_empty() {
            return;
        }

        let show_avatars = !self.avatar_protocols.is_empty();
        let avatar_col = if show_avatars { AVATAR_WIDTH } else { 0 };
        let content_width = inner.width.saturating_sub(MSG_RIGHT_PAD);
        let text_width = content_width.saturating_sub(avatar_col) as usize;
        let visible_rows = inner.height as usize;

        // Build / retrieve cached lines, then ensure the selected
        // message is scrolled into view.
        let total = self.get_or_build_lines(text_width).len();
        {
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

        let selected = self.selected_message;
        let highlight_bg = Style::default().bg(SELECTED_MSG_BG);

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

            if show_avatars {
                let row = Rect::new(inner.x, y, content_width, 1);
                let [avatar_area, text_area] =
                    Layout::horizontal([Constraint::Length(avatar_col), Constraint::Fill(1)])
                        .areas(row);

                if vline.show_avatar
                    && let Some(protocol) = self.avatar_protocols.get_mut(&vline.user_id)
                {
                    let img = StatefulImage::default();
                    frame.render_stateful_widget(img, avatar_area, protocol);
                }

                let mut para = Paragraph::new(vline.line.clone());
                if is_selected {
                    para = para.style(highlight_bg);
                }
                frame.render_widget(para, text_area);
            } else {
                let row = Rect::new(inner.x, y, content_width, 1);
                let mut para = Paragraph::new(vline.line.clone());
                if is_selected {
                    para = para.style(highlight_bg);
                }
                frame.render_widget(para, row);
            }

            // Fill the full row background for selected messages so
            // the highlight extends to the right edge.
            if is_selected {
                let row = Rect::new(inner.x, y, inner.width, 1);
                let buf = frame.buffer_mut();
                for x in row.left()..row.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_bg(SELECTED_MSG_BG);
                    }
                }
            }
        }

        // Scrollbar
        if total > visible_rows {
            let mut scrollbar_state = ScrollbarState::new(total).position(offset);
            let scrollbar = Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }
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
            None => format!(":{}\u{00A0}{}", r.name, r.count),
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
    use crate::components::messages::wrap_text;

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
    fn file_size_formatting() {
        use super::format_file_size;

        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1_048_576), "1.0 MB");
        assert_eq!(format_file_size(2_621_440), "2.5 MB");
        assert_eq!(format_file_size(1_073_741_824), "1.0 GB");
    }
}
