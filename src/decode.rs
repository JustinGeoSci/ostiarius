//  src/decode.rs

//! Saved logs arrive in more than one encoding, and this module turns their
//! bytes into text.
//!
//! A log saved by Windows PowerShell 5.1 with `>` or `Out-File` is UTF-16LE
//! with a byte-order mark (BOM), while a log saved by `cmd`, a POSIX shell, or
//! a CI system is UTF-8, sometimes with a BOM of its own. The mark decides the
//! encoding, and bytes without one are read as UTF-8. Undecodable bytes are
//! replaced rather than treated as fatal, so a log truncated mid-character can
//! still be read. The replacement is reported, never silent.

// ===========================================================================
// Decoding
// ===========================================================================

/// Decodes a log, returning the text and whether any bytes had to be replaced.
pub(crate) fn decode(bytes: &[u8]) -> (String, bool) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return utf8(rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    utf8(bytes)
}

fn utf8(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> (String, bool) {
    // An odd trailing byte cannot be part of any code unit, so it is dropped
    // and counted as a replacement.
    let mut lossy = bytes.len() % 2 == 1;
    let units = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]]));
    let mut text = String::with_capacity(bytes.len() / 2);
    for decoded in char::decode_utf16(units) {
        if let Ok(c) = decoded {
            text.push(c);
        } else {
            text.push(char::REPLACEMENT_CHARACTER);
            lossy = true;
        }
    }
    (text, lossy)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::decode;

    fn utf16le(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn utf8_plain() {
        assert_eq!(
            decode(b"test result: ok."),
            ("test result: ok.".to_owned(), false)
        );
    }

    #[test]
    fn utf8_bom_stripped() {
        assert_eq!(decode(b"\xEF\xBB\xBFabc"), ("abc".to_owned(), false));
    }

    #[test]
    fn utf16le_bom() {
        assert_eq!(
            decode(&utf16le("r\u{e9}sultat\r\n")),
            ("r\u{e9}sultat\r\n".to_owned(), false)
        );
    }

    #[test]
    fn utf16be_bom() {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "ok".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode(&bytes), ("ok".to_owned(), false));
    }

    #[test]
    fn invalid_utf8_is_lossy_and_flagged() {
        let (text, lossy) = decode(b"ok \xFF end");
        assert!(lossy);
        assert_eq!(text, "ok \u{FFFD} end");
    }

    #[test]
    fn odd_length_utf16_is_lossy() {
        let mut bytes = utf16le("ok");
        bytes.push(0x41);
        let (text, lossy) = decode(&bytes);
        assert!(lossy);
        assert_eq!(text, "ok");
    }

    #[test]
    fn unpaired_surrogate_is_lossy() {
        let bytes = [0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00];
        let (text, lossy) = decode(&bytes);
        assert!(lossy);
        assert_eq!(text, "\u{FFFD}A");
    }
}
