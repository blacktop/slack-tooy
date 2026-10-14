use std::io::{self, Write};

use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_TITLE_BYTES: usize = 192;
const CLEAR: &[u8] = b"\x1b]7501;state=clear:id=slack-tooy\x1b\\";

#[derive(Debug, PartialEq, Eq)]
pub struct ChatNotification {
    record_id: String,
    title: String,
    thread_reply: bool,
}

impl ChatNotification {
    pub fn new(channel_id: &str, label: &str, direct_message: bool) -> Self {
        // Each conversation keeps its own record. Unexpected IDs fall back to
        // our parent record instead of being interpolated into OSC syntax.
        let record_id = if valid_record_segment(channel_id) {
            format!("slack-tooy/{channel_id}")
        } else {
            "slack-tooy".into()
        };
        let prefix = if direct_message { "DM with " } else { "#" };
        let mut title = String::from(prefix);
        title.push_str(&single_line(label, MAX_TITLE_BYTES - prefix.len()));
        Self {
            record_id,
            title,
            thread_reply: false,
        }
    }

    pub fn thread_reply(channel_id: &str, label: &str, direct_message: bool) -> Self {
        let mut notification = Self::new(channel_id, label, direct_message);
        notification.thread_reply = true;
        notification
    }
}

fn valid_record_segment(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn single_line(text: &str, limit: usize) -> String {
    let mut result = String::with_capacity(limit);
    // Bound work as well as output, even if the input is all invisible text.
    for c in text.chars().take(limit) {
        let c = if c.is_control()
            || c.is_whitespace()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            ' '
        } else {
            c
        };
        if c.len_utf8() > limit.saturating_sub(result.len()) {
            break;
        }
        result.push(c);
    }
    result
}

// OSC 7501 permits reports without a capability query. Other terminals ignore
// the OSC and still receive the ordinary bell. Only the conversation name is
// published; message contents stay inside the app.
pub fn notify(writer: &mut impl Write, notification: &ChatNotification) -> io::Result<()> {
    let title = STANDARD.encode(&notification.title);
    let kind = if notification.thread_reply {
        "thread reply"
    } else {
        "message"
    };
    let message = STANDARD.encode(format!("New Slack {kind} in {}", notification.title));
    write!(
        writer,
        "\x07\x1b]7501;state=done:id={}:app=slack-tooy:title={title}:msg={message}\x1b\\",
        notification.record_id
    )?;
    writer.flush()
}

pub fn clear(writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(CLEAR)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use crate::terminal_status::{ChatNotification, MAX_TITLE_BYTES, clear, notify};

    #[test]
    fn reports_and_clears_only_the_owned_record() -> color_eyre::Result<()> {
        use color_eyre::eyre::ensure;

        let mut bytes = Vec::new();
        notify(&mut bytes, &ChatNotification::new("C123", "general", false))?;
        clear(&mut bytes)?;
        ensure!(bytes == b"\x07\x1b]7501;state=done:id=slack-tooy/C123:app=slack-tooy:title=I2dlbmVyYWw=:msg=TmV3IFNsYWNrIG1lc3NhZ2UgaW4gI2dlbmVyYWw=\x1b\\\x1b]7501;state=clear:id=slack-tooy\x1b\\");
        Ok(())
    }

    #[test]
    fn external_labels_and_ids_cannot_escape_protocol_limits() {
        let label = format!("Alice\x1b\x07\u{009c}\u{202e}\n{}", "🦀".repeat(500));
        let notification = ChatNotification::new("D1:state=error", &label, true);
        assert_eq!(notification.record_id, "slack-tooy");
        assert!(notification.title.starts_with("DM with Alice     "));
        assert!(notification.title.len() <= MAX_TITLE_BYTES);
        assert!(!notification.title.chars().any(char::is_control));
    }
}
