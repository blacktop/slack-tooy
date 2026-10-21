use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::action::{Action, ReactionTarget};
use crate::{emoji, theme};

const MAX_NAME_BYTES: usize = 128;
const MAX_QUERY_BYTES: usize = MAX_NAME_BYTES + 2;
const MAX_CATALOG_NAMES: usize = 8192;
const MAX_CHOICES: usize = 12;
const COMMON_NAMES: &[&str] = &["+1", "heart", "tada", "eyes", "joy", "white_check_mark"];

/// The target is frozen when the picker opens, even if a poll changes selection.
pub struct ReactionPicker {
    pub target: ReactionTarget,
    query: String,
    input_overflow: bool,
    catalog: Vec<String>,
    choices: Vec<String>,
    selected: usize,
}

impl ReactionPicker {
    pub fn new<'a>(target: ReactionTarget, names: impl Iterator<Item = &'a str>) -> Self {
        let mut catalog = BTreeSet::new();
        let standard_names = emoji::names().map(|name| -> &'a str { name });
        for name in standard_names.chain(names) {
            if reaction_name(name) == Some(name) {
                catalog.insert(name.to_string());
                if catalog.len() > MAX_CATALOG_NAMES {
                    catalog.pop_last();
                }
            }
        }
        let mut picker = Self {
            target,
            query: String::new(),
            input_overflow: false,
            catalog: catalog.into_iter().collect(),
            choices: Vec::new(),
            selected: 0,
        };
        picker.filter();
        picker
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.choices.get(self.selected).map(String::as_str)
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.len() > MAX_QUERY_BYTES.saturating_sub(self.query.len()) {
            self.input_overflow = true;
        } else {
            let previous_len = self.query.len();
            self.query
                .extend(text.chars().filter(|ch| !ch.is_control()));
            if self.query.len() != previous_len {
                self.input_overflow = false;
            }
        }
        self.filter();
    }

    pub fn move_selection(&mut self, up: bool) {
        self.selected = if up {
            self.selected.saturating_sub(1)
        } else {
            self.selected
                .saturating_add(1)
                .min(self.choices.len().saturating_sub(1))
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                KeyCode::Char('u') => {
                    self.query.clear();
                    self.input_overflow = false;
                    self.filter();
                    Action::Render
                }
                _ => Action::Render,
            };
        }
        match key.code {
            KeyCode::Esc => Action::CloseReactionPicker,
            KeyCode::Enter => Action::SubmitReaction,
            KeyCode::Up | KeyCode::BackTab => {
                self.move_selection(true);
                Action::Render
            }
            KeyCode::Down | KeyCode::Tab => {
                self.move_selection(false);
                Action::Render
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.input_overflow = false;
                self.filter();
                Action::Render
            }
            KeyCode::Char(ch) => {
                self.insert_text(ch.encode_utf8(&mut [0; 4]));
                Action::Render
            }
            _ => Action::Render,
        }
    }

    fn filter(&mut self) {
        self.selected = 0;
        self.choices.clear();
        if self.input_overflow {
            return;
        }
        let query = self.query.trim().trim_matches(':');
        if query.is_empty() {
            self.choices
                .extend(COMMON_NAMES.iter().map(|name| (*name).to_string()));
        } else if let Some(name) = reaction_name(query) {
            // An exact typed name is always available, including Slack names
            // absent from our small Unicode table and workspace snapshot.
            self.choices.push(name.to_string());
        }
        for name in &self.catalog {
            if self.choices.len() >= MAX_CHOICES {
                break;
            }
            if name.contains(query) && !self.choices.contains(name) {
                self.choices.push(name.clone());
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, bounds: Rect) {
        if bounds.width < 16 || bounds.height < 6 {
            return;
        }
        let width = bounds.width.saturating_sub(2).min(64);
        let height = bounds.height.saturating_sub(2).min(17);
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
        let block = theme::panel_block(Line::from(" add reaction "), true);
        let inner = block.inner(area);
        let [search, _, choices, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        let input = format!("> {}", self.query);
        let input_width = UnicodeWidthStr::width(input.as_str());
        let scroll = input_width.saturating_sub(usize::from(search.width.saturating_sub(1)));
        frame.render_widget(
            Paragraph::new(input)
                .style(theme::surface())
                .scroll((0, u16::try_from(scroll).unwrap_or(u16::MAX))),
            search,
        );
        let start = self
            .selected
            .saturating_sub(usize::from(choices.height.saturating_sub(1)));
        let lines: Vec<_> = self
            .choices
            .iter()
            .enumerate()
            .skip(start)
            .take(usize::from(choices.height))
            .map(|(index, name)| {
                let marker = if index == self.selected { ">" } else { " " };
                let glyph = emoji::lookup(name).unwrap_or(" ");
                let style = if index == self.selected {
                    theme::selected()
                } else {
                    theme::surface()
                };
                Line::from(Span::styled(format!("{marker} {glyph} :{name}:"), style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).style(theme::surface()), choices);
        let hint_text = if self.input_overflow {
            "Emoji name too long; Ctrl+U:clear"
        } else {
            "↑/↓:choose Enter:add Esc:cancel"
        };
        frame.render_widget(Paragraph::new(hint_text).style(theme::muted()), hint);
        if search.width > 0 && search.height > 0 {
            let cursor = u16::try_from(input_width.saturating_sub(scroll)).unwrap_or(u16::MAX);
            frame.set_cursor_position((
                search
                    .x
                    .saturating_add(cursor.min(search.width.saturating_sub(1))),
                search.y,
            ));
        }
    }
}

fn reaction_name(input: &str) -> Option<&str> {
    let name = input.trim().trim_matches(':');
    if let Some(name) = emoji::name_for_unicode(name) {
        return Some(name);
    }
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return None;
    }
    let base = if let Some((base, tone)) = name.split_once("::skin-tone-") {
        if !matches!(tone, "2" | "3" | "4" | "5" | "6") {
            return None;
        }
        base
    } else {
        name
    };
    (!base.is_empty()
        && base
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b'+')))
    .then_some(name)
}

#[cfg(test)]
mod tests {
    use crate::action::{Action, ReactionTarget};
    use crate::components::reaction_picker::{
        MAX_CATALOG_NAMES, MAX_CHOICES, MAX_NAME_BYTES, ReactionPicker, reaction_name,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn picker(names: &[&str]) -> ReactionPicker {
        ReactionPicker::new(
            ReactionTarget {
                channel_id: "C1".into(),
                message_ts: "100.2".into(),
            },
            names.iter().copied(),
        )
    }

    #[test]
    fn names_accept_shortcodes_unicode_and_skin_tones() {
        assert_eq!(reaction_name(" :thumbsup: "), Some("thumbsup"));
        assert_eq!(reaction_name("👍"), Some("+1"));
        assert_eq!(
            reaction_name(":wave::skin-tone-4:"),
            Some("wave::skin-tone-4")
        );
        assert_eq!(reaction_name("custom-party"), Some("custom-party"));
        assert_eq!(reaction_name("bad name"), None);
        assert_eq!(reaction_name("wave::skin-tone-9"), None);
        assert_eq!(reaction_name(":"), None);
    }

    #[test]
    fn exact_name_is_preserved_and_custom_suggestions_are_selectable() {
        let mut picker = picker(&["team_party", "team_parrot"]);
        picker.insert_text("team_par");
        assert_eq!(picker.selected_name(), Some("team_par"));
        picker.move_selection(false);
        assert_eq!(picker.selected_name(), Some("team_parrot"));
        assert!(matches!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::SubmitReaction
        ));
        picker.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        picker.insert_text(":standard_name_absent_from_local_map:");
        assert_eq!(
            picker.selected_name(),
            Some("standard_name_absent_from_local_map")
        );
    }

    #[test]
    fn oversized_paste_cannot_submit_a_valid_truncated_name() {
        let mut picker = picker(&[]);
        let name = "a".repeat(MAX_NAME_BYTES);
        picker.insert_text(&format!(":{name}:suffix"));
        assert!(picker.selected_name().is_none());
        assert_eq!(picker.query, "");

        picker.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        picker.insert_text(":heart:");
        assert_eq!(picker.selected_name(), Some("heart"));
    }

    #[test]
    fn picker_bounds_choices_and_catalog() {
        let names: Vec<_> = (0..MAX_CATALOG_NAMES + 20)
            .map(|index| format!("custom_{index:05}"))
            .collect();
        let picker = ReactionPicker::new(picker(&[]).target, names.iter().map(String::as_str));
        assert_eq!(picker.catalog.len(), MAX_CATALOG_NAMES);
        assert!(picker.choices.len() <= MAX_CHOICES);
    }

    #[test]
    fn picker_renders_selected_name_and_survives_small_terminals() {
        let mut picker = picker(&["team_party"]);
        picker.insert_text(":team_party:");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, frame.area()))
            .expect("render");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("add reaction"));
        assert!(text.contains(":team_party:"));
        for (width, height) in [(1, 1), (16, 6), (20, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|frame| picker.render(frame, frame.area()))
                .expect("small render");
        }
    }
}
