use std::io::{self, Read};

use thiserror::Error;

/// Maximum encoded RFC 5322 message size accepted by the default reader.
pub const MESSAGE_BYTES_LIMIT: usize = 64 * 1024 * 1024;
/// Maximum raw-source bytes retained for a text preview.
pub const RAW_TEXT_LIMIT: usize = 4 * 1024 * 1024;
/// Maximum header bytes retained for a text preview.
pub const HEADER_TEXT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum MessageIoError {
    #[error("reading message data: {0}")]
    Io(#[from] io::Error),
    #[error("message exceeds the {limit}-byte safety limit")]
    TooLarge { limit: usize },
}

/// A binary-tolerant, explicitly bounded text representation of message data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedText {
    /// Retained source bytes converted to UTF-8 for display.
    pub text: String,
    /// Number of source bytes represented by `text`, before lossy conversion.
    pub bytes_read: usize,
    /// Whether more source bytes existed beyond the retained prefix.
    pub truncated: bool,
    /// Whether invalid UTF-8 or an embedded NUL was replaced for display.
    pub lossy: bool,
}

/// Read an RFC 5322 message without allowing the input allocation to grow without bound.
pub fn read_message_bytes(reader: impl Read) -> Result<Vec<u8>, MessageIoError> {
    read_message_bytes_with_limit(reader, MESSAGE_BYTES_LIMIT)
}

/// Read an RFC 5322 message with a caller-selected encoded-byte limit.
pub fn read_message_bytes_with_limit(
    reader: impl Read,
    limit: usize,
) -> Result<Vec<u8>, MessageIoError> {
    let (bytes, truncated) = read_prefix(reader, limit)?;
    if truncated {
        Err(MessageIoError::TooLarge { limit })
    } else {
        Ok(bytes)
    }
}

/// Read a binary-tolerant raw-source preview using [`RAW_TEXT_LIMIT`].
pub fn read_raw_text(reader: impl Read) -> Result<BoundedText, MessageIoError> {
    read_raw_text_with_limit(reader, RAW_TEXT_LIMIT)
}

/// Read a binary-tolerant raw-source preview with a caller-selected limit.
pub fn read_raw_text_with_limit(
    reader: impl Read,
    limit: usize,
) -> Result<BoundedText, MessageIoError> {
    let (bytes, truncated) = read_prefix(reader, limit)?;
    Ok(display_text(bytes, truncated))
}

/// Read a binary-tolerant header preview using [`HEADER_TEXT_LIMIT`].
///
/// Reading stops at the first RFC 5322 header/body separator. A message with a
/// very large body therefore does not require reading or retaining that body.
pub fn read_header_text(reader: impl Read) -> Result<BoundedText, MessageIoError> {
    read_header_text_with_limit(reader, HEADER_TEXT_LIMIT)
}

/// Read a binary-tolerant header preview with a caller-selected limit.
pub fn read_header_text_with_limit(
    mut reader: impl Read,
    limit: usize,
) -> Result<BoundedText, MessageIoError> {
    const CHUNK_SIZE: usize = 8 * 1024;

    let mut retained = Vec::with_capacity(limit.min(CHUNK_SIZE));
    let mut chunk = [0_u8; CHUNK_SIZE];
    loop {
        let count = match reader.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(display_text(retained, false));
        }

        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(count);
        retained.extend_from_slice(&chunk[..keep]);
        if let Some(header_end) = header_end(&retained) {
            retained.truncate(header_end);
            return Ok(display_text(retained, false));
        }

        if keep < count {
            return Ok(display_text(retained, true));
        }
        if retained.len() == limit {
            let mut extra = [0_u8; 1];
            let has_more = loop {
                match reader.read(&mut extra) {
                    Ok(0) => break false,
                    Ok(_) => break true,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            };
            return Ok(display_text(retained, has_more));
        }
    }
}

fn read_prefix(mut reader: impl Read, limit: usize) -> Result<(Vec<u8>, bool), io::Error> {
    let requested = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .by_ref()
        .take(u64::try_from(requested).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, truncated))
}

fn display_text(bytes: Vec<u8>, truncated: bool) -> BoundedText {
    let bytes_read = bytes.len();
    let invalid_utf8 = std::str::from_utf8(&bytes).is_err();
    let embedded_nul = bytes.contains(&0);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if embedded_nul {
        text = text.replace('\0', "\u{fffd}");
    }
    BoundedText {
        text,
        bytes_read,
        truncated,
        lossy: invalid_utf8 || embedded_nul,
    }
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    match (crlf, lf) {
        (Some(crlf), Some(lf)) => Some(crlf.min(lf)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};

    use super::*;

    #[test]
    fn bounded_message_reader_rejects_one_byte_over_limit() {
        assert_eq!(
            read_message_bytes_with_limit(Cursor::new(b"1234"), 4).expect("at limit"),
            b"1234"
        );
        let error = read_message_bytes_with_limit(Cursor::new(b"12345"), 4)
            .expect_err("over limit")
            .to_string();
        assert!(error.contains("4-byte safety limit"), "{error}");
    }

    #[test]
    fn raw_text_is_lossy_and_explicitly_truncated() {
        let preview =
            read_raw_text_with_limit(Cursor::new(b"ok\xff\0tail"), 5).expect("read raw preview");

        assert_eq!(preview.text, "ok\u{fffd}\u{fffd}t");
        assert_eq!(preview.bytes_read, 5);
        assert!(preview.truncated);
        assert!(preview.lossy);
    }

    #[test]
    fn header_reader_stops_before_a_huge_body() {
        struct FailAfterHeader {
            cursor: Cursor<Vec<u8>>,
            maximum_read: usize,
        }

        impl Read for FailAfterHeader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.cursor.position() as usize >= self.maximum_read {
                    return Err(io::Error::other("body must not be read"));
                }
                let remaining = self.maximum_read - self.cursor.position() as usize;
                let count = buffer.len().min(remaining);
                self.cursor.read(&mut buffer[..count])
            }
        }

        let source =
            b"Subject: bounded\r\nX-Test: yes\r\n\r\nbody that must remain unread".to_vec();
        let separator_end = source
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("separator")
            + 4;
        let preview = read_header_text_with_limit(
            FailAfterHeader {
                cursor: Cursor::new(source),
                maximum_read: separator_end,
            },
            1024,
        )
        .expect("read headers only");

        assert_eq!(preview.text, "Subject: bounded\r\nX-Test: yes");
        assert!(!preview.truncated);
        assert!(!preview.lossy);
    }

    #[test]
    fn oversized_or_binary_headers_return_bounded_display_text() {
        let preview = read_header_text_with_limit(Cursor::new(b"Subject: \xff\0abcdef"), 12)
            .expect("read bounded headers");

        assert_eq!(preview.bytes_read, 12);
        assert!(preview.truncated);
        assert!(preview.lossy);
        assert!(preview.text.starts_with("Subject: "));
    }

    #[test]
    fn header_exactly_at_limit_is_not_reported_as_truncated() {
        let preview = read_header_text_with_limit(Cursor::new(b"Subject: x"), 10)
            .expect("read exact-length header");

        assert_eq!(preview.text, "Subject: x");
        assert!(!preview.truncated);
    }
}
