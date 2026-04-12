pub mod input;
pub mod messages;
pub mod sidebar;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::action::Action;

/// Result of a component handling a key event.
pub enum EventResult {
    /// Event was consumed, no app-level action needed.
    Consumed,
    /// Event was not relevant to this component.
    Ignored,
    /// Event produced an app-level action.
    Action(Action),
}

/// Trait implemented by each UI panel.
pub trait Component {
    fn handle_key(&mut self, key: KeyEvent) -> EventResult;
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool);
}
