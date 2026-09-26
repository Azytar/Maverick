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
    /// The value as text, decoding a string's escapes. `None` for a non-string.
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
}
