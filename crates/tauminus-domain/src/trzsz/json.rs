// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The two JSON objects of the handshake: the terminal's `#ACT`, written
//! here, and the host's `#CFG`, read with a small reader of its own (the
//! domain takes no serde), bounded in size and depth.

use std::collections::BTreeMap;
use std::time::Duration;

use super::TrzszError;

/// Most bytes of `#CFG` JSON read.
const MAX_JSON: usize = 64 << 10;
const MAX_DEPTH: usize = 32;
/// Most bytes the host may send per chunk.
pub const MAX_BUFSIZE: u64 = 10 << 20;
const DEFAULT_BUFSIZE: u64 = 10 << 20;

/// The terminal's answer to a trigger: protocol 1, base64, files only, no
/// tunnel, fork or tmux control mode (ADR 0031). `confirm` false says the
/// user declined.
#[must_use]
pub fn action(confirm: bool) -> String {
    format!(
        "{{\"lang\":\"rs\",\"version\":\"1.1.0\",\"confirm\":{confirm},\"newline\":\"\\n\",\
         \"protocol\":1,\"binary\":false,\"support_dir\":false,\"tunnel\":false,\
         \"fork\":false,\"tmuxcc\":false}}"
    )
}

/// What the host's `#CFG` sets that this side follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// How long one line may take, held to 5 to 60 s.
    pub timeout: Duration,
    /// Most bytes per chunk the host takes.
    pub bufsize: u64,
    /// tmux runs on the host.
    pub junk: bool,
}

impl Config {
    /// The host's `#CFG` JSON, checked against what this side said.
    ///
    /// # Errors
    /// Not a JSON object, or asking for what this side does not do:
    /// binary, folders, another newline or protocol, or a chunk past
    /// [`MAX_BUFSIZE`].
    pub fn read(json: &[u8]) -> Result<Self, TrzszError> {
        if json.len() > MAX_JSON {
            return Err(TrzszError::TooLong(MAX_JSON));
        }
        let fields = object(json)?;
        let unsupported = |what: &str| Err(TrzszError::Unsupported(what.into()));
        let flag = |key: &str| matches!(fields.get(key), Some(Value::Bool(true)));
        if flag("binary") {
            return unsupported("binary transfers");
        }
        if flag("directory") {
            return unsupported("folders");
        }
        match fields.get("newline") {
            None | Some(Value::Null) => {}
            Some(Value::Text(newline)) if newline == "\n" => {}
            Some(_) => return unsupported("a Windows host's line ends"),
        }
        match fields.get("protocol") {
            None | Some(Value::Null) => {}
            Some(Value::Number(n)) if *n == 0 || *n == 1 => {}
            Some(_) => return unsupported("another protocol version"),
        }
        let timeout = match fields.get("timeout") {
            Some(Value::Number(n)) if *n <= 0 => 60,
            Some(Value::Number(n)) => (*n).clamp(5, 60),
            _ => 20,
        };
        let bufsize = match fields.get("bufsize") {
            Some(Value::Number(n)) if *n > 0 => n.unsigned_abs(),
            Some(Value::Number(_)) => return Err(TrzszError::Protocol("a chunk size".into())),
            _ => DEFAULT_BUFSIZE,
        };
        if bufsize > MAX_BUFSIZE {
            return unsupported("chunks over 10 MiB (tsz -B)");
        }
        Ok(Self {
            timeout: Duration::from_secs(timeout.unsigned_abs()),
            bufsize,
            junk: flag("tmux_output_junk"),
        })
    }
}

/// A value of the top-level object; nested ones are read and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Null,
    Bool(bool),
    /// An integer; a number with a fraction or exponent is `Other`.
    Number(i64),
    Text(String),
    Other,
}

/// The top-level object's fields. O(n).
fn object(json: &[u8]) -> Result<BTreeMap<String, Value>, TrzszError> {
    let mut reader = Reader { json, at: 0 };
    reader.space();
    let mut fields = BTreeMap::new();
    reader.expect(b'{')?;
    reader.space();
    if !reader.eat(b'}') {
        loop {
            reader.space();
            let key = reader.text()?;
            reader.space();
            reader.expect(b':')?;
            let value = reader.value(1)?;
            fields.insert(key, value);
            reader.space();
            if reader.eat(b'}') {
                break;
            }
            reader.expect(b',')?;
        }
    }
    reader.space();
    if reader.at != json.len() {
        return Err(bad());
    }
    Ok(fields)
}

fn bad() -> TrzszError {
    TrzszError::Protocol("the host's configuration".into())
}

struct Reader<'a> {
    json: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.json.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    fn expect(&mut self, byte: u8) -> Result<(), TrzszError> {
        if self.eat(byte) { Ok(()) } else { Err(bad()) }
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, TrzszError> {
        if depth > MAX_DEPTH {
            return Err(bad());
        }
        self.space();
        match self.peek().ok_or_else(bad)? {
            b'"' => self.text().map(Value::Text),
            b'{' => {
                self.at += 1;
                self.space();
                if !self.eat(b'}') {
                    loop {
                        self.space();
                        self.text()?;
                        self.space();
                        self.expect(b':')?;
                        self.value(depth + 1)?;
                        self.space();
                        if self.eat(b'}') {
                            break;
                        }
                        self.expect(b',')?;
                    }
                }
                Ok(Value::Other)
            }
            b'[' => {
                self.at += 1;
                self.space();
                if !self.eat(b']') {
                    loop {
                        self.value(depth + 1)?;
                        self.space();
                        if self.eat(b']') {
                            break;
                        }
                        self.expect(b',')?;
                    }
                }
                Ok(Value::Other)
            }
            b't' => self.word(b"true", Value::Bool(true)),
            b'f' => self.word(b"false", Value::Bool(false)),
            b'n' => self.word(b"null", Value::Null),
            b'-' | b'0'..=b'9' => Ok(self.number()),
            _ => Err(bad()),
        }
    }

    fn word(&mut self, word: &[u8], value: Value) -> Result<Value, TrzszError> {
        if self.json[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(bad())
        }
    }

    fn number(&mut self) -> Value {
        let start = self.at;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.at += 1;
        }
        std::str::from_utf8(&self.json[start..self.at])
            .ok()
            .and_then(|text| text.parse().ok())
            .map_or(Value::Other, Value::Number)
    }

    fn text(&mut self) -> Result<String, TrzszError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek(), Some(b'"' | b'\\') | None) {
                self.at += 1;
            }
            out.push_str(std::str::from_utf8(&self.json[start..self.at]).map_err(|_| bad())?);
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escaped = self.peek().ok_or_else(bad)?;
                    self.at += 1;
                    out.push(match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode()?,
                        _ => return Err(bad()),
                    });
                }
                _ => return Err(bad()),
            }
        }
    }

    /// The character of a `\uXXXX`, its low surrogate read after it.
    fn unicode(&mut self) -> Result<char, TrzszError> {
        let high = self.hex()?;
        if (0xD800..0xDC00).contains(&high) {
            if !self.json[self.at..].starts_with(b"\\u") {
                return Err(bad());
            }
            self.at += 2;
            let low = self.hex()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(bad());
            }
            let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
            return char::from_u32(code).ok_or_else(bad);
        }
        char::from_u32(high).ok_or_else(bad)
    }

    fn hex(&mut self) -> Result<u32, TrzszError> {
        let digits = self.json.get(self.at..self.at + 4).ok_or_else(bad)?;
        self.at += 4;
        let text = std::str::from_utf8(digits).map_err(|_| bad())?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        u32::from_str_radix(text, 16).map_err(|_| bad())
    }
}
