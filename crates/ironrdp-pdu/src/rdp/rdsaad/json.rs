//! Minimal JSON ([RFC 8259]) support for the RDS AAD Auth PDUs.
//!
//! The PDUs carry a single top-level JSON object with one string or number member each, so a
//! general-purpose JSON library would be a heavy dependency for this core-tier crate. The reader
//! accepts the full RFC 8259 grammar (so a peer adding members or nesting does not break us), but
//! only surfaces the top-level members. Nested values are validated and skipped.
//!
//! [RFC 8259]: https://www.rfc-editor.org/rfc/rfc8259

use ironrdp_core::WriteCursor;

/// Maximum nesting depth, the top-level object included.
///
/// INVARIANT: `depth < MAX_DEPTH` for every container opened while parsing, which bounds recursion.
const MAX_DEPTH: usize = 16;

/// Value of a top-level member.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Value<'a> {
    String(String),
    /// Raw number text, validated against the RFC 8259 grammar.
    Number(&'a str),
    /// `true`, `false`, `null`, or a nested object or array.
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Error {
    pub(super) reason: &'static str,
    /// Byte offset into the parsed text.
    pub(super) offset: usize,
}

/// Parses `text` as a single JSON object and returns its members in document order.
///
/// Duplicate member names are rejected: RFC 8259 leaves their meaning undefined, and silently
/// picking one would let two parsers disagree on the same message.
pub(super) fn parse_object(text: &str) -> Result<Vec<(String, Value<'_>)>, Error> {
    let mut parser = Parser { text, pos: 0 };

    parser.skip_whitespace();
    let members = parser.object_members()?;
    parser.skip_whitespace();

    if parser.pos < text.len() {
        return Err(parser.error("trailing data after JSON object"));
    }

    Ok(members)
}

/// Number of bytes [`write_string`] emits for `value`, including the surrounding quotes.
pub(super) fn string_size(value: &str) -> usize {
    let escaped: usize = value
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0C => 2,
            0x00..=0x1F => 6,
            _ => 1,
        })
        .sum();

    1 /* opening quote */ + escaped + 1 /* closing quote */
}

/// Writes `value` as a JSON string literal, including the surrounding quotes.
///
/// The caller must have checked that `dst` holds at least [`string_size`] bytes.
pub(super) fn write_string(dst: &mut WriteCursor<'_>, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    dst.write_u8(b'"');

    for b in value.bytes() {
        match b {
            b'"' => dst.write_slice(b"\\\""),
            b'\\' => dst.write_slice(b"\\\\"),
            b'\n' => dst.write_slice(b"\\n"),
            b'\r' => dst.write_slice(b"\\r"),
            b'\t' => dst.write_slice(b"\\t"),
            0x08 => dst.write_slice(b"\\b"),
            0x0C => dst.write_slice(b"\\f"),
            0x00..=0x1F => {
                dst.write_slice(b"\\u00");
                dst.write_u8(HEX[usize::from(b >> 4)]);
                dst.write_u8(HEX[usize::from(b & 0x0F)]);
            }
            _ => dst.write_u8(b),
        }
    }

    dst.write_u8(b'"');
}

struct Parser<'a> {
    text: &'a str,
    /// INVARIANT: `pos <= text.len()` and `pos` is on a UTF-8 character boundary.
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, reason: &'static str) -> Error {
        Error {
            reason,
            offset: self.pos,
        }
    }

    /// Returns `text[start..end]`; both ends must be character boundaries.
    fn slice(&self, start: usize, end: usize) -> Result<&'a str, Error> {
        self.text.get(start..end).ok_or(Error {
            reason: "not a character boundary",
            offset: start,
        })
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), Error> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(reason))
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn object_members(&mut self) -> Result<Vec<(String, Value<'a>)>, Error> {
        let mut members: Vec<(String, Value<'a>)> = Vec::new();

        self.expect(b'{', "expected JSON object")?;
        self.skip_whitespace();

        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(members);
        }

        loop {
            let name_offset = self.pos;
            let name = self.string()?;

            if members.iter().any(|(existing, _)| *existing == name) {
                return Err(Error {
                    reason: "duplicate member name",
                    offset: name_offset,
                });
            }

            self.skip_whitespace();
            self.expect(b':', "expected ':' after member name")?;
            self.skip_whitespace();

            let value = match self.peek() {
                Some(b'"') => Value::String(self.string()?),
                Some(b'-' | b'0'..=b'9') => Value::Number(self.number()?),
                _ => {
                    self.skip_value(1)?;
                    Value::Other
                }
            };

            members.push((name, value));

            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_whitespace();
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(members);
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
    }

    /// Validates and skips any value. `depth` counts the enclosing containers.
    fn skip_value(&mut self, depth: usize) -> Result<(), Error> {
        match self.peek() {
            Some(b'"') => {
                self.string()?;
            }
            Some(b'-' | b'0'..=b'9') => {
                self.number()?;
            }
            Some(b't') => self.literal("true")?,
            Some(b'f') => self.literal("false")?,
            Some(b'n') => self.literal("null")?,
            Some(open @ (b'{' | b'[')) => {
                if MAX_DEPTH <= depth {
                    return Err(self.error("JSON nesting too deep"));
                }

                let close = if open == b'{' { b'}' } else { b']' };
                self.pos += 1;
                self.skip_whitespace();

                if self.peek() == Some(close) {
                    self.pos += 1;
                    return Ok(());
                }

                loop {
                    if open == b'{' {
                        self.string()?;
                        self.skip_whitespace();
                        self.expect(b':', "expected ':' after member name")?;
                        self.skip_whitespace();
                    }

                    self.skip_value(depth + 1)?;
                    self.skip_whitespace();

                    match self.peek() {
                        Some(b',') => {
                            self.pos += 1;
                            self.skip_whitespace();
                        }
                        Some(b) if b == close => {
                            self.pos += 1;
                            return Ok(());
                        }
                        _ => return Err(self.error("expected ',' or closing bracket")),
                    }
                }
            }
            _ => return Err(self.error("expected JSON value")),
        }

        Ok(())
    }

    fn literal(&mut self, literal: &'static str) -> Result<(), Error> {
        if self.text.as_bytes()[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }

    fn number(&mut self) -> Result<&'a str, Error> {
        let start = self.pos;

        if self.peek() == Some(b'-') {
            self.pos += 1;
        }

        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("expected digit in number")),
        }

        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected digit after decimal point"));
            }
            self.digits();
        }

        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected digit in exponent"));
            }
            self.digits();
        }

        // All bytes consumed above are ASCII, so both ends are character boundaries.
        self.slice(start, self.pos)
    }

    fn digits(&mut self) {
        while let Some(b'0'..=b'9') = self.peek() {
            self.pos += 1;
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.expect(b'"', "expected string")?;

        let mut out = String::new();

        loop {
            // Copy the longest run that needs no unescaping in one go.
            let run = self.text.as_bytes()[self.pos..]
                .iter()
                .position(|&b| b == b'"' || b == b'\\' || b < 0x20)
                .ok_or(Error {
                    reason: "unterminated string",
                    offset: self.text.len(),
                })?;
            // The stop bytes are ASCII, so the run ends on a character boundary.
            out.push_str(self.slice(self.pos, self.pos + run)?);
            self.pos += run;

            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{08}',
                        Some(b'f') => '\u{0C}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            let escape_offset = self.pos;
                            let unit = self.hex4()?;
                            let code_point = match unit {
                                0xD800..=0xDBFF => {
                                    if !self.text.as_bytes()[self.pos..].starts_with(b"\\u") {
                                        return Err(self.error("unpaired surrogate in string"));
                                    }
                                    self.pos += 2;
                                    let low = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err(self.error("unpaired surrogate in string"));
                                    }
                                    0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
                                }
                                0xDC00..=0xDFFF => {
                                    return Err(Error {
                                        reason: "unpaired surrogate in string",
                                        offset: escape_offset,
                                    });
                                }
                                _ => u32::from(unit),
                            };
                            // Surrogates are excluded above, so every remaining value is a scalar value.
                            let c = char::from_u32(code_point).ok_or(Error {
                                reason: "invalid unicode escape",
                                offset: escape_offset,
                            })?;
                            out.push(c);
                            continue;
                        }
                        _ => return Err(self.error("invalid escape sequence")),
                    };
                    self.pos += 1;
                    out.push(escaped);
                }
                _ => return Err(self.error("unescaped control character in string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, Error> {
        let digits = self
            .text
            .as_bytes()
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("truncated unicode escape"))?;

        let mut value: u16 = 0;
        for &digit in digits {
            let nibble = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return Err(self.error("invalid unicode escape")),
            };
            value = (value << 4) | u16::from(nibble);
        }

        self.pos += 4;
        Ok(value)
    }
}
