use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders};

pub const CANVAS: Color = Color::Rgb(20, 20, 18);
pub const SURFACE: Color = Color::Rgb(27, 26, 23);
pub const SURFACE_RAISED: Color = Color::Rgb(35, 34, 30);
pub const SELECTED: Color = Color::Rgb(45, 43, 37);
pub const BORDER_MUTED: Color = Color::Rgb(47, 45, 40);
pub const TEXT: Color = Color::Rgb(224, 220, 210);
pub const TEXT_MUTED: Color = Color::Rgb(145, 140, 128);
pub const BLUE: Color = Color::Rgb(145, 184, 204);
pub const GREEN: Color = Color::Rgb(150, 190, 150);
pub const YELLOW: Color = Color::Rgb(211, 178, 109);
pub const RED: Color = Color::Rgb(213, 132, 126);
pub const MAGENTA: Color = Color::Rgb(194, 151, 188);

pub fn panel_border(focused: bool) -> Style {
    let color = if focused { BLUE } else { BORDER_MUTED };
    Style::default().fg(color)
}

pub fn panel_block(title: Line<'_>, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(panel_border(focused))
        .style(Style::default().bg(SURFACE))
        .title(title)
}

pub fn surface() -> Style {
    Style::default().fg(TEXT).bg(SURFACE)
}

pub fn canvas() -> Style {
    Style::default().fg(TEXT).bg(CANVAS)
}

pub fn selected() -> Style {
    Style::default().fg(TEXT).bg(SELECTED)
}

pub fn muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}
