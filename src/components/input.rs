use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::action::Action;
use crate::components::{Component, EventResult};

pub struct TextInput {
    text: Vec<char>,
    cursor_pos: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self {
            text: Vec::new(),
            cursor_pos: 0,
        }
    }

    pub fn get_text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of visual lines in the input (1 + count of newlines).
    pub fn line_count(&self) -> u16 {
        let count = self.text.iter().filter(|&&c| c == '\n').count();
        u16::try_from(count + 1).unwrap_or(u16::MAX)
    }

    /// Cursor (row, col) where row counts newlines before cursor
    /// and col counts chars since the last newline.
    fn cursor_row_col(&self) -> (usize, usize) {
        let mut row = 0;
        let mut col = 0;
        for &c in &self.text[..self.cursor_pos] {
            if c == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (row, col)
    }

    fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor_pos, c);
        self.cursor_pos += 1;
    }

    fn delete_char_back(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.text.remove(self.cursor_pos);
        }
    }

    fn delete_char_forward(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.text.remove(self.cursor_pos);
        }
    }

    fn move_cursor_left(&mut self) {
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.cursor_pos += 1;
        }
    }

    fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    fn move_cursor_end(&mut self) {
        self.cursor_pos = self.text.len();
    }
}

impl Component for TextInput {
    fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        match key.code {
            KeyCode::Char(c) => {
                self.insert_char(c);
                EventResult::Consumed
            }
            KeyCode::Backspace => {
                self.delete_char_back();
                EventResult::Consumed
            }
            KeyCode::Delete => {
                self.delete_char_forward();
                EventResult::Consumed
            }
            KeyCode::Left => {
                self.move_cursor_left();
                EventResult::Consumed
            }
            KeyCode::Right => {
                self.move_cursor_right();
                EventResult::Consumed
            }
            KeyCode::Home => {
                self.move_cursor_home();
                EventResult::Consumed
            }
            KeyCode::End => {
                self.move_cursor_end();
                EventResult::Consumed
            }
            KeyCode::Enter if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) => {
                self.insert_char('\n');
                EventResult::Consumed
            }
            KeyCode::Enter => {
                if self.is_empty() {
                    EventResult::Consumed
                } else {
                    EventResult::Action(Action::SendMessage)
                }
            }
            KeyCode::Esc => EventResult::Action(Action::EnterNormalMode),
            _ => EventResult::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let display_text: String = self.text.iter().collect();

        let border_style = if focused {
            ratatui::style::Style::default().green()
        } else {
            ratatui::style::Style::default().dim()
        };

        let input = Paragraph::new(display_text.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Message".bold()),
        );

        frame.render_widget(input, area);

        if focused {
            let (row, col) = self.cursor_row_col();
            // Clamp to inner area (subtract 2 for borders, +1 for offset)
            let max_row = area.height.saturating_sub(2);
            let max_col = area.width.saturating_sub(2);
            #[expect(clippy::cast_possible_truncation, reason = "bounded by visible area")]
            let (row_u16, col_u16) = (row as u16, col as u16);
            let cursor_y = area.y + row_u16.min(max_row.saturating_sub(1)) + 1;
            let cursor_x = area.x + col_u16.min(max_col.saturating_sub(1)) + 1;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_text() {
        let mut input = TextInput::new();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.get_text(), "hi");
        assert_eq!(input.cursor_pos, 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut input = TextInput::new();
        input.delete_char_back();
        assert!(input.is_empty());
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn backspace_removes_char() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.delete_char_back();
        assert_eq!(input.get_text(), "a");
        assert_eq!(input.cursor_pos, 1);
    }

    #[test]
    fn cursor_movement() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');

        input.move_cursor_home();
        assert_eq!(input.cursor_pos, 0);

        input.move_cursor_end();
        assert_eq!(input.cursor_pos, 3);

        input.move_cursor_left();
        assert_eq!(input.cursor_pos, 2);

        input.move_cursor_right();
        assert_eq!(input.cursor_pos, 3);

        // Right past end is clamped
        input.move_cursor_right();
        assert_eq!(input.cursor_pos, 3);

        // Left past start is clamped
        input.move_cursor_home();
        input.move_cursor_left();
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn insert_at_cursor_mid() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('c');
        input.move_cursor_left();
        input.insert_char('b');
        assert_eq!(input.get_text(), "abc");
        assert_eq!(input.cursor_pos, 2);
    }

    #[test]
    fn delete_forward() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.move_cursor_home();
        input.delete_char_forward();
        assert_eq!(input.get_text(), "b");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn clear_resets_state() {
        let mut input = TextInput::new();
        input.insert_char('x');
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn enter_on_empty_does_not_send() {
        let mut input = TextInput::new();
        let key = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Consumed));
    }

    #[test]
    fn alt_enter_inserts_newline() {
        let mut input = TextInput::new();
        input.insert_char('a');
        let key = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::ALT);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Consumed));
        input.insert_char('b');
        assert_eq!(input.get_text(), "a\nb");
        assert_eq!(input.line_count(), 2);
    }

    #[test]
    fn cursor_row_col_tracks_newlines() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('\n');
        input.insert_char('c');
        assert_eq!(input.cursor_row_col(), (1, 1));

        input.move_cursor_home();
        assert_eq!(input.cursor_row_col(), (0, 0));

        input.move_cursor_end();
        assert_eq!(input.cursor_row_col(), (1, 1));
    }

    #[test]
    fn enter_with_text_sends() {
        let mut input = TextInput::new();
        input.insert_char('h');
        let key = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Action(Action::SendMessage)));
    }
}
