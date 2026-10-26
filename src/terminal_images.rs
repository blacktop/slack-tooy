use ratatui_image::picker::Picker;

/// Query once, after entering the alternate screen and before reading events.
pub fn picker() -> Option<Picker> {
    let picker = match Picker::from_query_stdio() {
        Ok(picker) => picker,
        Err(error) => {
            tracing::warn!(%error, "Failed to detect terminal image support");
            return None;
        }
    };

    tracing::info!(
        protocol = ?picker.protocol_type(),
        font_size = ?picker.font_size(),
        "Selected terminal image protocol"
    );
    Some(picker)
}
