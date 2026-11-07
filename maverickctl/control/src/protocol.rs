//! Versioned line framing shared with the independent control client.
use std::io::{self, BufRead, Read};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_LINE_LEN: usize = 64 * 1024;
pub const MAX_CMD_LEN: usize = MAX_LINE_LEN;

/// Read one complete UTF-8 frame, bounding allocation before reading. One LF
/// terminates the frame; an optional preceding CR is removed. EOF halfway
/// through a frame is an error, so truncated replies cannot look successful.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_LINE_LEN + 2) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_LINE_LEN + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control line too long",
        ));
    }
    if bytes.pop() != Some(b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete control line",
        ));
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn fragmented_frames_preserve_the_next_command() {
        let mut reader = BufReader::with_capacity(1, Cursor::new("ping\r\nidentify\n"));
        assert_eq!(read_frame(&mut reader).unwrap().as_deref(), Some("ping"));
        assert_eq!(
            read_frame(&mut reader).unwrap().as_deref(),
            Some("identify")
        );
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn full_boundary_is_accepted_but_a_large_unterminated_stream_is_bounded() {
        let valid = format!("{}\n", "x".repeat(MAX_LINE_LEN));
        assert_eq!(
            read_frame(&mut Cursor::new(valid)).unwrap().unwrap().len(),
            MAX_LINE_LEN
        );
        // A synthetic reader can produce arbitrarily much input without
        // allocating it first. The cursor proves the reader consumes a bound.
        let mut input = BufReader::with_capacity(1, std::io::repeat(b'x'));
        assert_eq!(
            read_frame(&mut input).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        for bytes in [&b"ping"[..], &b"\xff\n"[..], &b"\xf0\x9f\n"[..]] {
            assert!(read_frame(&mut Cursor::new(bytes)).is_err());
        }
    }
}
