use ratatui_image::picker::{Picker, ProtocolType};

/// Query once, after entering the alternate screen and before reading events.
pub fn picker() -> Option<Picker> {
    let mut picker = match Picker::from_query_stdio() {
        Ok(picker) => picker,
        Err(error) => {
            tracing::warn!(%error, "Failed to detect terminal image support");
            return None;
        }
    };

    let detected = picker.protocol_type();
    let protocol = select_protocol(detected, is_rex());
    picker.set_protocol_type(protocol);
    tracing::info!(
        ?detected,
        ?protocol,
        font_size = ?picker.font_size(),
        "Selected terminal image protocol"
    );
    Some(picker)
}

fn is_rex() -> bool {
    std::env::var("TERM").is_ok_and(|term| matches!(term.as_str(), "xterm-rex" | "rex"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program.eq_ignore_ascii_case("rex"))
        || (std::env::var_os("REX_SESSION").is_some_and(|value| !value.is_empty())
            && std::env::var_os("REX_BLOCK").is_some_and(|value| !value.is_empty()))
}

fn select_protocol(detected: ProtocolType, rex: bool) -> ProtocolType {
    // Rex Beta accepts Kitty graphics commands through libghostty, but its
    // client renders ratatui-image's U+10EEEE placeholders as missing glyphs.
    if rex && detected == ProtocolType::Kitty {
        ProtocolType::Halfblocks
    } else {
        detected
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal_images::select_protocol;
    use color_eyre::eyre::ensure;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui_image::StatefulImage;
    use ratatui_image::picker::{Picker, ProtocolType};

    #[test]
    fn rex_uses_colored_cells_instead_of_kitty_placeholders() -> color_eyre::Result<()> {
        let protocol = select_protocol(ProtocolType::Kitty, true);
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(protocol);
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            16,
            image::Rgb([240, 50, 60]),
        ));
        let mut state = picker.new_resize_protocol(image);
        let mut terminal = Terminal::new(TestBackend::new(4, 2))?;
        terminal.draw(|frame| {
            frame.render_stateful_widget(StatefulImage::new(), Rect::new(0, 0, 4, 2), &mut state);
        })?;

        let cells = &terminal.backend().buffer().content;
        ensure!(
            cells.iter().any(|cell| cell.fg == Color::Rgb(240, 50, 60)),
            "missing upper image pixels"
        );
        ensure!(
            cells.iter().any(|cell| cell.bg == Color::Rgb(240, 50, 60)),
            "missing lower image pixels"
        );
        ensure!(
            cells
                .iter()
                .all(|cell| !cell.symbol().contains('\u{10eeee}')),
            "Kitty placeholders leaked into the fallback"
        );
        Ok(())
    }

    #[test]
    fn other_terminals_and_protocols_keep_their_protocol() {
        assert_eq!(
            select_protocol(ProtocolType::Kitty, false),
            ProtocolType::Kitty
        );
        assert_eq!(
            select_protocol(ProtocolType::Sixel, true),
            ProtocolType::Sixel
        );
    }
}
