use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::components::{Component, EventResult};
use crate::slack::types::Channel;
use crate::theme;

/// Right padding inside the sidebar so text doesn't touch the border.
const SIDEBAR_RIGHT_PAD: usize = 1;

pub struct ChannelSidebar {
    pub channels: Vec<Channel>,
    pub unread_channels: HashSet<String>,
    pub starred_channels: HashSet<String>,
    pub filter_unread: bool,
    collapsed_sections: HashSet<Section>,
    /// DM channel display names keyed by user ID.
    dm_names: HashMap<String, String>,
    /// Index into `channels` (not filtered).
    pub selected: usize,
    scroll_offset: usize,
    /// Inner (borderless) area of the last render, for mouse
    /// hit-testing.  Zero-sized before the first render.
    last_inner: Rect,
}

/// What a rendered sidebar row points at — built by `visible_rows`,
/// which is the single source of truth for row order shared by
/// rendering, selection math, and click mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowTarget {
    Header(Section),
    Channel(usize),
}

impl ChannelSidebar {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            unread_channels: HashSet::new(),
            starred_channels: HashSet::new(),
            filter_unread: false,
            collapsed_sections: HashSet::from([Section::Channels, Section::DirectMessages]),
            dm_names: HashMap::new(),
            selected: 0,
            scroll_offset: 0,
            last_inner: Rect::default(),
        }
    }

    pub fn set_channels(&mut self, mut channels: Vec<Channel>) {
        let starred = &self.starred_channels;
        channels.sort_by(|a, b| {
            (home_section(starred, a) as u8)
                .cmp(&(home_section(starred, b) as u8))
                .then_with(|| a.display_name().cmp(b.display_name()))
        });
        self.channels = channels;
        self.selected = 0;
    }

    pub fn set_starred(&mut self, starred: HashSet<String>) {
        self.starred_channels = starred;
        self.resort();
    }

    fn resort(&mut self) {
        let selected_id = self.selected_channel().map(|ch| ch.id.clone());
        let starred = &self.starred_channels;
        self.channels.sort_by(|a, b| {
            (home_section(starred, a) as u8)
                .cmp(&(home_section(starred, b) as u8))
                .then_with(|| a.display_name().cmp(b.display_name()))
        });
        if let Some(ref id) = selected_id
            && let Some(idx) = self.channels.iter().position(|c| c.id == *id)
        {
            self.selected = idx;
        }
    }

    /// The section a channel is rendered under right now: `Unread`
    /// while it has unread messages, its home section otherwise.
    fn display_section(&self, ch: &Channel) -> Section {
        if self.unread_channels.contains(&ch.id) {
            Section::Unread
        } else {
            home_section(&self.starred_channels, ch)
        }
    }

    fn toggle_current_section(&mut self) {
        let Some(ch) = self.selected_channel() else {
            return;
        };
        let section = self.display_section(ch);
        self.toggle_section(section);
    }

    fn toggle_section(&mut self, section: Section) {
        if !self.collapsed_sections.remove(&section) {
            self.collapsed_sections.insert(section);
            self.snap_selection_to_visible();
        }
    }

    fn expand_all_sections(&mut self) {
        self.collapsed_sections.clear();
    }

    pub fn selected_channel(&self) -> Option<&Channel> {
        self.channels.get(self.selected)
    }

    pub fn update_dm_name(&mut self, user_id: &str, display_name: &str) {
        self.dm_names
            .insert(user_id.to_string(), display_name.to_string());
    }

    pub fn channel_label<'a>(&'a self, ch: &'a Channel) -> &'a str {
        if ch.is_im == Some(true) {
            self.dm_names
                .get(&ch.user)
                .map_or(ch.user.as_str(), String::as_str)
        } else {
            ch.display_name()
        }
    }

    /// Select a known conversation even when its section is collapsed or the
    /// unread filter currently hides it.
    pub fn select_channel(&mut self, channel_id: &str) -> bool {
        let Some(index) = self.channels.iter().position(|ch| ch.id == channel_id) else {
            return false;
        };
        let Some(channel) = self.channels.get(index) else {
            return false;
        };
        let section = self.display_section(channel);
        self.collapsed_sections.remove(&section);
        if !self.unread_channels.contains(channel_id) {
            self.filter_unread = false;
        }
        self.selected = index;
        true
    }

    pub fn mark_all_read(&mut self) {
        self.unread_channels.clear();
        // Channels leave the Unread section for their home section; if
        // that section is collapsed the selection would vanish.
        self.snap_selection_to_visible();
    }

    pub fn snap_selection_to_visible(&mut self) {
        let visible = self.visible_indices();
        if !visible.is_empty() && !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
    }

    /// Indices into `self.channels` that pass the current filter, in
    /// display order (grouped by section).  Vec order alone is wrong
    /// here: unread channels render at the top regardless of where
    /// they sit in the sorted list, and j/k must follow the screen.
    fn visible_indices(&self) -> Vec<usize> {
        let mut indices = Vec::with_capacity(self.channels.len());
        for &section in &SECTIONS {
            if self.collapsed_sections.contains(&section) {
                continue;
            }
            for (i, ch) in self.channels.iter().enumerate() {
                if self.filter_unread && !self.unread_channels.contains(&ch.id) {
                    continue;
                }
                if self.display_section(ch) == section {
                    indices.push(i);
                }
            }
        }
        indices
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

    /// Does this section have any channels (regardless of filters)?
    fn section_exists(&self, section: Section) -> bool {
        self.channels
            .iter()
            .any(|ch| self.display_section(ch) == section)
    }

    /// Should a section header be shown?  Yes if the section has
    /// channels and either is collapsed or has visible items.
    fn show_section_header(&self, section: Section, visible: &[usize]) -> bool {
        if !self.section_exists(section) {
            return false;
        }
        if self.collapsed_sections.contains(&section) {
            return true;
        }
        visible
            .iter()
            .any(|&i| self.display_section(&self.channels[i]) == section)
    }

    fn ensure_visible(&mut self, visible: &[usize], visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        let rows = self.visible_rows(visible);
        let visual_row = rows
            .iter()
            .position(|row| *row == RowTarget::Channel(self.selected));
        if let Some(visual_row) = visual_row {
            if visual_row < self.scroll_offset {
                self.scroll_offset = visual_row;
            } else if visual_row >= self.scroll_offset + visible_height {
                self.scroll_offset = visual_row - visible_height + 1;
            }
        }
        // The selection can be absent (filtered out, section
        // collapsed) — never leave the view scrolled past the end.
        self.scroll_offset = self
            .scroll_offset
            .min(rows.len().saturating_sub(visible_height));
    }

    /// The rendered rows in display order: section headers followed by
    /// their visible channels.
    fn visible_rows(&self, visible: &[usize]) -> Vec<RowTarget> {
        let mut rows = Vec::with_capacity(visible.len() + SECTIONS.len());
        for &section in &SECTIONS {
            if !self.show_section_header(section, visible) {
                continue;
            }
            rows.push(RowTarget::Header(section));
            for &ci in visible {
                if self.display_section(&self.channels[ci]) == section {
                    rows.push(RowTarget::Channel(ci));
                }
            }
        }
        rows
    }

    /// Left click: a channel row selects and opens it; a section
    /// header toggles collapse.  Returns the action to dispatch.
    pub fn handle_click(&mut self, position: Position) -> Option<Action> {
        if !self.last_inner.contains(position) {
            return None;
        }
        let row = usize::from(position.y - self.last_inner.y) + self.scroll_offset;
        let visible = self.visible_indices();
        match self.visible_rows(&visible).get(row)? {
            RowTarget::Header(section) => {
                self.toggle_section(*section);
                None
            }
            RowTarget::Channel(ci) => {
                self.selected = *ci;
                Some(Action::OpenChannel)
            }
        }
    }

    /// Mouse wheel: move the selection like j/k.  Scrolling the view
    /// independently would fight `ensure_visible`'s selection-follow.
    pub fn handle_scroll(&mut self, up: bool) {
        self.move_selection(if up { -1 } else { 1 });
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
            KeyCode::Char('z') => {
                self.toggle_current_section();
                EventResult::Consumed
            }
            KeyCode::Char('Z') => {
                self.expand_all_sections();
                EventResult::Consumed
            }
            KeyCode::Enter => EventResult::Action(Action::OpenChannel),
            KeyCode::Char('R') => EventResult::Action(Action::MarkAllRead),
            KeyCode::Char('u') => EventResult::Action(Action::ToggleUnreadFilter),
            _ => EventResult::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let title = if self.filter_unread {
            Line::from(vec![
                Span::styled(" unread ", Style::default().fg(theme::YELLOW).bold()),
                Span::styled(format!("{} ", self.unread_channels.len()), theme::muted()),
            ])
        } else {
            Line::from(Span::styled(
                " channels ",
                Style::default().fg(theme::TEXT).bold(),
            ))
        };

        let block = theme::panel_block(title, focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.last_inner = inner;

        let visible = self.visible_indices();
        let visible_height = inner.height as usize;
        self.ensure_visible(&visible, visible_height);

        // Available columns for channel text (inside border, minus
        // right padding).
        let max_text_width = (inner.width as usize).saturating_sub(SIDEBAR_RIGHT_PAD);

        let mut lines: Vec<Line> = Vec::new();
        for row in self.visible_rows(&visible) {
            let line = match row {
                RowTarget::Header(section) => {
                    let collapsed = self.collapsed_sections.contains(&section);
                    let marker = if collapsed { "+" } else { "-" };
                    Line::from(vec![
                        Span::styled(format!(" {marker} "), theme::muted()),
                        Span::styled(section.label(), Style::default().fg(theme::TEXT_MUTED)),
                    ])
                }
                RowTarget::Channel(ci) => {
                    let ch = &self.channels[ci];
                    let is_dm = ch.is_im == Some(true);
                    let prefix = if is_dm { " @ " } else { " # " };
                    let channel_label = self.channel_label(ch);
                    let selected = ci == self.selected;
                    let has_unread = self.unread_channels.contains(&ch.id);

                    let gutter_width = 1;
                    let suffix_width = if has_unread && !selected { 2 } else { 0 };
                    let prefix_width = UnicodeWidthStr::width(prefix);
                    let label_budget =
                        max_text_width.saturating_sub(gutter_width + prefix_width + suffix_width);
                    let display_label = truncate_with_ellipsis(channel_label, label_budget);
                    let text = format!("{prefix}{display_label}");

                    if selected {
                        Line::from(vec![
                            Span::styled(">", Style::default().fg(theme::BLUE).bold()),
                            Span::styled(text, theme::selected().bold()),
                        ])
                    } else if has_unread {
                        Line::from(vec![
                            Span::from(" "),
                            Span::styled(text, Style::default().fg(theme::YELLOW).bold()),
                            Span::styled(" *", Style::default().fg(theme::YELLOW).bold()),
                        ])
                    } else if is_dm {
                        Line::from(vec![
                            Span::from(" "),
                            Span::styled(text, Style::default().fg(theme::MAGENTA)),
                        ])
                    } else {
                        Line::from(vec![
                            Span::from(" "),
                            Span::styled(text, Style::default().fg(theme::TEXT)),
                        ])
                    }
                }
            };
            lines.push(line);
        }

        if lines.is_empty() && self.filter_unread {
            lines.push(Line::from(Span::styled("  All caught up.", theme::muted())));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Unread = 0,
    Starred = 1,
    Channels = 2,
    DirectMessages = 3,
}

impl Section {
    const fn label(self) -> &'static str {
        match self {
            Section::Unread => "* Unread",
            Section::Starred => "Starred",
            Section::Channels => "# Channels",
            Section::DirectMessages => "@ Direct messages",
        }
    }
}

const SECTIONS: [Section; 4] = [
    Section::Unread,
    Section::Starred,
    Section::Channels,
    Section::DirectMessages,
];

/// The section a channel belongs to when it has no unread messages.
/// Used as the stable sort key; unread status is applied on top at
/// display time by `display_section` so reads never reorder the list.
fn home_section(starred: &HashSet<String>, ch: &Channel) -> Section {
    if starred.contains(&ch.id) {
        Section::Starred
    } else if ch.is_im == Some(true) {
        Section::DirectMessages
    } else {
        Section::Channels
    }
}

/// Truncate `text` to fit within `max_width` display columns,
/// appending `…` when content is clipped.
pub(crate) fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use ratatui::layout::{Position, Rect};

    use crate::action::Action;
    use crate::components::sidebar::{ChannelSidebar, RowTarget, Section};
    use crate::slack::types::Channel;

    fn channel(id: &str, name: &str) -> Channel {
        Channel {
            id: id.into(),
            name: Some(name.into()),
            is_channel: Some(true),
            is_im: Some(false),
            is_member: Some(true),
            user: String::new(),
        }
    }

    fn dm(id: &str, user: &str) -> Channel {
        Channel {
            id: id.into(),
            name: None,
            is_channel: Some(false),
            is_im: Some(true),
            is_member: Some(true),
            user: user.into(),
        }
    }

    /// Sidebar with rows: [# Channels header, #alpha, #beta,
    /// @ DMs header, dm U1] rendered into a 20x10 inner area.
    fn rendered_sidebar() -> ChannelSidebar {
        let mut sidebar = ChannelSidebar::new();
        sidebar.expand_all_sections();
        sidebar.set_channels(vec![
            channel("C1", "alpha"),
            channel("C2", "beta"),
            dm("D1", "U1"),
        ]);
        sidebar.last_inner = Rect::new(1, 1, 20, 10);
        sidebar
    }

    #[test]
    fn selecting_a_notification_reveals_its_exact_hidden_conversation() {
        for id in ["C1", "D1"] {
            let mut sidebar = ChannelSidebar::new();
            sidebar.set_channels(vec![channel("C1", "alpha"), dm("D1", "U1")]);
            sidebar.filter_unread = true;
            assert_eq!(sidebar.visible_indices(), Vec::<usize>::new());
            assert!(sidebar.select_channel(id));
            assert!(sidebar.visible_indices().contains(&sidebar.selected));
            assert_eq!(
                sidebar.selected_channel().map(|ch| ch.id.as_str()),
                Some(id)
            );
            assert!(!sidebar.select_channel("missing"));
            assert_eq!(
                sidebar.selected_channel().map(|ch| ch.id.as_str()),
                Some(id)
            );
        }
    }

    #[test]
    fn click_on_channel_row_selects_and_opens() {
        let mut sidebar = rendered_sidebar();

        // Inner row 2 = second channel (#beta) after the section header.
        let action = sidebar.handle_click(Position::new(3, 3));

        assert!(matches!(action, Some(Action::OpenChannel)));
        assert_eq!(
            sidebar.selected_channel().map(|c| c.id.as_str()),
            Some("C2")
        );
    }

    #[test]
    fn click_on_header_toggles_collapse() {
        let mut sidebar = rendered_sidebar();

        // Inner row 0 = "# Channels" header.
        let action = sidebar.handle_click(Position::new(3, 1));

        assert!(action.is_none());
        assert!(sidebar.collapsed_sections.contains(&Section::Channels));
        // Rows collapsed: header, DM header, DM — clicking again expands.
        let action = sidebar.handle_click(Position::new(3, 1));
        assert!(action.is_none());
        assert!(!sidebar.collapsed_sections.contains(&Section::Channels));
    }

    #[test]
    fn click_accounts_for_scroll_offset() {
        let mut sidebar = rendered_sidebar();
        sidebar.scroll_offset = 3;

        // Inner row 0 + offset 3 = row 3 = "@ Direct messages" header.
        let action = sidebar.handle_click(Position::new(3, 1));

        assert!(action.is_none());
        assert!(
            sidebar
                .collapsed_sections
                .contains(&Section::DirectMessages)
        );
    }

    #[test]
    fn click_outside_inner_area_is_ignored() {
        let mut sidebar = rendered_sidebar();

        // (0, 0) is on the border, outside the inner rect.
        assert!(sidebar.handle_click(Position::new(0, 0)).is_none());
        assert_eq!(sidebar.selected, 0);
    }

    #[test]
    fn click_past_last_row_is_ignored() {
        let mut sidebar = rendered_sidebar();

        // Row 8 is beyond the 5 rendered rows.
        assert!(sidebar.handle_click(Position::new(3, 9)).is_none());
        assert_eq!(sidebar.selected, 0);
    }

    #[test]
    fn wheel_scroll_moves_selection() {
        let mut sidebar = rendered_sidebar();
        assert_eq!(sidebar.selected, 0);

        sidebar.handle_scroll(false);
        assert_eq!(sidebar.selected, 1);
        sidebar.handle_scroll(true);
        assert_eq!(sidebar.selected, 0);
        // Clamped at the top.
        sidebar.handle_scroll(true);
        assert_eq!(sidebar.selected, 0);
    }

    #[test]
    fn visible_rows_matches_selection_math() {
        let sidebar = rendered_sidebar();
        let visible = sidebar.visible_indices();
        let rows = sidebar.visible_rows(&visible);

        assert_eq!(
            rows,
            vec![
                RowTarget::Header(Section::Channels),
                RowTarget::Channel(0),
                RowTarget::Channel(1),
                RowTarget::Header(Section::DirectMessages),
                RowTarget::Channel(2),
            ]
        );
        let _ = visible;
    }

    #[test]
    fn unread_channels_group_under_unread_section() {
        let mut sidebar = rendered_sidebar();
        sidebar.unread_channels.insert("C2".into());
        sidebar.unread_channels.insert("D1".into());

        let visible = sidebar.visible_indices();
        let rows = sidebar.visible_rows(&visible);

        // #beta (C2) and the DM move to the top; #alpha keeps its
        // home section.
        assert_eq!(
            rows,
            vec![
                RowTarget::Header(Section::Unread),
                RowTarget::Channel(1),
                RowTarget::Channel(2),
                RowTarget::Header(Section::Channels),
                RowTarget::Channel(0),
            ]
        );
    }

    #[test]
    fn collapsed_home_sections_still_render_unread_channels_at_the_top() -> color_eyre::Result<()> {
        use color_eyre::eyre::ensure;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::components::Component;

        let mut sidebar = ChannelSidebar::new();
        sidebar.set_channels(vec![
            channel("C1", "alpha"),
            channel("C2", "beta"),
            dm("D1", "U1"),
        ]);
        sidebar.unread_channels.insert("C2".into());
        sidebar.unread_channels.insert("D1".into());

        let mut terminal = Terminal::new(TestBackend::new(32, 8))?;
        terminal.draw(|frame| sidebar.render(frame, frame.area(), false))?;
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content
            .chunks(32)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
            .collect();

        ensure!(rows[1].contains("- * Unread"));
        ensure!(rows[2].contains("# beta"));
        ensure!(rows[3].contains("@ U1"));
        ensure!(rows[4].contains("+ # Channels"));
        ensure!(!rows.iter().any(|row| row.contains("alpha")));
        Ok(())
    }

    #[test]
    fn reading_a_channel_returns_it_to_home_section() {
        let mut sidebar = rendered_sidebar();
        sidebar.unread_channels.insert("C2".into());
        sidebar.unread_channels.remove("C2");

        let visible = sidebar.visible_indices();
        let rows = sidebar.visible_rows(&visible);

        assert_eq!(
            rows,
            vec![
                RowTarget::Header(Section::Channels),
                RowTarget::Channel(0),
                RowTarget::Channel(1),
                RowTarget::Header(Section::DirectMessages),
                RowTarget::Channel(2),
            ]
        );
    }

    #[test]
    fn selection_follows_display_order_with_unread_on_top() {
        let mut sidebar = rendered_sidebar();
        sidebar.unread_channels.insert("D1".into());
        sidebar.select_first();

        // Display order: dm U1 (Unread), #alpha, #beta.
        assert_eq!(
            sidebar.selected_channel().map(|c| c.id.as_str()),
            Some("D1")
        );
        sidebar.handle_scroll(false);
        assert_eq!(
            sidebar.selected_channel().map(|c| c.id.as_str()),
            Some("C1")
        );
        sidebar.handle_scroll(false);
        assert_eq!(
            sidebar.selected_channel().map(|c| c.id.as_str()),
            Some("C2")
        );
    }

    #[test]
    fn collapsing_unread_section_hides_its_channels() {
        let mut sidebar = rendered_sidebar();
        sidebar.unread_channels.insert("C1".into());
        sidebar.selected = 0;
        sidebar.toggle_current_section();

        assert!(sidebar.collapsed_sections.contains(&Section::Unread));
        let visible = sidebar.visible_indices();
        assert!(!visible.contains(&0));
        // Selection snapped to a visible channel.
        assert_ne!(sidebar.selected, 0);
    }

    #[test]
    fn mark_all_read_snaps_selection_out_of_collapsed_home() {
        let mut sidebar = rendered_sidebar();
        sidebar.unread_channels.insert("C1".into());
        sidebar.selected = 0;
        // Collapse the home section while #alpha sits under Unread.
        sidebar.toggle_section(Section::Channels);

        sidebar.mark_all_read();

        // #alpha returned to the collapsed Channels section; selection
        // must land on something still visible.
        let visible = sidebar.visible_indices();
        assert!(visible.contains(&sidebar.selected));
    }

    #[test]
    fn ensure_visible_clamps_offset_when_selection_hidden() {
        let mut sidebar = rendered_sidebar();
        // Selection filtered out entirely (unread filter, nothing
        // unread) with a stale deep scroll.
        sidebar.filter_unread = true;
        sidebar.scroll_offset = 10;

        let visible = sidebar.visible_indices();
        sidebar.ensure_visible(&visible, 5);

        // Zero visible rows -> offset clamps to 0, view can't blank.
        assert_eq!(sidebar.scroll_offset, 0);
    }
}
