//! A read-only JSON parser for the CLI's stream-json lines, and a writer for the one
//! place the backend builds a line to send.
//!
//! Written here rather than taken from a crate because the core links no serialisation
//! library on purpose: `crates/core/src/lib.rs` rests the consent token's
//! non-serialisability on that absence, and `.github/workflows/build.yml` fails the build
//! if `serde`, `serde_json` or a peer appears in the core's dependency tree. A parser that
//! produces a [`Value`] tree gives the gate nothing: no derive, no trait a token could
//! implement. It is the whole of RFC 8259 minus what the CLI never sends and the backend
//! never reads into — numbers are kept as `f64`, which is exact for every token count and
//! millisecond the CLI reports and is only ever read back as an integer or a cost.
//!
//! Input is bytes the CLI wrote, on a process the user runs under their own account; the
//! parser is still bounded — depth-limited, and every failure is an error rather than a
//! panic — because a backend that can be crashed by its own subprocess's output is a
//! backend that cannot report the crash.

use std::fmt;

/// A parsed JSON value. Objects keep their members in order and allow duplicates; `get`
/// returns the first, which is what the CLI's own reader does with the lines it accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset the parser stopped at.
    pub at: usize,
    pub what: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

/// Deeper than any stream-json line the CLI emits, and shallow enough that the recursive
/// descent below stays well inside a thread's stack.
const MAX_DEPTH: usize = 64;

impl Value {
    pub fn parse(text: &str) -> Result<Value, ParseError> {
        let mut p = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        p.skip_ws();
        let value = p.value(0)?;
        p.skip_ws();
        if p.at != p.bytes.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(value)
    }

    /// The member `key` of an object, or `None` for a missing member and for a non-object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A non-negative integral number. A count the CLI reports as `1.5` or `-1` is not a
    /// count, and reads as absent.
    pub fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= u64::MAX as f64 {
            Some(n as u64)
        } else {
            None
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// `self` as one JSON text, compact, with every string escaped so the result is a
    /// single line. The inverse of [`Value::parse`] up to number formatting.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out);
        out
    }
}

pub fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(value: &Value, out: &mut String) {
    use std::fmt::Write as _;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_finite() {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    let _ = write!(out, "{}", *n as i64);
                } else {
                    let _ = write!(out, "{n}");
                }
            } else {
                out.push_str("null");
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (i, (k, v)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value(v, out);
            }
            out.push('}');
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> ParseError {
        ParseError { at: self.at, what }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), ParseError> {
        if self.peek() == Some(b) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.err("unexpected character"))
        }
    }

    fn literal(&mut self, word: &[u8], value: Value) -> Result<Value, ParseError> {
        if self.bytes[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.err("unexpected character"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.peek() {
            None => Err(self.err("unexpected end")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.expect(b'{')?;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected a member name"));
            }
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(members));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.err("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("expected a digit"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("expected a digit"));
            }
            self.digits();
        }
        // The slice is ASCII by construction, and a grammar-valid number always parses.
        let text = std::str::from_utf8(&self.bytes[start..self.at]).expect("ascii");
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.err("number out of range"))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            // Copy the longest run with nothing to decode in one step.
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.at += 1;
            }
            // The input is a `&str`, and the run stopped on an ASCII byte, so the run is
            // on a character boundary at both ends.
            out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).expect("utf-8"));
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.err("control character in string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), ParseError> {
        let c = match self.peek() {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                self.at += 1;
                let first = self.hex4()?;
                let c = if (0xD800..0xDC00).contains(&first) {
                    // A high surrogate must be followed by an escaped low one.
                    if !self.bytes[self.at..].starts_with(b"\\u") {
                        return Err(self.err("lone surrogate"));
                    }
                    self.at += 2;
                    let second = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&second) {
                        return Err(self.err("lone surrogate"));
                    }
                    let cp = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                    char::from_u32(cp).ok_or_else(|| self.err("lone surrogate"))?
                } else {
                    char::from_u32(first).ok_or_else(|| self.err("lone surrogate"))?
                };
                out.push(c);
                return Ok(());
            }
            _ => return Err(self.err("bad escape")),
        };
        self.at += 1;
        out.push(c);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let Some(hex) = self.bytes.get(self.at..self.at + 4) else {
            return Err(self.err("bad escape"));
        };
        let text = std::str::from_utf8(hex).map_err(|_| self.err("bad escape"))?;
        let n = u32::from_str_radix(text, 16).map_err(|_| self.err("bad escape"))?;
        self.at += 4;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::String(text.into())
    }

    #[test]
    fn parses_the_shapes_the_cli_sends() {
        let v = Value::parse(
            r#"{"type":"result","is_error":false,"total_cost_usd":0.0123,"usage":{"input_tokens":10,"output_tokens":0},"permission_denials":[],"x":null,"nested":[1,[2,{"k":"v"}]]}"#,
        )
        .unwrap();
        assert_eq!(v.get("type"), Some(&s("result")));
        assert_eq!(v.get("is_error").and_then(Value::as_bool), Some(false));
        assert_eq!(
            v.get("total_cost_usd").and_then(Value::as_f64),
            Some(0.0123)
        );
        assert_eq!(
            v.get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(Value::as_u64),
            Some(10)
        );
        assert_eq!(
            v.get("permission_denials").and_then(Value::as_array),
            Some(&[][..])
        );
        assert_eq!(v.get("x"), Some(&Value::Null));
        assert_eq!(v.get("missing"), None);
        assert_eq!(s("a").get("k"), None);
    }

    #[test]
    fn decodes_escapes_including_surrogate_pairs() {
        let v = Value::parse(r#""a\"b\\c\/\n\té😀""#).unwrap();
        assert_eq!(v.as_str(), Some("a\"b\\c/\n\té😀"));
        assert!(Value::parse(r#""\ud83d""#).is_err());
        assert!(Value::parse(r#""\ud83dx""#).is_err());
        assert!(Value::parse("\"a\nb\"").is_err());
    }

    #[test]
    fn numbers_read_as_counts_only_when_they_are_counts() {
        assert_eq!(Value::parse("42").unwrap().as_u64(), Some(42));
        assert_eq!(Value::parse("-1").unwrap().as_u64(), None);
        assert_eq!(Value::parse("1.5").unwrap().as_u64(), None);
        assert_eq!(Value::parse("1e3").unwrap().as_u64(), Some(1000));
        assert!(Value::parse("01").is_err());
        assert!(Value::parse("1.").is_err());
        assert!(Value::parse("-").is_err());
    }

    #[test]
    fn rejects_what_is_not_json() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":}",
            "{a:1}",
            "tru",
            "nul",
            "1 2",
            "{\"a\":1,}",
        ] {
            assert!(Value::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn depth_is_bounded() {
        let deep = "[".repeat(1000) + &"]".repeat(1000);
        assert_eq!(Value::parse(&deep).unwrap_err().what, "nesting too deep");
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(Value::parse(&ok).is_ok());
    }

    #[test]
    fn writes_what_it_parses() {
        let text = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"a \"q\" \n \\ é"}]},"n":[1,2.5,-3,true,null]}"#;
        let v = Value::parse(text).unwrap();
        let again = v.to_json();
        assert_eq!(Value::parse(&again).unwrap(), v);
        assert!(!again.contains('\n'));
    }

    #[test]
    fn duplicate_members_read_first() {
        let v = Value::parse(r#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_u64), Some(1));
    }
}
