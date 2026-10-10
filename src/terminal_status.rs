use std::io::{self, Write};

// OSC 7501 revision 0.3 permits reports without a capability query. Terminals
// that do not implement it ignore the OSC and still receive the ordinary bell.
// The fixed base64 message is "New Slack messages"; no workspace content is
// exposed in terminal metadata or interpolated into an escape sequence.
const NEW_CHAT: &[u8] =
    b"\x07\x1b]7501;state=done:id=slack-tooy:app=slack-tooy:msg=TmV3IFNsYWNrIG1lc3NhZ2Vz\x1b\\";
const CLEAR: &[u8] = b"\x1b]7501;state=clear:id=slack-tooy\x1b\\";

pub fn notify(writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(NEW_CHAT)?;
    writer.flush()
}

pub fn clear(writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(CLEAR)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use crate::terminal_status::{clear, notify};

    #[test]
    fn reports_and_clears_only_the_owned_record() -> color_eyre::Result<()> {
        use color_eyre::eyre::ensure;

        let mut bytes = Vec::new();
        notify(&mut bytes)?;
        clear(&mut bytes)?;
        ensure!(bytes == b"\x07\x1b]7501;state=done:id=slack-tooy:app=slack-tooy:msg=TmV3IFNsYWNrIG1lc3NhZ2Vz\x1b\\\x1b]7501;state=clear:id=slack-tooy\x1b\\");
        Ok(())
    }
}
