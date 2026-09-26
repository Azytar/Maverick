//! Minimal JSON string helpers and a flat-object codec shared by the whole
//! project (no `serde` dependency).
//!
//! Maverick's payloads (ficha, session spec, state snapshot, event lines) are
//! small flat objects written by Maverick itself, so a full JSON stack would be
//! dead weight in a WM binary. [`crate::identity`], [`crate::session`] and
//! [`crate::control`] all go through this single escaper to stay
//! wire-compatible.
//!
//! # Ownership
//!
//! Pure `&str` → `String` helpers; no global state, no dependencies. The codec
//! ([`scan_object`]) is likewise pure and total: it never panics, never slices
//! a multi-byte character, and is not a general-purpose JSON parser — it is the
//! reader for the exact shapes this project writes.

/// Escape `s` for use inside a JSON string literal, WITHOUT adding the
/// surrounding quotes. Quotes, backslashes and C0 control characters are
/// escaped (`\uXXXX` for the rest of the control range).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

/// Return `s` as a complete JSON string token (escaped, with quotes).
pub fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&json_escape(s));
    out.push('"');
    out
}

/// Inverse of `json_escape`/`json_quote`: decode a JSON string (with or without
/// its surrounding quotes) back to the original text. Lenient: an unknown
/// escape is copied verbatim, and a malformed `\uXXXX` is left as-is.
pub fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000c}'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                            continue;
                        }
                    }
                    out.push_str(&format!("\\u{hex}"));
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One value read out of a flat JSON object.
///
/// A `Str` keeps its escapes: decoding is the caller's decision, because a
/// caller that wants the *text* has to go through [`json_unescape`] while one
/// that wants the raw token does not. This mirrors the writer, which only ever
/// escapes inside strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<'a> {
    /// A quoted string, holding its body *without* the surrounding quotes and
    /// *with* its escapes intact.
    Str(&'a str),
    /// A bare token: number, `true`, `false`, `null`, or anything unrecognised.
    Raw(&'a str),
    /// An array, holding its elements' texts in order. A quoted element has
    /// already had its delimiters removed by the scan, so an element is only
    /// ever unescaped once, by the caller — the same rule a `Str` follows.
    Array(Vec<&'a str>),
}

/// A `key: value` pair from a flat JSON object, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field<'a> {
    /// The key, unquoted and still escaped.
    pub key: &'a str,
    /// The value.
    pub value: Value<'a>,
}

impl Field<'_> {
    /// The value as text, decoding a string's escapes. `None` for a non-string,
    /// and for `null` — which is how a document says "no value", and reading it
    /// as the four-character string `"null"` turns an absent optional field
    /// into a present one holding nonsense.
    pub fn as_str(&self) -> Option<String> {
        match &self.value {
            Value::Str(body) => Some(json_unescape(body)),
            _ => None,
        }
    }
    /// The value as text, whether or not it was quoted. Useful for a field the
    /// writer always emits as a string but a lenient reader must survive.
    pub fn text(&self) -> String {
        match &self.value {
            Value::Str(body) => json_unescape(body),
            // A writer that emits `"field":null` for an absent optional has
            // said exactly that; the literal text `null` is the one value that
            // must never be mistaken for content.
            Value::Raw("null") => String::new(),
            Value::Raw(raw) => (*raw).to_string(),
            Value::Array(_) => String::new(),
        }
    }
    /// The value parsed as an unsigned integer, `None` for anything else.
    pub fn as_u64(&self) -> Option<u64> {
        match &self.value {
            Value::Raw(raw) => raw.parse().ok(),
            _ => None,
        }
    }
    /// The value as a boolean. Only the exact tokens `true`/`false` count, so a
    /// number or a string never reads as `true` by accident.
    pub fn as_bool(&self) -> Option<bool> {
        match &self.value {
            Value::Raw("true") => Some(true),
            Value::Raw("false") => Some(false),
            _ => None,
        }
    }
    /// The array elements as text, decoding each element's escapes. `None` when
    /// the value is not an array, so a caller can tell "absent" from "empty".
    ///
    /// No quote stripping happens here: the scan already consumed a quoted
    /// element's delimiters, and peeling a second pair would eat the closing
    /// quote of an element that *ends* in an escaped one.
    pub fn as_str_array(&self) -> Option<Vec<String>> {
        match &self.value {
            Value::Array(items) => Some(items.iter().map(|raw| json_unescape(raw)).collect()),
            _ => None,
        }
    }
}

/// Serialize a list of strings as a JSON array.
pub fn quote_array<I: IntoIterator<Item = S>, S: AsRef<str>>(items: I) -> String {
    let mut out = String::from("[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_quote(item.as_ref()));
    }
    out.push(']');
    out
}

/// Scan a flat JSON object body into its fields, in document order.
///
/// The surrounding braces are optional, so a pretty-printed document (which
/// puts newlines between fields) and a compact one read the same. This is the
/// single reader behind [`crate::identity`]'s ficha and [`crate::session`]'s
/// session spec, so neither has to grow its own scanner.
///
/// # Invariants
///
/// Total and panic-free on hostile input: every slice goes through `str::get`
/// (which yields `None` on a non-char-boundary) and the scan is a monotonic
/// cursor, so a truncated or malformed document yields fewer fields rather than
/// a panic or an infinite loop. A quoted value is consumed up to its closing
/// quote with escape awareness, so an escaped `\"` is payload and cannot end
/// the value early. Unknown keys are returned like any other: only the caller
/// knows which of them it wants.
pub fn scan_object(doc: &str) -> Vec<Field<'_>> {
    let body = doc.trim();
    let body = body.strip_prefix('{').unwrap_or(body);
    let body = body.strip_suffix('}').unwrap_or(body);
    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut fields = Vec::new();
    let mut i = 0;
    while i < len {
        // Skip whitespace and the field separators.
        while i < len
            && (bytes[i] == b' '
                || bytes[i] == b'\t'
                || bytes[i] == b','
                || bytes[i] == b'\n'
                || bytes[i] == b'\r')
        {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Key: quoted, or a bare token for a hand-written document.
        let key_start = i;
        let key = if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let k = body.get(start..i.min(len)).unwrap_or("");
            i += 1;
            k
        } else {
            let start = i;
            while i < len && bytes[i] != b':' && bytes[i] != b' ' {
                i += 1;
            }
            body.get(start..i).unwrap_or("")
        };
        // Strip exactly one pair of surrounding quotes, not all of them:
        // `trim_matches` would peel `"a"` -> `a` but also `""a""` -> `a`.
        let key = key
            .strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .unwrap_or(key);
        if key.is_empty() {
            // No progress is possible on this byte; step over it so a
            // malformed document still terminates.
            i = key_start + 1;
            continue;
        }
        // Skip ':' and whitespace.
        while i < len && (bytes[i] == b':' || bytes[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Value: a quoted string, an array, or a bare token.
        let value = if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len {
                if bytes[i] == b'\\' {
                    // Consume the whole escape: an escaped quote is payload,
                    // not the end of the value, so a `"\""` must not be split.
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    break;
                }
                i += 1;
            }
            let v = Value::Str(body.get(start..i.min(len)).unwrap_or(""));
            if i < len {
                i += 1;
            }
            v
        } else if bytes[i] == b'[' {
            let mut items = Vec::new();
            i += 1;
            loop {
                while i < len && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i >= len {
                    break;
                }
                if bytes[i] == b']' {
                    i += 1;
                    break;
                }
                if bytes[i] == b',' {
                    i += 1;
                    continue;
                }
                let start = i;
                if bytes[i] == b'"' {
                    i += 1;
                    let inner = i;
                    while i < len && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    let elem = body.get(inner..i.min(len)).unwrap_or("");
                    if i < len {
                        i += 1;
                    }
                    items.push(elem);
                } else {
                    while i < len && bytes[i] != b',' && bytes[i] != b']' {
                        i += 1;
                    }
                    items.push(body.get(start..i).unwrap_or(""));
                }
            }
            Value::Array(items)
        } else {
            let start = i;
            while i < len && bytes[i] != b',' && bytes[i] != b'}' {
                i += 1;
            }
            Value::Raw(body.get(start..i).unwrap_or("").trim())
        };
        fields.push(Field { key, value });
    }
    fields
}

/// A parsed JSON value.
///
/// Enough of JSON to read what Maverick *writes* — window ids, pids, geometry,
/// the nesting of the tree query — and nothing more. The alternative would be
/// `serde` in a window manager's control plane, which is a dependency the whole
/// project has so far declined; this is the same trade `scan_object` makes, one
/// level deeper.
///
/// Numbers are kept as `f64`. Every number Maverick emits is an integer that
/// fits exactly (a window id is at most 32 bits, a geometry at most 16), so
/// [`Json::as_u64`] is exact; a fractional value only comes from the camera
/// position and the column weights, which are read as floats anyway.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// Any JSON number.
    Num(f64),
    /// A string, decoded.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, in document order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The value at `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at `key` as a string, `""` when absent or not a string.
    pub fn str_field(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Json::Str(s)) => s,
            _ => "",
        }
    }

    /// The value at `key` as a `u64`, `0` when absent or not a number.
    pub fn num_field(&self, key: &str) -> u64 {
        self.get(key).and_then(Json::as_u64).unwrap_or(0)
    }

    /// The value at `key` as a `bool`, `false` when absent or not a boolean.
    pub fn bool_field(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Json::Bool(true)))
    }

    /// The elements, if this is an array; an empty slice otherwise.
    pub fn as_array(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }

    /// The number, if this is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The number as a `u64`, when it is a non-negative whole number.
    ///
    /// A fractional or negative value is `None` rather than a truncated one: a
    /// window id is never `-1.5`, and reading a garbage id as a large unsigned
    /// number would address a window that does not exist.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 && *n <= u64::MAX as f64 => {
                Some(*n as u64)
            }
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Render the value back as compact JSON.
    ///
    /// Used where a tool has parsed a document only to reshape it, so the
    /// escaping is the writer's own (see [`json_quote`]) rather than anything
    /// recovered from the input.
    pub fn to_json(&self) -> String {
        match self {
            Json::Null => "null".to_string(),
            Json::Bool(b) => b.to_string(),
            Json::Num(n) => format_number(*n),
            Json::Str(s) => json_quote(s),
            Json::Arr(items) => {
                let parts: Vec<String> = items.iter().map(Json::to_json).collect();
                format!("[{}]", parts.join(","))
            }
            Json::Obj(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}:{}", json_quote(k), v.to_json()))
                    .collect();
                format!("{{{}}}", parts.join(","))
            }
        }
    }
}

/// Render a number the way the project's own writers do: a whole number as an
/// integer, anything else as the shortest form that round-trips.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Parse a complete JSON document. Returns `None` for anything that is not one.
///
/// Total and panic-free: the parser is a recursive-descent over a byte cursor
/// with an explicit depth bound, so a document of nothing but `[[[[…` cannot
/// exhaust the stack, and every slice goes through `str::get`, so a truncated
/// multi-byte character cannot panic.
///
/// A *trailing* fragment after the value is a failure, not something ignored: a
/// tool that reported success for half a document would silently act on
/// truncated state.
pub fn parse(doc: &str) -> Option<Json> {
    let mut p = Parser {
        bytes: doc.as_bytes(),
        pos: 0,
        depth: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos == p.bytes.len() {
        Some(value)
    } else {
        None
    }
}

/// Nesting bound. Maverick's deepest document is the tree query (monitor →
/// workspace → column → window), four levels; the bound is generous enough for
/// anything a future query nests and small enough that a hostile document
/// cannot turn into unbounded recursion.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn text(&self) -> &'a str {
        // The document is a `&str`, so every offset the cursor reaches is a
        // char boundary; the fallback keeps that guarantee local rather than
        // assumed at each of the slices below.
        std::str::from_utf8(self.bytes).unwrap_or("")
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, b: u8) -> Option<()> {
        self.eat(b).then_some(())
    }

    fn literal(&mut self, word: &str) -> bool {
        if self.text()[self.pos..].starts_with(word) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.skip_ws();
        if self.depth >= MAX_DEPTH {
            return None;
        }
        match *self.bytes.get(self.pos)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' => self.literal("true").then_some(Json::Bool(true)),
            b'f' => self.literal("false").then_some(Json::Bool(false)),
            b'n' => self.literal("null").then_some(Json::Null),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.expect(b'{')?;
        self.depth += 1;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.eat(b'}') {
            self.depth -= 1;
            return Some(Json::Obj(fields));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.value()?;
            fields.push((key, value));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            self.expect(b'}')?;
            self.depth -= 1;
            return Some(Json::Obj(fields));
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.expect(b'[')?;
        self.depth += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            self.depth -= 1;
            return Some(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            self.expect(b']')?;
            self.depth -= 1;
            return Some(Json::Arr(items));
        }
    }

    /// A quoted string, decoded, consuming both quotes.
    fn string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let start = self.pos;
        loop {
            let b = *self.bytes.get(self.pos)?;
            match b {
                b'\\' => {
                    // An escape is at most two bytes here, because `\uXXXX` is
                    // decoded as a unit by `json_unescape`; stepping two always
                    // makes progress and never steps over the closing quote.
                    self.pos += 2;
                }
                b'"' => {
                    let body = self.text().get(start..self.pos)?;
                    self.pos += 1;
                    return Some(json_unescape(body));
                }
                _ => self.pos += 1,
            }
        }
    }

    /// A number, in the JSON grammar: an optional sign, an integer part with no
    /// leading zeros, an optional fraction and an optional exponent.
    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        let negative = self.eat(b'-');
        let digits_start = self.pos;
        if !self.eat(b'0') {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos == digits_start {
            return None;
        }
        if self.eat(b'.') {
            let frac_start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return None;
            }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return None;
            }
        }
        let text = self.text().get(start..self.pos)?;
        // `-0` and a leading `+` are the only spellings a caller could not also
        // write; both still have to reach the same number.
        let _ = negative;
        text.parse().ok().map(Json::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_the_usual_specials() {
        assert_eq!(json_escape("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(json_quote("x"), "\"x\"");
    }

    #[test]
    fn control_characters_use_unicode_escapes() {
        assert_eq!(json_escape("\u{07}"), "\\u0007");
    }

    #[test]
    fn unescape_handles_unicode_escapes() {
        assert_eq!(json_unescape("\\u00e9"), "é");
        assert_eq!(json_unescape("\\u0041\\u0042"), "AB");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        for s in [
            "plain",
            "with \"quotes\" and \\slashes\\",
            "tab\there\n",
            "é👍",
        ] {
            assert_eq!(json_unescape(&json_escape(s)), s);
            let quoted = json_quote(s);
            // `json_quote` adds the surrounding quotes; `unescape` expects the
            // body without them (matching `identity::unquote`).
            assert_eq!(json_unescape(&quoted[1..quoted.len() - 1]), s);
        }
    }

    /// Convenience: the `text` of the single field named `key`.
    fn field<'a>(doc: &'a str, key: &str) -> Field<'a> {
        scan_object(doc)
            .into_iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("no field {key:?} in {doc:?}"))
    }

    #[test]
    fn scans_scalars_of_every_type() {
        let doc = r#"{"name":"dev","pid":1234,"alive":true,"ratio":-0.5,"none":null}"#;
        assert_eq!(field(doc, "name").as_str().as_deref(), Some("dev"));
        assert_eq!(field(doc, "pid").as_u64(), Some(1234));
        assert_eq!(field(doc, "alive").as_bool(), Some(true));
        assert_eq!(field(doc, "ratio").as_str(), None);
        assert_eq!(field(doc, "none").as_bool(), None);
    }

    /// A comma inside a quoted value is payload, not a field separator: it is
    /// exactly the shape a name or an argument list takes.
    #[test]
    fn a_comma_inside_a_string_does_not_split_the_object() {
        let doc = r#"{"name":"a,b","pid":7}"#;
        assert_eq!(field(doc, "name").as_str().as_deref(), Some("a,b"));
        assert_eq!(field(doc, "pid").as_u64(), Some(7));
    }

    /// An escaped quote is payload, so it must not end the value early and
    /// swallow the rest of the object.
    #[test]
    fn an_escaped_quote_stays_inside_the_value() {
        let doc = r#"{"name":"say \"hi\"","pid":7}"#;
        assert_eq!(field(doc, "name").as_str().as_deref(), Some(r#"say "hi""#));
        assert_eq!(field(doc, "pid").as_u64(), Some(7));
    }

    /// Backslashes are what make the previous case work: the last one of a
    /// value sits right before the closing quote.
    #[test]
    fn a_trailing_backslash_does_not_eat_the_delimiter() {
        let doc = r#"{"display":":0\\","pid":7}"#;
        assert_eq!(field(doc, "display").as_str().as_deref(), Some(r":0\"));
        assert_eq!(field(doc, "pid").as_u64(), Some(7));
    }

    #[test]
    fn arrays_round_trip_through_quote_array() {
        let doc = format!("{{\"args\":{}}}", quote_array(["a b", "c,d", r#"q"q"#]));
        assert_eq!(
            field(&doc, "args").as_str_array(),
            Some(vec![
                "a b".to_string(),
                "c,d".to_string(),
                r#"q"q"#.to_string()
            ])
        );
    }

    /// An element that *ends* in an escaped quote is the shape that breaks a
    /// reader which strips a second pair of delimiters: the scan already
    /// consumed the real closing quote, so peeling one more would eat the
    /// escaped one. This is a real shape — a session record stores the
    /// arguments a user typed after `--`.
    #[test]
    fn an_array_element_ending_in_an_escaped_quote_survives() {
        let items = [r#"--log "x,y""#.to_string(), r#"tail\"#.to_string()];
        let doc = format!("{{\"args\":{}}}", quote_array(&items));
        assert_eq!(field(&doc, "args").as_str_array(), Some(items.to_vec()));
    }

    /// `"field": null` is how a writer says "no value", and it must read as
    /// absent. Reading it as the four characters `null` is how an optional
    /// string field turns into a present one holding nonsense — a working
    /// directory called "null", a resolution that fails to parse for the wrong
    /// reason.
    #[test]
    fn a_null_field_reads_as_absent_not_as_the_text_null() {
        let doc = r#"{"a":null,"b":"","c":0}"#;
        assert_eq!(field(doc, "a").text(), "");
        assert_eq!(field(doc, "a").as_str(), None);
        assert_eq!(field(doc, "a").as_u64(), None);
        assert_eq!(field(doc, "b").text(), "");
        assert_eq!(field(doc, "c").text(), "0", "zero is a value, null is not");
    }

    /// An empty array and an absent field must stay distinguishable: an empty
    /// argument list is a real answer, a missing field is "unknown".
    #[test]
    fn an_empty_array_is_not_an_absent_field() {
        let doc = r#"{"args":[],"other":1}"#;
        assert_eq!(field(doc, "args").as_str_array(), Some(Vec::new()));
        assert_eq!(
            scan_object(r#"{"other":1}"#)
                .iter()
                .find(|f| f.key == "args")
                .and_then(Field::as_str_array),
            None
        );
    }

    /// Pretty-printed input carries newlines between fields, which the
    /// compact-only separator skip would leave in front of the next key.
    #[test]
    fn a_pretty_printed_object_reads_like_a_compact_one() {
        let compact = r#"{"a":"1","b":2}"#;
        let pretty = "{\n  \"a\": \"1\",\n  \"b\": 2\n}";
        let of = |doc: &str| -> Vec<(String, String)> {
            scan_object(doc)
                .into_iter()
                .map(|f| (f.key.to_string(), f.text()))
                .collect()
        };
        assert_eq!(of(compact), of(pretty));
    }

    /// The codec reads whatever is on disk, which is not always a document
    /// Maverick wrote. It has to terminate and answer with what it found, not
    /// panic and not invent fields.
    #[test]
    fn hostile_input_terminates_without_inventing_fields() {
        for doc in [
            "",
            "{",
            "}",
            "{{{",
            r#"{"a""#,
            r#"{"a":"unterminated"#,
            r#"{"a":"esc"#,
            r#"{"a":[1,2"#,
            r#"{"a":"é""#,
            "\u{0}{\"a\":1}",
        ] {
            let fields = scan_object(doc);
            // Document order is all we promise; every returned key is a real
            // token of the input, never a synthesised one.
            for f in &fields {
                assert!(
                    !f.key.is_empty() && doc.contains(f.key),
                    "invented key {:?} from {doc:?}",
                    f.key
                );
            }
        }
    }

    // ── value parser ────────────────────────────────────────────────────────

    #[test]
    fn parses_every_value_kind() {
        let v = parse(r#"{"s":"x","n":42,"f":-1.5e2,"t":true,"z":null,"a":[1,2],"o":{"k":"v"}}"#)
            .expect("valid document");
        assert_eq!(v.str_field("s"), "x");
        assert_eq!(v.num_field("n"), 42);
        assert_eq!(v.num_field("f"), 0, "-1.5e2 is negative, not a window id");
        assert_eq!(v.get("f").and_then(Json::as_f64), Some(-150.0));
        assert!(v.bool_field("t"));
        assert_eq!(v.get("z"), Some(&Json::Null));
        assert_eq!(v.get("a").map(Json::as_array).map(<[Json]>::len), Some(2));
        assert_eq!(v.get("o").unwrap().str_field("k"), "v");
    }

    /// The shapes a missing field must produce are what every caller relies on
    /// to stay total: `""`, `0`, `false`, an empty array. A tool that printed
    /// "None" or panicked on a window without a pid would be useless exactly
    /// when something is already wrong.
    #[test]
    fn absent_and_wrongly_typed_fields_degrade_to_neutral_values() {
        let v = parse(r#"{"a":"text","b":[1],"c":true}"#).expect("valid");
        assert_eq!(v.str_field("missing"), "");
        assert_eq!(v.num_field("missing"), 0);
        assert!(!v.bool_field("missing"));
        assert!(v.get("a").and_then(Json::as_u64).is_none());
        assert_eq!(v.str_field("b"), "", "an array is not a string");
        assert!(!v.bool_field("a"), "a non-empty string is not true");
        assert!(v.get("missing").is_none());
        // And on a non-object, every accessor is still total.
        let arr = parse("[1,2]").expect("valid");
        assert_eq!(arr.str_field("a"), "");
        assert_eq!(arr.num_field("a"), 0);
    }

    /// Every integer a window id, a pid or a geometry can be is exact through
    /// `as_u64`. This is the property that lets a window id survive a parse.
    #[test]
    fn integers_survive_exactly() {
        for n in [0u64, 1, 42, 0x42003, u32::MAX as u64, 65535] {
            let doc = format!("{{\"v\":{n}}}");
            let v = parse(&doc).expect("valid");
            assert_eq!(v.num_field("v"), n);
        }
        // And a fractional or negative value is refused rather than truncated.
        for bad in ["-1", "1.5"] {
            let doc = format!("{{\"v\":{bad}}}");
            let v = parse(&doc).expect("valid");
            assert_eq!(v.get("v").and_then(Json::as_u64), None, "{bad}");
        }
        // `-0` is zero: it is the one negative spelling that names a real
        // value, and truncating it to 0 is the correct answer, not a
        // fabrication.
        assert_eq!(parse("-0").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn strings_decode_and_escapes_do_not_end_them_early() {
        let v = parse(r#"{"a":"say \"hi\"","b":"tab\there","c":"é","d":""}"#).expect("valid");
        assert_eq!(v.str_field("a"), r#"say "hi""#);
        assert_eq!(v.str_field("b"), "tab\there");
        assert_eq!(v.str_field("c"), "é");
        assert_eq!(v.str_field("d"), "");
    }

    /// Pretty-printed input is what a human sees in a log, and a tool must
    /// read it the same as the compact form.
    #[test]
    fn whitespace_between_tokens_is_insignificant() {
        let compact = r#"{"a":[1,{"b":2}],"c":"d"}"#;
        let pretty = "{\n  \"a\": [1, {\n    \"b\": 2\n  }],\n  \"c\": \"d\"\n}";
        assert_eq!(parse(compact), parse(pretty));
    }

    /// A document of nothing but open brackets must not exhaust the stack: the
    /// parser is recursive, and the input comes off a socket.
    #[test]
    fn deep_nesting_is_refused_rather_than_overflowing() {
        let deep = "[".repeat(10_000);
        assert!(parse(&deep).is_none());
        let deep_objs = "{\"a\":".repeat(10_000);
        assert!(parse(&deep_objs).is_none());
        // Just inside the bound still parses.
        let ok = "[".repeat(MAX_DEPTH - 1) + &"]".repeat(MAX_DEPTH - 1);
        assert!(parse(&ok).is_some());
    }

    /// Truncated output is the realistic bad input — a control socket that
    /// closed mid-write — and it must never be reported as a document.
    #[test]
    fn a_truncated_document_is_not_a_document() {
        let full = r#"{"monitors":[{"index":0,"workspaces":[{"name":"a"}]}]}"#;
        for cut in 1..full.len() {
            let partial = &full[..cut];
            if !full.is_char_boundary(cut) {
                continue;
            }
            if parse(partial).is_some() {
                // A prefix can be a complete value only if it ends the document
                // — which a truncated one never does.
                panic!("{partial:?} must not parse");
            }
        }
    }

    /// A trailing fragment after the value is a failure, not something ignored:
    /// a tool that accepted half a document would act on truncated state.
    #[test]
    fn a_trailing_fragment_is_refused() {
        assert!(parse(r#"{"a":1} {"b":2}"#).is_none());
        assert!(parse("1 2").is_none());
        assert!(parse(r#"{"a":1}x"#).is_none());
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
    }

    /// The grammar, not just the happy path: a leading zero, a bare `.` or a
    /// missing digit are all malformed, and a reader that accepted them would
    /// invent values.
    #[test]
    fn the_number_grammar_is_enforced() {
        for bad in ["01", "+1", ".5", "1.", "1e", "1e+", "--1", "1..2"] {
            assert!(parse(bad).is_none(), "{bad:?} is not a JSON number");
        }
        for good in ["0", "-0", "1", "-1", "1.5", "1e3", "1E-3", "1.5e+10"] {
            assert!(parse(good).is_some(), "{good:?} is a JSON number");
        }
    }

    #[test]
    fn the_literal_keywords_are_not_prefix_matched() {
        for bad in ["tru", "truex", "nul", "falsey"] {
            assert!(parse(bad).is_none(), "{bad:?} is not a literal");
        }
        assert!(parse("true").is_some());
        assert!(parse("null").is_some());
    }

    /// Round-tripping is what makes the parser usable where a document is
    /// reshaped for output.
    #[test]
    fn values_round_trip_through_to_json() {
        for doc in [
            r#"{"a":1,"b":"x"}"#,
            r#"{"a":[1,2,3],"b":{"c":false,"d":null}}"#,
            r#"{"a":1.5,"b":"with \"quotes\" and \\ backslash"}"#,
            r#"{}"#,
            r#"[]"#,
        ] {
            let v = parse(doc).expect("valid");
            let again = parse(&v.to_json()).expect("re-parses");
            assert_eq!(v, again, "{doc}");
        }
    }

    /// The scanner and the parser are two readers of the same writer's output;
    /// if they disagreed about where a value ended, a document written by one
    /// and read by the other would be misread.
    #[test]
    fn the_scanner_and_the_parser_agree_on_field_values() {
        let doc = r#"{"name":"a,b","n":7,"f":true,"args":["x","y"]}"#;
        let flat = scan_object(doc);
        let tree = parse(doc).expect("valid");
        for field in &flat {
            let other = tree.get(field.key).expect("present in both readings");
            match &field.value {
                Value::Str(body) => {
                    assert_eq!(other.as_str(), Some(json_unescape(body).as_str()))
                }
                Value::Raw(raw) => match *raw {
                    "true" => assert!(matches!(other, Json::Bool(true))),
                    "false" => assert!(matches!(other, Json::Bool(false))),
                    number => {
                        assert_eq!(
                            tree.num_field(field.key),
                            number.parse().expect("a number Maverick writes")
                        )
                    }
                },
                Value::Array(items) => {
                    let read = items
                        .iter()
                        .map(|raw| json_unescape(raw))
                        .collect::<Vec<String>>();
                    let parsed: Vec<String> = other
                        .as_array()
                        .iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect();
                    assert_eq!(parsed, read);
                }
            }
        }
    }
}
