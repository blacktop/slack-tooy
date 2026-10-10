use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::action::Action;
use crate::components::EventResult;
use crate::components::messages::wrap_text;
use crate::emoji;
use crate::slack::types::Reaction;
use crate::theme;

const POPUP_MAX_WIDTH: u16 = 56;
const POPUP_MIN_WIDTH: u16 = 24;

/// Centered overlay listing who reacted to the selected message.
/// Holds a snapshot of the reactions at open time; user names resolve
/// live from the shared user cache as `users.info` responses arrive.
pub struct ReactionsPopup {
    reactions: Vec<Reaction>,
    scroll: u16,
}

impl ReactionsPopup {
    pub fn new(reactions: Vec<Reaction>) -> Self {
        Self {
            reactions,
            scroll: 0,
        }
    }

    /// User ids in the snapshot, for resolving display names.
    pub fn user_ids(&self) -> impl Iterator<Item = &str> {
        self.reactions
            .iter()
            .flat_map(|reaction| reaction.users.iter())
            .map(String::as_str)
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }

    /// Keys while the popup is open.  Returns [`EventResult::Ignored`]
    /// for keys it doesn't own; the caller decides whether those fall
    /// through (the popup is modal, so `App` swallows them).
    pub fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return EventResult::Ignored;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Left | KeyCode::Char('q' | 'r' | 'h') => {
                EventResult::Action(Action::ToggleReactions)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_up();
                EventResult::Consumed
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_down();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        bounds: Rect,
        user_cache: &HashMap<String, String>,
        custom_emoji_urls: &HashMap<String, String>,
    ) {
        if bounds.width < 8 || bounds.height < 5 {
            return;
        }

        let width = bounds
            .width
            .saturating_sub(6)
            .clamp(POPUP_MIN_WIDTH, POPUP_MAX_WIDTH);
        let text_width = usize::from(width.saturating_sub(4));
        let lines = build_lines(&self.reactions, user_cache, custom_emoji_urls, text_width);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to terminal height"
        )]
        let content_height = lines.len().min(usize::from(u16::MAX)) as u16;
        let max_height = bounds.height.saturating_sub(2);
        let height = content_height.saturating_add(2).min(max_height).max(3);

        let area = centered_rect(width, height, bounds);
        let block = theme::panel_block(
            Line::from(Span::styled(
                " reactions ",
                Style::default().fg(theme::BLUE).bold(),
            )),
            true,
        );
        let inner = block.inner(area);

        let max_scroll = content_height.saturating_sub(inner.height);
        self.scroll = self.scroll.min(max_scroll);

        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(lines)
                .style(theme::surface())
                .scroll((self.scroll, 0)),
            inner,
        );
    }
}

fn centered_rect(width: u16, height: u16, bounds: Rect) -> Rect {
    let x = bounds.x + bounds.width.saturating_sub(width) / 2;
    let y = bounds.y + bounds.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(bounds.width), height.min(bounds.height))
}

fn build_lines(
    reactions: &[Reaction],
    user_cache: &HashMap<String, String>,
    custom_emoji_urls: &HashMap<String, String>,
    text_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (i, reaction) in reactions.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(header_line(reaction, custom_emoji_urls));

        if reaction.users.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (names unavailable)",
                theme::muted(),
            )));
            continue;
        }

        let joined = reaction
            .users
            .iter()
            .map(|user_id| {
                user_cache
                    .get(user_id)
                    .map_or(user_id.as_str(), String::as_str)
            })
            .collect::<Vec<_>>()
            .join(", ");
        for wrapped in wrap_text(&joined, text_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(format!("  {wrapped}")));
        }

        let listed = u32::try_from(reaction.users.len()).unwrap_or(u32::MAX);
        let extra = reaction.count.saturating_sub(listed);
        if extra > 0 {
            lines.push(Line::from(Span::styled(
                format!("  +{extra} more"),
                theme::muted(),
            )));
        }
    }

    lines
}

fn header_line(reaction: &Reaction, custom_emoji_urls: &HashMap<String, String>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if let Some(unicode) = emoji::lookup(&reaction.name) {
        spans.push(Span::from(format!("{unicode} ")));
        spans.push(Span::styled(format!(":{}:", reaction.name), theme::muted()));
    } else {
        let style = if custom_emoji_urls.contains_key(&reaction.name) {
            Style::default().fg(theme::MAGENTA)
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!(":{}:", reaction.name), style));
    }
    spans.push(Span::styled(
        format!("  {}", reaction.count),
        Style::default().fg(theme::YELLOW),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::components::reactions::build_lines;
    use crate::slack::types::Reaction;

    fn reaction(name: &str, count: u32, users: &[&str]) -> Reaction {
        Reaction {
            name: name.into(),
            count,
            users: users.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn build_lines_resolves_names_and_notes_truncation() {
        let user_cache = HashMap::from([(String::from("U1"), String::from("Alice"))]);
        let reactions = vec![reaction("thumbsup", 3, &["U1", "U2"])];

        let lines = build_lines(&reactions, &user_cache, &HashMap::new(), 40);
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();

        assert!(text[0].contains(":thumbsup:"));
        assert!(text[0].contains('3'));
        assert!(text.iter().any(|line| line.contains("Alice, U2")));
        assert!(text.iter().any(|line| line.contains("+1 more")));
    }

    #[test]
    fn build_lines_handles_missing_user_list() {
        let reactions = vec![reaction("tada", 2, &[])];

        let lines = build_lines(&reactions, &HashMap::new(), &HashMap::new(), 40);
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();

        assert!(text.iter().any(|line| line.contains("names unavailable")));
    }

    #[test]
    fn build_lines_separates_reactions_with_blank_line() {
        let reactions = vec![
            reaction("thumbsup", 1, &["U1"]),
            reaction("tada", 1, &["U2"]),
        ];

        let lines = build_lines(&reactions, &HashMap::new(), &HashMap::new(), 40);
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();

        assert!(text.iter().any(String::is_empty));
        assert!(text.iter().any(|line| line.contains(":tada:")));
    }

    #[test]
    fn build_lines_wraps_long_name_lists() {
        let users: Vec<String> = (0..12).map(|i| format!("U{i:02}")).collect();
        let user_refs: Vec<&str> = users.iter().map(String::as_str).collect();
        let reactions = vec![reaction("eyes", 12, &user_refs)];

        let lines = build_lines(&reactions, &HashMap::new(), &HashMap::new(), 20);

        // Header + at least two wrapped name lines.
        assert!(lines.len() >= 3);
    }
}
