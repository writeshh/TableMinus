//! Values typed in `PartiQL`'s literal syntax, as `DynamoDB`'s typed JSON (`AttributeValue`): `'text'` is `{"S"}`,
//! `42` `{"N"}`, `true` `{"BOOL"}`, `null` `{"NULL"}`, `{'a': 1}` `{"M"}`, `[1, 'x']` `{"L"}`, and `<<'a', 'b'>>` a
//! string or number set.

use crate::error::{Error, Result};
use serde_json::{Map, Value, json};

/// The most nesting a typed value may have.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

fn fail(what: &str) -> Error {
    Error::Dynamo(format!("can't read the value: {what}"))
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.space();
        if self.s[self.at..].starts_with(token.as_bytes()) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &str) -> bool {
        self.space();
        let end = self.at + w.len();
        let next_is_word = self.s.get(end).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if self.s.len() >= end && self.s[self.at..end].eq_ignore_ascii_case(w.as_bytes()) && !next_is_word {
            self.at = end;
            true
        } else {
            false
        }
    }

    /// A quoted string: `'…'` with `''` for a quote (or `"…"`, the way names are written, for a map's keys).
    fn string(&mut self) -> Result<Option<String>> {
        self.space();
        let Some(&q) = self.s.get(self.at).filter(|b| **b == b'\'' || **b == b'"') else { return Ok(None) };
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match self.s.get(self.at) {
                None => return Err(fail("a quote isn't closed")),
                Some(&b) if b == q && self.s.get(self.at + 1) == Some(&q) => {
                    out.push(q);
                    self.at += 2;
                }
                Some(&b) if b == q => {
                    self.at += 1;
                    return String::from_utf8(out).map(Some).map_err(|_| fail("not UTF-8"));
                }
                Some(&b) => {
                    out.push(b);
                    self.at += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Option<String> {
        self.space();
        let start = self.at;
        let digits = |p: &mut Self| {
            let from = p.at;
            while p.s.get(p.at).is_some_and(u8::is_ascii_digit) {
                p.at += 1;
            }
            p.at > from
        };
        if matches!(self.s.get(self.at), Some(b'-' | b'+')) {
            self.at += 1;
        }
        let whole = digits(self);
        let fraction = self.s.get(self.at) == Some(&b'.') && {
            self.at += 1;
            digits(self)
        };
        if !whole && !fraction {
            self.at = start;
            return None;
        }
        if matches!(self.s.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.s.get(self.at), Some(b'-' | b'+')) {
                self.at += 1;
            }
            if !digits(self) {
                self.at = start;
                return None;
            }
        }
        Some(String::from_utf8_lossy(&self.s[start..self.at]).into_owned())
    }

    fn list(&mut self, close: &str, depth: usize) -> Result<Vec<Value>> {
        let mut items = vec![];
        if self.eat(close) {
            return Ok(items);
        }
        loop {
            items.push(self.value(depth + 1)?);
            if self.eat(close) {
                return Ok(items);
            }
            if !self.eat(",") {
                return Err(fail(&format!("expected , or {close}")));
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err(fail("it nests too deep"));
        }
        if let Some(s) = self.string()? {
            return Ok(json!({ "S": s }));
        }
        if let Some(n) = self.number() {
            return Ok(json!({ "N": n }));
        }
        if self.word("true") {
            return Ok(json!({ "BOOL": true }));
        }
        if self.word("false") {
            return Ok(json!({ "BOOL": false }));
        }
        if self.word("null") || self.word("missing") {
            return Ok(json!({ "NULL": true }));
        }
        if self.eat("<<") {
            let items = self.list(">>", depth)?;
            let all = |t: &str| !items.is_empty() && items.iter().all(|i| i.get(t).is_some());
            let set = if all("S") {
                "SS"
            } else if all("N") {
                "NS"
            } else {
                return Err(fail("a set holds strings or numbers, at least one"));
            };
            let tag = &set[..1];
            return Ok(json!({ set: items.iter().map(|i| i[tag].clone()).collect::<Vec<_>>() }));
        }
        if self.eat("[") {
            return Ok(json!({ "L": self.list("]", depth)? }));
        }
        if self.eat("{") {
            let mut map = Map::new();
            if self.eat("}") {
                return Ok(json!({ "M": map }));
            }
            loop {
                let key = self.string()?.ok_or_else(|| fail("a map's keys are quoted, like {'name': 'Ada'}"))?;
                if !self.eat(":") {
                    return Err(fail("expected : after a key"));
                }
                map.insert(key, self.value(depth + 1)?);
                if self.eat("}") {
                    return Ok(json!({ "M": map }));
                }
                if !self.eat(",") {
                    return Err(fail("expected , or }"));
                }
            }
        }
        Err(fail("expected 'text', a number, true, false, null, {…}, […] or <<…>>"))
    }
}

/// `text` as one typed value; anything after it is an error.
pub fn parse(text: &str) -> Result<Value> {
    let mut p = Parser { s: text.as_bytes(), at: 0 };
    let v = p.value(0)?;
    p.space();
    if p.at < p.s.len() {
        return Err(fail("there's more after the value"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::json;

    #[test]
    fn parse_should_read_each_kind_of_value() {
        assert_eq!(parse("'it''s'").unwrap(), json!({ "S": "it's" }));
        assert_eq!(parse(" -1.5e3 ").unwrap(), json!({ "N": "-1.5e3" }));
        assert_eq!(parse("TRUE").unwrap(), json!({ "BOOL": true }));
        assert_eq!(parse("false").unwrap(), json!({ "BOOL": false }));
        assert_eq!(parse("null").unwrap(), json!({ "NULL": true }));
        assert_eq!(parse("[1, 'a', []]").unwrap(), json!({ "L": [{ "N": "1" }, { "S": "a" }, { "L": [] }] }));
        assert_eq!(
            parse("{'name': 'Ada', \"tags\": <<'x', 'y'>>}").unwrap(),
            json!({ "M": { "name": { "S": "Ada" }, "tags": { "SS": ["x", "y"] } } })
        );
        assert_eq!(parse("<<1, 2>>").unwrap(), json!({ "NS": ["1", "2"] }));
    }

    #[test]
    fn parse_should_refuse_what_isnt_one_value() {
        for bad in ["", "Ada", "'open", "1 2", "{name: 1}", "<<1, 'a'>>", "<<>>", "truex", &"[".repeat(40)] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
