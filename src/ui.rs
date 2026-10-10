use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Mode};
use crate::components::Component;
use crate::theme;

/// Pane rectangles from the most recent layout pass, used to route
/// mouse events.  Zero-sized rects (before the first render) contain
/// no position, so early mouse events are safely ignored.
#[derive(Debug, Clone, Copy, Default)]
pub struct Panes {
    pub sidebar: Rect,
    pub messages: Rect,
    pub input: Rect,
}

pub fn render(frame: &mut Frame, app: &mut App) {
    frame.render_widget(Block::default().style(theme::canvas()), frame.area());

    let [main_area, status_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    let [sidebar_area, _, right_area] = Layout::horizontal([
        Constraint::Ratio(app.config.sidebar_width, 12),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(main_area);

    // Grow input box to fit rendered content (visual rows + borders), capped at 8.
    let input_inner_width = right_area.width.saturating_sub(2);
    let input_height = app
        .input
        .visual_line_count(input_inner_width)
        .saturating_add(2)
        .min(8);
    let [messages_area, _, input_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(input_height),
    ])
    .areas(right_area);

    app.panes = Panes {
        sidebar: sidebar_area,
        messages: messages_area,
        input: input_area,
    };

    app.sidebar
        .render(frame, sidebar_area, app.focus == Focus::Sidebar);
    app.messages
        .render(frame, messages_area, app.focus == Focus::Messages);
    app.input
        .render(frame, input_area, app.mode == Mode::Insert);

    if let Some(ref mut popup) = app.reactions_popup {
        popup.render(
            frame,
            messages_area,
            &app.messages.user_cache,
            &app.messages.custom_emoji_urls,
        );
    }
    if let Some(ref picker) = app.reaction_picker {
        picker.render(frame, messages_area);
    }
    if let Some(ref picker) = app.link_picker {
        picker.render(frame, messages_area);
    }

    render_status_bar(frame, status_area, app);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    spans.push(Span::styled(
        " TOOY ",
        Style::default()
            .fg(theme::TEXT)
            .bg(theme::SURFACE_RAISED)
            .bold(),
    ));

    let mode = match app.mode {
        Mode::Insert => Span::styled(
            " INSERT ",
            Style::default().fg(theme::CANVAS).bg(theme::GREEN).bold(),
        ),
        Mode::Normal => Span::styled(
            " NORMAL ",
            Style::default().fg(theme::TEXT_MUTED).bg(theme::SURFACE),
        ),
    };
    spans.push(mode);

    let focus_label = match app.focus {
        Focus::Sidebar => " nav ",
        Focus::Messages => " history ",
    };
    spans.push(Span::styled(focus_label, theme::muted()));

    if let Some(ref id) = app.current_channel_id {
        let name = app.messages.channel_name.as_str();
        if name.is_empty() {
            spans.push(Span::styled(format!(" {id} "), theme::muted()));
        } else {
            spans.push(Span::styled(
                format!(" #{name} "),
                Style::default().fg(theme::BLUE).bg(theme::CANVAS),
            ));
        }
        if app.messages.active_thread.is_some() {
            spans.push(Span::styled(" thread ", Style::default().fg(theme::BLUE)));
        }
    }

    if let Some(ref msg) = app.status_message {
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::default().fg(theme::YELLOW),
        ));
    } else if app.loading {
        spans.push(Span::styled(
            " Loading... ",
            Style::default().fg(theme::YELLOW),
        ));
    }

    let used: usize = spans.iter().map(Span::width).sum();
    let remaining = (area.width as usize).saturating_sub(used);

    let hints = if app.link_picker.is_some() {
        "j/k:choose Enter:open Esc:cancel"
    } else if app.reaction_picker.is_some() {
        "type:search ↑/↓:choose Enter:add Esc:cancel"
    } else if app.reactions_popup.is_some() {
        "Esc/q:close j/k:scroll"
    } else {
        match app.mode {
            Mode::Insert if app.keyboard_enhanced => {
                "Esc:normal Enter:send Opt/Shift+Enter:newline"
            }
            Mode::Insert => "Esc:normal Enter:send Opt+Enter/Ctrl+J:newline",
            Mode::Normal => match app.focus {
                Focus::Sidebar => {
                    "q:quit i:insert Tab:history Enter:open R:read-all u:unread j/k:move"
                }
                Focus::Messages => {
                    if app.messages.active_thread.is_some() {
                        "q:quit i:reply h/Left:close R:react r:reactions o:open c:copy d:save J/K:msg j/k:line"
                    } else {
                        "q:quit i:insert Tab:nav l/Right:thread R:react r:reactions o:open c:copy d:save J/K:msg j/k:line"
                    }
                }
            },
        }
    };

    let hint_width = UnicodeWidthStr::width(hints) + 2;
    if remaining > hint_width {
        let pad = remaining - hint_width;
        spans.push(Span::from(" ".repeat(pad)));
    }
    spans.push(Span::styled(format!(" {hints} "), theme::muted()));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line).style(theme::canvas()), area);
}
