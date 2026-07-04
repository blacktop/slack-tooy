use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
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
        let (lines, _, _) = wrap_with_cursor(&self.text, self.text.len(), width);
        u16::try_from(lines.len()).unwrap_or(u16::MAX)
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
        let border_style = if focused {
            ratatui::style::Style::default().green()
        } else {
            ratatui::style::Style::default().dim()
        };

        // Pre-wrap with the same algorithm used for cursor math and
        // height sizing, so the drawn text, the box height, and the
        // cursor cell can never disagree (ratatui's `Wrap` word-wraps,
        // which diverges from the char-wrap the cursor math uses).
        let inner_width = area.width.saturating_sub(2);
        let inner_height = usize::from(area.height.saturating_sub(2));
        let (lines, cursor_row, cursor_col) =
            wrap_with_cursor(&self.text, self.cursor_pos, inner_width);

        // Scroll just enough to keep the cursor row visible when the
        // content is taller than the box.
        let scroll_top = cursor_row.saturating_sub(inner_height.saturating_sub(1));

        let text = Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>());
        let input = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title("Message".bold()),
            )
            .scroll((u16::try_from(scroll_top).unwrap_or(u16::MAX), 0));

        frame.render_widget(input, area);

        if focused && inner_height > 0 && inner_width > 0 {
            let row = u16::try_from(cursor_row - scroll_top).unwrap_or(u16::MAX);
            let col = u16::try_from(cursor_col).unwrap_or(u16::MAX);
            let cursor_y = area.y + 1 + row;
            let cursor_x = area.x + 1 + col.min(inner_width - 1);
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

/// Greedy character wrap of `text` at `width` display columns.
/// Returns the wrapped lines plus the visual (row, col) of the char
/// index `cursor` (the position where the next typed char lands).
///
/// This is the single source of truth for input layout: rendering,
/// box-height sizing, and cursor placement all call it.
fn wrap_with_cursor(text: &[char], cursor: usize, width: u16) -> (Vec<String>, usize, usize) {
    let width = usize::from(width.max(1));
    let mut lines = vec![String::new()];
    let mut col: usize = 0;
    let mut cursor_row = 0;
    let mut cursor_col = 0;

    for (i, &c) in text.iter().enumerate() {
        if i == cursor {
            cursor_row = lines.len() - 1;
            cursor_col = col;
        }

        if c == '\n' {
            lines.push(String::new());
            col = 0;
            continue;
        }

        let char_width = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
        if col > 0 && col.saturating_add(char_width) > width {
            lines.push(String::new());
            col = 0;
        }
        if let Some(line) = lines.last_mut() {
            line.push(c);
        }
        col = col.saturating_add(char_width);
    }

    if cursor >= text.len() {
        cursor_row = lines.len() - 1;
        cursor_col = col;
    }

    (lines, cursor_row, cursor_col)
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
    fn cursor_row_col_tracks_newlines() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('\n');
        input.insert_char('c');
        let cursor_at = |input: &TextInput| {
            let (_, row, col) = super::wrap_with_cursor(&input.text, input.cursor_pos, 80);
            (row, col)
        };
        assert_eq!(cursor_at(&input), (1, 1));

        input.move_cursor_home();
        assert_eq!(cursor_at(&input), (0, 0));

        input.move_cursor_end();
        assert_eq!(cursor_at(&input), (1, 1));
    }

    #[test]
    fn wrap_with_cursor_char_wraps_mid_word() {
        // Width 8: "hello world" must wrap after 8 columns, not at the
        // word boundary — the cursor math depends on it.
        let text: Vec<char> = "hello world".chars().collect();
        let (lines, row, col) = super::wrap_with_cursor(&text, text.len(), 8);

        assert_eq!(lines, vec!["hello wo".to_string(), "rld".to_string()]);
        assert_eq!((row, col), (1, 3));
    }

    #[test]
    fn wrap_with_cursor_counts_wide_chars_by_display_width() {
        // Each emoji is 2 columns wide: only two fit in width 5.
        let text: Vec<char> = "🦀🦀🦀".chars().collect();
        let (lines, row, col) = super::wrap_with_cursor(&text, text.len(), 5);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "🦀🦀");
        assert_eq!((row, col), (1, 2));
    }

    #[test]
    fn wrap_with_cursor_line_count_matches_visual_line_count() {
        let mut input = TextInput::new();
        input.insert_text("abcd\nef");
        let (lines, _, _) = super::wrap_with_cursor(&input.text, 0, 3);
        assert_eq!(u16::try_from(lines.len()), Ok(input.visual_line_count(3)));
    }

    #[test]
    fn wrap_with_cursor_empty_text_is_one_line() {
        let (lines, row, col) = super::wrap_with_cursor(&[], 0, 10);
        assert_eq!(lines, vec![String::new()]);
        assert_eq!((row, col), (0, 0));
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
