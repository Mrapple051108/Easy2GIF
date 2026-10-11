//! A tiny JSON reader.
//!
//! The core deliberately has no external crates, so this module implements just
//! enough of RFC 8259 to consume `ffprobe -of json` output.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            // ffprobe sometimes emits numbers as strings.
            Json::Str(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(v) => Some(v),
            _ => None,
        }
    }

    /// Read a string field, treating `"N/A"` as absent.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        match self.get(key).and_then(|v| v.as_str()) {
            Some("N/A") => None,
            other => other,
        }
    }

    pub fn f64_field(&self, key: &str) -> Option<f64> {
        let v = self.get(key)?;
        if v.as_str() == Some("N/A") {
            return None;
        }
        v.as_f64()
    }
}

pub fn parse(text: &str) -> Result<Json> {
    let bytes = text.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    // Trailing content is tolerated: ffprobe output is trusted enough.
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(Error::BadMedia(format!(
                "malformed JSON: expected '{}' at byte {}",
                c as char, self.i
            )))
        }
    }

    fn value(&mut self) -> Result<Json> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(_) => self.number(),
            None => Err(Error::BadMedia("unexpected end of JSON".into())),
        }
    }

    fn literal(&mut self, lit: &str) -> Result<()> {
        if self.b.len() >= self.i + lit.len() && &self.b[self.i..self.i + lit.len()] == lit.as_bytes()
        {
            self.i += lit.len();
            Ok(())
        } else {
            Err(Error::BadMedia(format!("malformed JSON literal at byte {}", self.i)))
        }
    }

    fn object(&mut self) -> Result<Json> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(map));
        }
        loop {
            self.skip_ws();
            let k = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let v = self.value()?;
            map.insert(k, v);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => return Err(Error::BadMedia("malformed JSON object".into())),
            }
        }
        Ok(Json::Obj(map))
    }

    fn array(&mut self) -> Result<Json> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            let v = self.value()?;
            out.push(v);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                _ => return Err(Error::BadMedia("malformed JSON array".into())),
            }
        }
        Ok(Json::Arr(out))
    }

    fn string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut s = String::new();
        loop {
            let c = self
                .bump()
                .ok_or_else(|| Error::BadMedia("unterminated JSON string".into()))?;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self
                        .bump()
                        .ok_or_else(|| Error::BadMedia("bad JSON escape".into()))?;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            // Surrogate pair handling.
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.peek() == Some(b'\\') {
                                    self.i += 1;
                                    self.expect(b'u')?;
                                    let lo = self.hex4()?;
                                    let c = 0x10000
                                        + (((cp - 0xD800) as u32) << 10)
                                        + (lo - 0xDC00) as u32;
                                    if let Some(ch) = char::from_u32(c) {
                                        s.push(ch);
                                    }
                                }
                            } else if let Some(ch) = char::from_u32(cp as u32) {
                                s.push(ch);
                            }
                        }
                        _ => return Err(Error::BadMedia("bad JSON escape".into())),
                    }
                }
                _ => {
                    // Collect the raw UTF-8 sequence.
                    let start = self.i - 1;
                    let extra = match c {
                        0x00..=0x7F => 0,
                        0xC0..=0xDF => 1,
                        0xE0..=0xEF => 2,
                        0xF0..=0xF7 => 3,
                        _ => 0,
                    };
                    if extra > 0 {
                        self.i += extra;
                    }
                    let end = self.i.min(self.b.len());
                    if let Ok(part) = std::str::from_utf8(&self.b[start..end]) {
                        s.push_str(part);
                    }
                }
            }
        }
        Ok(s)
    }

    fn hex4(&mut self) -> Result<u16> {
        let mut v: u16 = 0;
        for _ in 0..4 {
            let c = self
                .bump()
                .ok_or_else(|| Error::BadMedia("bad \\u escape".into()))?;
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(Error::BadMedia("bad \\u escape".into())),
            };
            v = (v << 4) | d as u16;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        if self.peek() == Some(b'-') || self.peek() == Some(b'+') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => self.i += 1,
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| Error::BadMedia("bad JSON number".into()))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| Error::BadMedia(format!("bad JSON number '{text}'")))
    }
}

/// Parse a rational like `"30000/1001"` or `"25/1"` into an f64.
pub fn parse_rational(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "0/0" {
        return None;
    }
    match s.split_once('/') {
        Some((n, d)) => {
            let n: f64 = n.trim().parse().ok()?;
            let d: f64 = d.trim().parse().ok()?;
            if d == 0.0 {
                None
            } else {
                let v = n / d;
                if v.is_finite() && v > 0.0 {
                    Some(v)
                } else {
                    None
                }
            }
        }
        None => {
            let v: f64 = s.parse().ok()?;
            if v.is_finite() && v > 0.0 {
                Some(v)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_and_escapes() {
        let j = parse(r#"{"a":[1,2.5,true,null],"b":{"c":"x\ny"}}"#).unwrap();
        assert_eq!(j.get("a").unwrap().as_arr().unwrap().len(), 4);
        assert_eq!(j.get("b").unwrap().get("c").unwrap().as_str(), Some("x\ny"));
    }

    #[test]
    fn rationals() {
        assert!((parse_rational("30000/1001").unwrap() - 29.97002997).abs() < 1e-6);
        assert_eq!(parse_rational("25/1"), Some(25.0));
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("N/A"), None);
    }
}
