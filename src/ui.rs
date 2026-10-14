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
    pub activity: Rect,
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

    let [activity_area, sidebar_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(sidebar_area);

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

    let (messages_area, input_area) = if app.activity_open {
        (right_area, Rect::default())
    } else {
        (messages_area, input_area)
    };
    app.panes = Panes {
        sidebar: sidebar_area,
        activity: activity_area,
        messages: messages_area,
        input: input_area,
    };
    render_activity_button(frame, activity_area, app);

    app.sidebar
        .render(frame, sidebar_area, app.focus == Focus::Sidebar);
    if app.activity_open {
        app.activity.render(
            frame,
            messages_area,
            app.focus == Focus::Messages,
            &app.sidebar,
            &app.messages.user_cache,
        );
    } else {
        app.messages
            .render(frame, messages_area, app.focus == Focus::Messages);
        app.input
            .render(frame, input_area, app.mode == Mode::Insert);
    }

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
    if let Some(help) = app.help_popup.as_mut() {
        help.render(frame, main_area);
    }
}

fn render_activity_button(frame: &mut Frame, area: Rect, app: &App) {
    let count = app.activity.inbox.unread_count();
    let label = if count == 0 {
        " Activity  a ".to_string()
    } else {
        format!(" Activity  ● {count}  a ")
    };
    let mut block = theme::panel_block(Line::default(), app.activity_open);
    let mut style = if app.activity_open {
        theme::selected()
    } else {
        theme::surface()
    };
    if count > 0 {
        style = style.fg(theme::YELLOW).bold();
        if !app.activity_open {
            style = style.bg(theme::SURFACE_RAISED);
            block = block.border_style(Style::default().fg(theme::YELLOW));
        }
    }
    frame.render_widget(Paragraph::new(label).block(block).style(style), area);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
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
    let mut spans = vec![mode];

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

    let (hints, compact) = if app.help_popup.is_some() {
        ("↑/↓:scroll  Esc:close", "Esc:close")
    } else if app.link_picker.is_some() {
        ("Enter:open  Esc:cancel", "Esc:cancel")
    } else if app.reaction_picker.is_some() {
        ("Enter:add  Esc:cancel", "Esc:cancel")
    } else if app.reactions_popup.is_some() {
        ("↑/↓:scroll  Esc:close", "Esc:close")
    } else if app.mode == Mode::Insert {
        ("Enter:send  Esc:normal", "Esc:normal")
    } else if app.activity_open && app.focus == Focus::Messages {
        ("Enter:thread  ?:help", "?:help")
    } else {
        let hint = match app.focus {
            Focus::Sidebar => "Enter:open  ?:help",
            Focus::Messages if app.messages.active_thread.is_some() => "i:reply  ?:help",
            Focus::Messages => "i:write  ?:help",
        };
        (hint, "?:help")
    };
    let hints = if usize::from(area.width) < hints.width().saturating_add(12) {
        compact
    } else {
        hints
    };
    let hint_width = u16::try_from(hints.width().saturating_add(2)).unwrap_or(u16::MAX);
    let [status, shortcuts] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(hint_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme::canvas()),
        status,
    );
    frame.render_widget(
        Paragraph::new(format!(" {hints} "))
            .right_aligned()
            .style(theme::muted()),
        shortcuts,
    );
}
