//! The lossless escape a dialog shows model-produced bytes through.
//!
//! Every byte the executor will receive is recoverable from what is displayed, and anything
//! that could hide — control characters, format and bidi characters, invalid UTF-8, and
//! every non-ASCII code point, since a homoglyph hides as well as U+202E does — is made
//! visible. Printable ASCII is shown as itself, a newline as a newline, `\` as `\\`, any
//! other Unicode scalar as `\u{XXXX}`, and a byte that is not part of a valid UTF-8 sequence
//! as `\x{HH}`. [`unescape`] inverts [`escape`] exactly, and the round trip is a test.

use std::fmt::Write as _;

/// Renders bytes as displayable text. Nothing is dropped and nothing is summarised.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                escape_str(s, &mut out);
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                // `valid` is valid by construction.
                escape_str(std::str::from_utf8(valid).unwrap_or(""), &mut out);
                let bad = e.error_len().unwrap_or(after.len());
                for b in &after[..bad] {
                    let _ = write!(out, "\\x{{{b:02X}}}");
                }
                rest = &after[bad..];
            }
        }
    }
    out
}

fn escape_str(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push('\n'),
            ' '..='~' => out.push(c),
            _ => {
                let _ = write!(out, "\\u{{{:X}}}", c as u32);
            }
        }
    }
}

/// The error [`unescape`] returns for text that [`escape`] could not have produced.
#[derive(Debug, PartialEq, Eq)]
pub struct MalformedEscape;

/// Recovers the bytes [`escape`] was given.
pub fn unescape(text: &str) -> Result<Vec<u8>, MalformedEscape> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\\') => out.push(b'\\'),
                Some(kind @ ('u' | 'x')) => {
                    if chars.next() != Some('{') {
                        return Err(MalformedEscape);
                    }
                    let mut hex = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(h) if h.is_ascii_hexdigit() => hex.push(h),
                            _ => return Err(MalformedEscape),
                        }
                    }
                    let n = u32::from_str_radix(&hex, 16).map_err(|_| MalformedEscape)?;
                    if kind == 'x' {
                        out.push(u8::try_from(n).map_err(|_| MalformedEscape)?);
                    } else {
                        let ch = char::from_u32(n).ok_or(MalformedEscape)?;
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                }
                _ => return Err(MalformedEscape),
            },
            '\n' => out.push(b'\n'),
            ' '..='~' => out.push(c as u8),
            _ => return Err(MalformedEscape),
        }
    }
    Ok(out)
}
