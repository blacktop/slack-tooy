use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use reqwest::Url;

use crate::action::Action;
use crate::theme;

/// Destinations are captured when the picker opens, so refreshes cannot change
/// the link that Enter opens.
pub struct LinkPicker {
    links: Vec<Url>,
    selected: usize,
}

impl LinkPicker {
    pub fn new(links: Vec<Url>) -> Self {
        Self { links, selected: 0 }
    }

    pub fn move_selection(&mut self, up: bool) {
        self.selected = if up {
            self.selected.saturating_sub(1)
        } else {
            self.selected
                .saturating_add(1)
                .min(self.links.len().saturating_sub(1))
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                Action::Quit
            } else {
                Action::Render
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Action::CloseLinkPicker,
            KeyCode::Enter => self
                .links
                .get(self.selected)
                .cloned()
                .map_or(Action::Render, Action::OpenUrl),
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                self.move_selection(true);
                Action::Render
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                self.move_selection(false);
                Action::Render
            }
            _ => Action::Render,
        }
    }

    pub fn render(&self, frame: &mut Frame, bounds: Rect) {
        if bounds.width < 16 || bounds.height < 6 {
            return;
        }
        let width = bounds.width.saturating_sub(2).min(88);
        let height = bounds.height.saturating_sub(2).min(16);
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
        let block = theme::panel_block(Line::from(" open link "), true);
        let [choices, hint] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(block.inner(area));
        let start = self
            .selected
            .saturating_sub(usize::from(choices.height.saturating_sub(1)));
        let lines: Vec<_> = self
            .links
            .iter()
            .enumerate()
            .skip(start)
            .take(usize::from(choices.height))
            .map(|(index, url)| {
                let (marker, style) = if index == self.selected {
                    (">", theme::selected())
                } else {
                    (" ", theme::surface())
                };
                Line::from(Span::styled(format!("{marker} {url}"), style))
            })
            .collect();
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).style(theme::surface()), choices);
        frame.render_widget(
            Paragraph::new("j/k:choose Enter:open Esc:cancel").style(theme::muted()),
            hint,
        );
    }
}
