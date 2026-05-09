use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::action::Action;
use crate::components::{Component, EventResult};

const PASTE_TAB_WIDTH: usize = 4;

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

    pub fn insert_text(&mut self, text: &str) {
        let mut skip_next_linefeed = false;
        for c in text.chars() {
            if skip_next_linefeed {
                skip_next_linefeed = false;
                if c == '\n' {
                    continue;
                }
            }

            match c {
                '\r' => {
                    self.insert_char('\n');
                    skip_next_linefeed = true;
                }
                '\n' => {
                    self.insert_char('\n');
                }
                '\t' => {
                    for _ in 0..PASTE_TAB_WIDTH {
                        self.insert_char(' ');
                    }
                }
                c if c.is_control() => {}
                c => {
                    self.insert_char(c);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of rendered rows needed for the input at a given inner
    /// width, including explicit newlines and soft wraps.
    pub fn visual_line_count(&self, width: u16) -> u16 {
        let (row, _) = Self::wrapped_row_col(self.text.iter().copied(), width);
        u16::try_from(row.saturating_add(1)).unwrap_or(u16::MAX)
    }

    fn cursor_visual_row_col(&self, width: u16) -> (usize, usize) {
        Self::wrapped_row_col(self.text[..self.cursor_pos].iter().copied(), width)
    }

    fn wrapped_row_col<I>(chars: I, width: u16) -> (usize, usize)
    where
        I: IntoIterator<Item = char>,
    {
        let width = usize::from(width.max(1));
        let mut row: usize = 0;
        let mut col: usize = 0;

        for c in chars {
            if c == '\n' {
                row += 1;
                col = 0;
                continue;
            }

            let char_width = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
            if col > 0 && col.saturating_add(char_width) > width {
                row += 1;
                col = 0;
            }
            col = col.saturating_add(char_width);
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
        let newline_modifiers = KeyModifiers::ALT | KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char('\n');
                EventResult::Consumed
            }
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
            KeyCode::Enter if key.modifiers.intersects(newline_modifiers) => {
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

        let input = Paragraph::new(display_text.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title("Message".bold()),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(input, area);

        if focused {
            // Clamp to inner area (subtract 2 for borders, +1 for offset)
            let max_row = area.height.saturating_sub(2);
            let max_col = area.width.saturating_sub(2);
            let (row, col) = self.cursor_visual_row_col(max_col);
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::action::Action;
    use crate::components::input::TextInput;
    use crate::components::{Component, EventResult};

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
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Consumed));
        input.insert_char('b');
        assert_eq!(input.get_text(), "a\nb");
        assert_eq!(input.visual_line_count(80), 2);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut input = TextInput::new();
        input.insert_char('a');
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Consumed));
        input.insert_char('b');
        assert_eq!(input.get_text(), "a\nb");
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut input = TextInput::new();
        input.insert_char('a');
        let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        let result = input.handle_key(key);
        assert!(matches!(result, EventResult::Consumed));
        input.insert_char('b');
        assert_eq!(input.get_text(), "a\nb");
    }

    #[test]
    fn paste_preserves_multiline_text() {
        let mut input = TextInput::new();
        input.insert_text("a\r\n\tb\nc");
        assert_eq!(input.get_text(), "a\n    b\nc");
        assert_eq!(input.visual_line_count(80), 3);
    }

    #[test]
    fn visual_line_count_includes_soft_wraps() {
        let mut input = TextInput::new();
        input.insert_text("abcd\nef");
        assert_eq!(input.visual_line_count(3), 3);
    }

    #[test]
    fn cursor_visual_row_col_tracks_newlines() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('\n');
        input.insert_char('c');
        assert_eq!(input.cursor_visual_row_col(80), (1, 1));

        input.move_cursor_home();
        assert_eq!(input.cursor_visual_row_col(80), (0, 0));

        input.move_cursor_end();
        assert_eq!(input.cursor_visual_row_col(80), (1, 1));
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
