use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::components::{Component, EventResult};
use crate::slack::types::Channel;

/// Right padding inside the sidebar so text doesn't touch the border.
const SIDEBAR_RIGHT_PAD: usize = 1;

pub struct ChannelSidebar {
    pub channels: Vec<Channel>,
    pub unread_channels: HashSet<String>,
    pub filter_unread: bool,
    /// DM channel display names keyed by user ID.
    dm_names: HashMap<String, String>,
    /// Index into `channels` (not filtered).
    pub selected: usize,
    scroll_offset: usize,
}

impl ChannelSidebar {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            unread_channels: HashSet::new(),
            filter_unread: false,
            dm_names: HashMap::new(),
            selected: 0,
            scroll_offset: 0,
        }
    }

    pub fn set_channels(&mut self, mut channels: Vec<Channel>) {
        channels.sort_by(|a, b| {
            let a_dm = a.is_im.unwrap_or(false);
            let b_dm = b.is_im.unwrap_or(false);
            a_dm.cmp(&b_dm)
                .then_with(|| a.display_name().cmp(b.display_name()))
        });
        self.channels = channels;
        self.selected = 0;
    }

    pub fn selected_channel(&self) -> Option<&Channel> {
        self.channels.get(self.selected)
    }

    pub fn update_dm_name(&mut self, user_id: &str, display_name: &str) {
        self.dm_names
            .insert(user_id.to_string(), display_name.to_string());
    }

    pub fn channel_label(&self, ch: &Channel) -> String {
        if ch.is_im.unwrap_or(false) {
            self.dm_names
                .get(&ch.user)
                .cloned()
                .unwrap_or_else(|| ch.user.clone())
        } else {
            ch.display_name().to_string()
        }
    }

    pub fn mark_all_read(&mut self) {
        self.unread_channels.clear();
    }

    pub fn snap_selection_to_visible(&mut self) {
        let visible = self.visible_indices();
        if !visible.is_empty() && !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
    }

    /// Indices into `self.channels` that pass the current filter.
    fn visible_indices(&self) -> Vec<usize> {
        self.channels
            .iter()
            .enumerate()
            .filter(|(_, ch)| !self.filter_unread || self.unread_channels.contains(&ch.id))
            .map(|(i, _)| i)
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        #[expect(
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss,
            reason = "clamped to visible range"
        )]
        let new_pos = (pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.selected = visible[new_pos];
    }

    fn select_first(&mut self) {
        let visible = self.visible_indices();
        if let Some(&first) = visible.first() {
            self.selected = first;
        }
    }

    fn select_last(&mut self) {
        let visible = self.visible_indices();
        if let Some(&last) = visible.last() {
            self.selected = last;
        }
    }

    fn ensure_visible(&mut self, visible: &[usize], visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        let visual_row = self.visual_row_for_selected(visible);
        if visual_row < self.scroll_offset {
            self.scroll_offset = visual_row;
        } else if visual_row >= self.scroll_offset + visible_height {
            self.scroll_offset = visual_row - visible_height + 1;
        }
    }

    fn visual_row_for_selected(&self, visible: &[usize]) -> usize {
        let dm_start = visible
            .iter()
            .position(|&i| self.channels[i].is_im.unwrap_or(false));

        let mut row = 0;
        for (vi, &ci) in visible.iter().enumerate() {
            // "Direct Messages" section header adds a row.
            if Some(vi) == dm_start {
                row += 1;
            }
            if ci == self.selected {
                return row;
            }
            row += 1;
        }
        row
    }
}

impl Component for ChannelSidebar {
    fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                EventResult::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                EventResult::Consumed
            }
            KeyCode::Char('g') => {
                self.select_first();
                EventResult::Consumed
            }
            KeyCode::Char('G') => {
                self.select_last();
                EventResult::Consumed
            }
            KeyCode::Enter => EventResult::Action(Action::OpenChannel),
            KeyCode::Char('R') => EventResult::Action(Action::MarkAllRead),
            KeyCode::Char('u') => EventResult::Action(Action::ToggleUnreadFilter),
            _ => EventResult::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            ratatui::style::Style::default().cyan()
        } else {
            ratatui::style::Style::default().dim()
        };

        let title = if self.filter_unread {
            " Unread Only ".bold().yellow()
        } else {
            " Channels ".bold().cyan()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let visible = self.visible_indices();
        let visible_height = inner.height as usize;
        self.ensure_visible(&visible, visible_height);

        let dm_start = visible
            .iter()
            .position(|&i| self.channels[i].is_im.unwrap_or(false));

        let mut lines: Vec<Line> = Vec::new();

        // Available columns for channel text (inside border, minus
        // right padding).
        let max_text_width = (inner.width as usize).saturating_sub(SIDEBAR_RIGHT_PAD);

        for (vi, &ci) in visible.iter().enumerate() {
            if Some(vi) == dm_start {
                lines.push(Line::from(Span::from(" Direct Messages").bold().dim()));
            }

            let ch = &self.channels[ci];
            let is_dm = ch.is_im.unwrap_or(false);
            let prefix = if is_dm { "  " } else { " # " };
            let label = self.channel_label(ch);
            let selected = ci == self.selected;
            let has_unread = self.unread_channels.contains(&ch.id);

            // Gutter: ">" for selected, " " otherwise — 1 column.
            let gutter_width = 1;
            // Unread indicator " *" takes 2 columns — but the
            // selected branch never emits it, so don't charge it.
            let suffix_width = if has_unread && !selected { 2 } else { 0 };
            let prefix_width = UnicodeWidthStr::width(prefix);
            let label_budget =
                max_text_width.saturating_sub(gutter_width + prefix_width + suffix_width);
            let display_label = truncate_with_ellipsis(&label, label_budget);
            let text = format!("{prefix}{display_label}");

            let line = if selected {
                Line::from(vec![
                    Span::from(">").bold().cyan(),
                    Span::from(text).bold().cyan(),
                ])
            } else if has_unread {
                Line::from(vec![
                    Span::from(" "),
                    Span::from(text).bold().yellow(),
                    Span::from(" *").bold().yellow(),
                ])
            } else if is_dm {
                Line::from(vec![
                    Span::from(" "),
                    Span::from(text).fg(Color::Rgb(160, 165, 200)),
                ])
            } else {
                Line::from(vec![Span::from(" "), Span::from(text)])
            };

            lines.push(line);
        }

        if lines.is_empty() && self.filter_unread {
            lines.push(Line::from(Span::from("  All caught up!").dim()));
        }

        let end = (self.scroll_offset + visible_height).min(lines.len());
        let start = self.scroll_offset.min(end);
        let rendered: Vec<Line> = lines[start..end].to_vec();

        frame.render_widget(Paragraph::new(rendered), inner);

        let total = lines.len();
        if total > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total).position(self.scroll_offset);
            let scrollbar = Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }
}

/// Truncate `text` to fit within `max_width` display columns,
/// appending `…` when content is clipped.
fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    let text_width = UnicodeWidthStr::width(text);
    if text_width <= max_width {
        return text.to_string();
    }
    // Need at least 1 column for the ellipsis character.
    if max_width == 0 {
        return String::new();
    }
    let target = max_width.saturating_sub(1); // reserve 1 col for "…"
    let mut width = 0;
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > target {
            break;
        }
        width += cw;
        end = i + ch.len_utf8();
    }
    format!("{}…", &text[..end])
}
