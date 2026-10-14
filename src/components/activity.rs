use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent};
use pulldown_cmark::{Event, Options, Parser, TagEnd};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::activity::{ActivityEntry, ActivityInbox, ActivitySnapshot, ThreadTarget};
use crate::components::sidebar::{ChannelSidebar, truncate_with_ellipsis};
use crate::slack::types::replace_user_mentions;
use crate::theme;

const THREAD_ROW_HEIGHT: u16 = 4;

#[derive(Default)]
pub struct ActivityView {
    pub inbox: ActivityInbox,
    pub error: Option<String>,
    pub refreshing: bool,
    pub limited: bool,
    pub unread_only: bool,
    selected: usize,
    offset: usize,
    /// Only the content of rows that were drawn can be clicked. Keep identities
    /// so a refresh between drawing and clicking cannot open a different thread.
    last_rows: Vec<(Rect, ThreadTarget)>,
}

impl ActivityView {
    fn visible(&self) -> impl Iterator<Item = &ActivityEntry> {
        self.inbox
            .entries
            .iter()
            .filter(|entry| !self.unread_only || entry.unread)
    }

    pub fn selected_entry(&self) -> Option<&ActivityEntry> {
        self.visible().nth(self.selected)
    }

    pub fn merge(&mut self, snapshot: ActivitySnapshot) -> Vec<ThreadTarget> {
        let selected = self.selected_entry().map(|item| item.target.clone());
        self.limited = !snapshot.complete;
        let alerts = self.inbox.merge(snapshot);
        let index =
            selected.and_then(|target| self.visible().position(|item| item.target == target));
        self.selected = index.unwrap_or(0);
        self.error = None;
        alerts
    }

    pub fn move_selection(&mut self, up: bool) {
        let max = self.visible().count().saturating_sub(1);
        self.selected = if up {
            self.selected.saturating_sub(1)
        } else {
            self.selected.saturating_add(1).min(max)
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(false),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(true),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self.visible().count().saturating_sub(1);
            }
            KeyCode::Char('u') => {
                self.unread_only = !self.unread_only;
                self.selected = 0;
                self.offset = 0;
            }
            KeyCode::Char('r') => return Action::RefreshActivity,
            KeyCode::Char('m') => return Action::ReadActivity,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(entry) = self.selected_entry() {
                    return Action::OpenActivityThread(entry.target.clone());
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => return Action::FocusSidebar,
            _ => {}
        }
        Action::Render
    }

    pub fn handle_click(&mut self, position: Position) -> Action {
        let target = self
            .last_rows
            .iter()
            .find(|(area, _)| area.contains(position))
            .map(|(_, target)| target.clone());
        let Some(target) = target else {
            return Action::Render;
        };
        let index = self.visible().position(|entry| entry.target == target);
        if let Some(index) = index {
            self.selected = index;
            return Action::OpenActivityThread(target);
        }
        Action::Render
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        sidebar: &ChannelSidebar,
        users: &HashMap<String, String>,
    ) {
        self.last_rows.clear();
        let count = self.visible().count();
        self.selected = self.selected.min(count.saturating_sub(1));
        let filter = if self.unread_only {
            "Unread"
        } else {
            "All threads"
        };
        let position = if count == 0 {
            0
        } else {
            self.selected.saturating_add(1)
        };
        let footer = if let Some(error) = &self.error {
            format!(" {error} · r:retry ")
        } else if self.refreshing {
            " Refreshing replies… ".into()
        } else if self.limited {
            format!(" {filter} · {position}/{count} · Recent history ")
        } else {
            format!(" {filter} · {position}/{count} ")
        };
        let mut block = theme::panel_block(Line::from(" Activity · Replies "), focused)
            .title_bottom(
                Line::from(truncate_with_ellipsis(
                    &footer,
                    usize::from(area.width.saturating_sub(4)),
                ))
                .fg(theme::TEXT_MUTED),
            );
        if area.width >= 45 {
            block = block.title_top(
                Line::from(format!(" {} unread ", self.inbox.unread_count()))
                    .fg(theme::TEXT_MUTED)
                    .right_aligned(),
            );
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        if count == 0 {
            self.offset = 0;
            let text = if self.refreshing {
                "Looking for replies to your threads…"
            } else if self.error.is_some() {
                "Activity is unavailable. Press r to retry."
            } else if self.unread_only {
                "You're caught up. Press u to show retained threads."
            } else {
                "No thread replies yet. New replies will appear here."
            };
            frame.render_widget(Paragraph::new(text).style(theme::muted()), inner);
            return;
        }

        let row_height = THREAD_ROW_HEIGHT.min(inner.height);
        let capacity = usize::from(inner.height / row_height);
        self.offset = self.offset.min(count.saturating_sub(capacity));
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(capacity) {
            self.offset = self.selected.saturating_add(1).saturating_sub(capacity);
        }

        let mut rows = Vec::with_capacity(capacity.min(count));
        let mut y = inner.y;
        for (index, entry) in self.visible().enumerate().skip(self.offset).take(capacity) {
            let row = Rect::new(inner.x, y, inner.width, row_height);
            let content = Rect {
                height: row.height.min(3),
                ..row
            };
            render_thread(
                frame,
                row,
                entry,
                index == self.selected,
                focused,
                sidebar,
                users,
            );
            rows.push((content, entry.target.clone()));
            y = y.saturating_add(row_height);
        }
        self.last_rows = rows;
    }
}

fn render_thread(
    frame: &mut Frame,
    area: Rect,
    entry: &ActivityEntry,
    selected: bool,
    focused: bool,
    sidebar: &ChannelSidebar,
    users: &HashMap<String, String>,
) {
    let body = Rect {
        height: area.height.min(3),
        ..area
    };
    let style = if selected {
        theme::selected()
    } else {
        theme::surface()
    };
    frame.render_widget(Block::default().style(style), body);
    let [_, rail, _, text, _] = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(body);
    let marker = if selected {
        "▎\n▎\n▎"
    } else if entry.unread {
        "●"
    } else {
        " "
    };
    let marker_color = if selected && focused {
        theme::BLUE
    } else if entry.unread {
        theme::YELLOW
    } else {
        theme::TEXT_MUTED
    };
    frame.render_widget(Paragraph::new(marker).fg(marker_color), rail);

    let width = usize::from(text.width);
    let root = display_preview(&entry.root_text, users);
    let root = if root.is_empty() { "[Message]" } else { &root };
    let sender = truncate_with_ellipsis(display_name(&entry.reply_user, users), width / 3);
    let reply_prefix = truncate_with_ellipsis(&format!("↳ {sender}: "), width);
    let reply = display_preview(&entry.reply_text, users);
    let reply = if reply.is_empty() { "[Reply]" } else { &reply };
    let reply = truncate_with_ellipsis(reply, width.saturating_sub(reply_prefix.width()));
    let lines = vec![
        thread_metadata(entry, sidebar, users, width),
        Line::from(truncate_with_ellipsis(root, width))
            .fg(theme::TEXT)
            .bold(),
        Line::from(vec![
            reply_prefix.fg(theme::TEXT),
            reply.fg(theme::TEXT_MUTED),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), text);
    if area.height == THREAD_ROW_HEIGHT {
        let separator = Rect::new(text.x, area.bottom().saturating_sub(1), text.width, 1);
        frame.render_widget(
            Line::from("─".repeat(width)).fg(theme::BORDER_MUTED),
            separator,
        );
    }
}

fn thread_metadata(
    entry: &ActivityEntry,
    sidebar: &ChannelSidebar,
    users: &HashMap<String, String>,
    width: usize,
) -> Line<'static> {
    let channel = sidebar
        .channels
        .iter()
        .find(|channel| channel.id == entry.target.channel_id);
    let label = channel.map_or(entry.target.channel_id.as_str(), |channel| {
        sidebar.channel_label(channel)
    });
    let prefix = if channel.is_some_and(|channel| channel.is_im == Some(true)) {
        "DM · "
    } else {
        "#"
    };
    let author = display_name(&entry.root_user, users);
    let source = format!("{prefix}{label} · {author}");
    let status = if entry.unread {
        format!(
            "{} new · {}",
            entry.unread_count.max(1),
            age(&entry.latest_ts)
        )
    } else {
        age(&entry.latest_ts)
    };
    let status = truncate_with_ellipsis(&status, width / 2);
    let source = truncate_with_ellipsis(
        &source,
        width.saturating_sub(status.width()).saturating_sub(2),
    );
    let gap = width
        .saturating_sub(source.width())
        .saturating_sub(status.width());
    Line::from(vec![
        source.fg(theme::BLUE),
        " ".repeat(gap).into(),
        status.fg(if entry.unread {
            theme::YELLOW
        } else {
            theme::TEXT_MUTED
        }),
    ])
}

fn display_name<'a>(id: &'a str, users: &'a HashMap<String, String>) -> &'a str {
    users.get(id).map_or(id, String::as_str)
}

fn display_preview(text: &str, users: &HashMap<String, String>) -> String {
    let bounded = crate::activity::preview(text);
    let markdown = crate::message_links::render_text(&bounded);
    let mut plain = String::new();
    for event in Parser::new_ext(&markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Text(text)
            | Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::FootnoteReference(text) => plain.push_str(&text),
            Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::CodeBlock,
            ) => plain.push(' '),
            Event::TaskListMarker(checked) => plain.push_str(if checked { "[x] " } else { "[ ] " }),
            Event::Start(_) | Event::End(_) => {}
        }
    }
    let named = replace_user_mentions(&plain, |id| users.get(id).cloned());
    named.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn age(ts: &str) -> String {
    let seconds = ts
        .split_once('.')
        .and_then(|(seconds, _)| seconds.parse::<u64>().ok());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = seconds.map_or(0, |seconds| now.saturating_sub(seconds));
    match elapsed {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", elapsed / 60),
        3600..86400 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic_in_result_fn,
    reason = "test assertions"
)]
mod tests {
    use std::collections::HashMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, layout::Position, style::Modifier};

    use crate::action::Action;
    use crate::activity::{ActivityEntry, test_entry};
    use crate::components::activity::{ActivityView, display_preview};
    use crate::components::sidebar::ChannelSidebar;
    use crate::theme;

    fn entry(index: u8) -> ActivityEntry {
        let mut entry = test_entry("C1", "1700000101.000000");
        entry.target.thread_ts = format!("1700000000.{index:06}");
        entry.root_text = format!("Thread {index}: review the release plan");
        entry.reply_text = "The <https://example.com/notes|release notes> are ready.".into();
        entry.unread = index == 2;
        entry
    }

    #[test]
    fn previews_show_text_instead_of_markup() {
        let users = HashMap::from([("U123".into(), "Alex".into())]);
        assert_eq!(
            display_preview(
                "*Update* from <@U123>: [release\\_notes](<https://example.com/notes>)\n\n`v2.0` &amp; <https://example.com/checks|checks>",
                &users,
            ),
            "Update from @Alex: release_notes v2.0 & checks"
        );
    }

    #[test]
    fn rows_are_distinct_and_clicks_follow_drawn_identities() -> color_eyre::Result<()> {
        let mut view = ActivityView::default();
        view.inbox.entries = (0..4).map(entry).collect();
        let clicked_target = view.inbox.entries[1].target.clone();
        let mut terminal = Terminal::new(TestBackend::new(64, 15))?;
        terminal.draw(|frame| {
            view.render(
                frame,
                frame.area(),
                true,
                &ChannelSidebar::new(),
                &HashMap::new(),
            );
        })?;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "▎");
        assert!(buffer[(4, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(4, 3)].bg, theme::SELECTED);
        assert_eq!(buffer[(4, 4)].symbol(), "─");
        assert_eq!(buffer[(4, 4)].bg, theme::SURFACE);
        assert!(matches!(
            view.handle_click(Position::new(8, 4)),
            Action::Render
        ));

        // The second displayed thread moves after an asynchronous refresh.
        view.inbox.entries.rotate_right(1);
        assert!(matches!(
            view.handle_click(Position::new(8, 6)),
            Action::OpenActivityThread(target) if target == clicked_target
        ));
        Ok(())
    }

    #[test]
    fn small_viewports_keep_the_selected_thread_reachable() -> color_eyre::Result<()> {
        let mut view = ActivityView::default();
        view.inbox.entries = (0..8).map(entry).collect();
        view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        for (width, height) in [(32, 11), (16, 5), (3, 3), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| {
                view.render(
                    frame,
                    frame.area(),
                    false,
                    &ChannelSidebar::new(),
                    &HashMap::new(),
                );
            })?;
            if width >= 3 && height >= 3 {
                let (area, target) = view.last_rows.last().expect("selected row is visible");
                assert_eq!(target, &entry(7).target);
                let position = Position::new(area.x, area.y);
                assert!(matches!(
                    view.handle_click(position),
                    Action::OpenActivityThread(_)
                ));
            } else {
                assert!(view.last_rows.is_empty());
            }
        }
        Ok(())
    }
}
