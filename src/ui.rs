use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Mode};
use crate::components::Component;

/// Slack brand aubergine (dark purple).
const SLACK_AUBERGINE: Color = Color::Rgb(74, 21, 75);

pub fn render(frame: &mut Frame, app: &mut App) {
    let [main_area, status_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    let [sidebar_area, right_area] = Layout::horizontal([
        Constraint::Ratio(app.config.sidebar_width, 12),
        Constraint::Fill(1),
    ])
    .areas(main_area);

    // Grow input box to fit rendered content (visual rows + borders), capped at 8.
    let input_inner_width = right_area.width.saturating_sub(2);
    let input_height = (app.input.visual_line_count(input_inner_width) + 2).min(8);
    let [messages_area, input_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(input_height)]).areas(right_area);

    app.sidebar
        .render(frame, sidebar_area, app.focus == Focus::Sidebar);
    app.messages
        .render(frame, messages_area, app.focus == Focus::Messages);
    app.input
        .render(frame, input_area, app.mode == Mode::Insert);

    render_status_bar(frame, status_area, app);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    // App badge — Slack brand: white on aubergine
    spans.push(Span::styled(
        " SLACK-TOOY ",
        Style::default().fg(Color::White).bg(SLACK_AUBERGINE).bold(),
    ));

    // Mode indicator — only show in INSERT mode (vim convention)
    if app.mode == Mode::Insert {
        spans.push(" INSERT ".bold().on_green());
    }

    // Focus indicator
    let focus_label = match app.focus {
        Focus::Sidebar => " Channels ",
        Focus::Messages => " Messages ",
    };
    spans.push(Span::from(focus_label).dim());

    // Channel / thread context
    if let Some(ref id) = app.current_channel_id {
        let name = app.messages.channel_name.as_str();
        if name.is_empty() {
            spans.push(Span::from(format!(" {id} ")).dim());
        } else {
            spans.push(Span::from(format!(" #{name} ")).cyan());
        }
        if app.messages.active_thread.is_some() {
            spans.push(Span::from(" \u{21B3} Thread ").fg(ratatui::style::Color::Blue));
        }
    }

    // Status / error
    if let Some(ref msg) = app.status_message {
        spans.push(format!(" {msg} ").yellow());
    } else if app.loading {
        spans.push(" Loading... ".yellow());
    }

    // Spacer (push hints to the right)
    let used: usize = spans.iter().map(Span::width).sum();
    let remaining = (area.width as usize).saturating_sub(used);

    // Context-aware hints
    let hints = match app.mode {
        Mode::Insert => "Esc:normal Enter:send Opt/Shift+Enter:newline",
        Mode::Normal => match app.focus {
            Focus::Sidebar => {
                "q:quit i:insert Tab:messages Enter:open R:read-all u:unread j/k:\u{2195}"
            }
            Focus::Messages => {
                if app.messages.active_thread.is_some() {
                    "q:quit i:reply h/\u{2190}:close J/K:msg j/k:line"
                } else {
                    "q:quit i:insert Tab:channels l/\u{2192}:thread J/K:msg j/k:line"
                }
            }
        },
    };

    // +2 for the " … " wrapper spaces around the hint text.
    let hint_width = UnicodeWidthStr::width(hints) + 2;
    if remaining > hint_width {
        let pad = remaining - hint_width;
        spans.push(Span::from(" ".repeat(pad)));
    }
    spans.push(Span::from(format!(" {hints} ")).dim());

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}
