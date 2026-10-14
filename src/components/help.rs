use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::components::messages::wrap_text;
use crate::theme;

struct Section {
    title: &'static str,
    bindings: &'static [(&'static str, &'static str)],
}

const SECTIONS: &[Section] = &[
    Section {
        title: "Global · Normal mode",
        bindings: &[
            ("?", "Open this key reference"),
            ("q / Ctrl+c", "Quit slack-tooy"),
            ("i", "Write a message or reply"),
            ("a", "Open Activity"),
            ("N", "Open the latest notified chat or thread"),
            ("Tab / Shift+Tab", "Switch between Channels and Messages"),
            ("1 / 2", "Focus Channels / Messages"),
            (
                "→ / ←",
                "Focus Messages / Channels; ← closes an open thread",
            ),
        ],
    },
    Section {
        title: "Activity",
        bindings: &[
            ("j / k / ↓ / ↑", "Move between threads"),
            ("g / G / Home / End", "Jump to first / last thread"),
            ("Enter / l / →", "Open the selected thread"),
            ("u", "Toggle unread threads only"),
            ("m", "Mark the selected thread read"),
            ("r", "Refresh Activity"),
            ("Esc / h / ←", "Focus Channels"),
        ],
    },
    Section {
        title: "Channels",
        bindings: &[
            ("j / k / ↓ / ↑", "Move down / up"),
            ("g / G", "Jump to first / last channel"),
            ("z / Z", "Collapse current section / expand all sections"),
            ("Enter", "Open the selected channel or DM"),
            ("R", "Mark all channels read"),
            ("u", "Toggle unread channels only"),
        ],
    },
    Section {
        title: "Messages and threads",
        bindings: &[
            ("j / k / ↓ / ↑", "Scroll down / up by line"),
            ("J / K", "Select next / previous message"),
            ("g / G", "Jump to oldest / newest message"),
            ("Ctrl+u / PageUp", "Scroll up half a page"),
            ("Ctrl+d / PageDown", "Scroll down half a page"),
            ("Ctrl+b / Ctrl+f", "Scroll up / down a full page"),
            ("Enter / l / →", "Open the selected message's thread"),
            ("Esc / h / ←", "Close the thread"),
            ("r", "Show who reacted"),
            ("R / +", "Add a reaction"),
            ("c", "Copy the selected message with clean link URLs"),
            ("o", "Open a message link in your browser"),
            ("d", "Save attached files to Downloads"),
        ],
    },
    Section {
        title: "Compose · Insert mode",
        bindings: &[
            ("Enter", "Send the message"),
            ("Alt+Enter / Ctrl+j", "Insert a newline"),
            (
                "Shift+Enter",
                "Insert a newline when supported by the terminal",
            ),
            ("Esc", "Return to Normal mode"),
            ("← / →", "Move the cursor"),
            ("Home / End", "Move to the start / end of the draft"),
            ("Backspace / Delete", "Delete before / at the cursor"),
            (
                "/upload <path> [comment]",
                "Upload a local file to this chat",
            ),
        ],
    },
    Section {
        title: "Reaction and link pickers",
        bindings: &[
            ("Type / paste", "Search emoji in the reaction picker"),
            ("Backspace / Ctrl+u", "Edit / clear the emoji search"),
            ("↓ / ↑ / Tab / Shift+Tab", "Choose an emoji or link"),
            ("j / k", "Choose a link; scroll the who-reacted list"),
            (
                "Enter",
                "Add the emoji, open the link, or close who-reacted",
            ),
            ("Esc", "Close the picker or who-reacted list"),
            ("q", "Close the link picker or who-reacted list"),
            ("r / h / ←", "Close the who-reacted list"),
        ],
    },
    Section {
        title: "This help window",
        bindings: &[
            ("j / k / ↓ / ↑", "Scroll down / up"),
            ("PageUp / PageDown", "Scroll up / down one page"),
            ("Ctrl+u / Ctrl+d", "Scroll up / down half a page"),
            ("Ctrl+b / Ctrl+f", "Scroll up / down one page"),
            ("g / G / Home / End", "Jump to first / last section"),
            ("Esc / ? / q / Enter", "Close this window"),
        ],
    },
    Section {
        title: "Mouse",
        bindings: &[
            ("Click channel / Activity", "Open that channel or Activity"),
            ("Click Activity thread", "Open the thread"),
            ("Click section header", "Collapse / expand the section"),
            ("Click message", "Select the message"),
            ("Click composer", "Start writing"),
            (
                "Scroll wheel",
                "Scroll or move selection in the hovered pane",
            ),
            (
                "Shift / Option + drag",
                "Use terminal text selection (terminal dependent)",
            ),
        ],
    },
];

#[derive(Default)]
pub struct HelpPopup {
    scroll: u16,
    page_height: u16,
}

impl HelpPopup {
    pub fn scroll(&mut self, up: bool, amount: u16) {
        self.scroll = if up {
            self.scroll.saturating_sub(amount)
        } else {
            self.scroll.saturating_add(amount)
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Action::Quit,
                KeyCode::Char('u') => self.scroll(true, (self.page_height / 2).max(1)),
                KeyCode::Char('d') => self.scroll(false, (self.page_height / 2).max(1)),
                KeyCode::Char('b') => self.scroll(true, self.page_height.max(1)),
                KeyCode::Char('f') => self.scroll(false, self.page_height.max(1)),
                _ => {}
            }
            return Action::Render;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?' | 'q') => return Action::CloseHelp,
            KeyCode::Up | KeyCode::Char('k') => self.scroll(true, 1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(false, 1),
            KeyCode::PageUp => self.scroll(true, self.page_height.max(1)),
            KeyCode::PageDown => self.scroll(false, self.page_height.max(1)),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = u16::MAX,
            _ => {}
        }
        Action::Render
    }

    pub fn render(&mut self, frame: &mut Frame, bounds: Rect) {
        if bounds.width < 8 || bounds.height < 6 {
            return;
        }
        let width = bounds.width.saturating_sub(2).min(94);
        let height = bounds.height.saturating_sub(2).min(34);
        let area = Rect::new(
            bounds
                .x
                .saturating_add(bounds.width.saturating_sub(width) / 2),
            bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height) / 2),
            width,
            height,
        );
        let block = theme::panel_block(
            Line::from(Span::styled(
                " Key bindings ",
                Style::default().fg(theme::BLUE).bold(),
            )),
            true,
        );
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(block.inner(area));
        let [_, content, _] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(body);
        let lines = build_lines(usize::from(content.width));
        self.page_height = content.height;
        let max_scroll = lines.len().saturating_sub(usize::from(content.height));
        self.scroll = self
            .scroll
            .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
        let mut scrollbar =
            ScrollbarState::new(max_scroll.saturating_add(1)).position(usize::from(self.scroll));

        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(lines)
                .style(theme::surface())
                .scroll((self.scroll, 0)),
            content,
        );
        if max_scroll > 0 {
            frame.render_stateful_widget(Scrollbar::default(), body, &mut scrollbar);
        }
        let hint = if footer.width >= 42 {
            "↑/↓:scroll  PgUp/PgDn:page  Esc:close"
        } else {
            "Esc:close"
        };
        frame.render_widget(
            Paragraph::new(hint).centered().style(theme::muted()),
            footer,
        );
    }
}

fn build_lines(width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let key_style = Style::default().fg(theme::BLUE).bold();
    let mut lines = Vec::new();
    for section in SECTIONS {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(
            wrap_text(section.title, width).into_iter().map(|line| {
                Line::from(Span::styled(line, Style::default().fg(theme::TEXT).bold()))
            }),
        );
        for &(keys, description) in section.bindings {
            if width < 60 {
                lines.extend(
                    wrap_text(keys, width)
                        .into_iter()
                        .map(|line| Line::from(Span::styled(line, key_style))),
                );
                let indent = width.saturating_sub(1).min(2);
                lines.extend(
                    wrap_text(description, width.saturating_sub(indent))
                        .into_iter()
                        .map(|line| Line::from(format!("{}{line}", " ".repeat(indent)))),
                );
                continue;
            }
            let key_width = 27;
            let keys = wrap_text(keys, key_width);
            let descriptions = wrap_text(description, width.saturating_sub(key_width + 2));
            for row in 0..keys.len().max(descriptions.len()) {
                let key = keys.get(row).map_or("", String::as_str);
                let description = descriptions.get(row).map_or("", String::as_str);
                lines.push(Line::from(vec![
                    Span::styled(key.to_string(), key_style),
                    Span::raw(" ".repeat(key_width.saturating_sub(key.width()).saturating_add(2))),
                    Span::raw(description.to_string()),
                ]));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use crate::components::help::{HelpPopup, build_lines};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn bindings_fit_narrow_and_wide_layouts() {
        for width in [1, 16, 42, 60, 88] {
            for line in build_lines(width) {
                assert!(line.width() <= width, "{width}: {line}");
            }
        }
    }

    #[test]
    fn help_can_reach_last_section_and_return_to_top() {
        let mut help = HelpPopup::default();
        let mut terminal = Terminal::new(TestBackend::new(52, 14)).expect("terminal");
        terminal
            .draw(|frame| help.render(frame, frame.area()))
            .expect("draw");
        help.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal
            .draw(|frame| help.render(frame, frame.area()))
            .expect("draw");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("terminal text selection"));
        assert!(rendered.contains("Esc:close"));
        help.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        terminal
            .draw(|frame| help.render(frame, frame.area()))
            .expect("draw");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Global · Normal mode"));
        assert_eq!(help.scroll, 0);
    }
}
