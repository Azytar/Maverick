//! Property-based coverage of the parser's documented contract.
//!
//! The contract under test, from the crate docs:
//!
//! * **Totality.** "This parser never panics" on any input, accepted or not.
//!   A rejected file is a whole-file `Err`, never a panic and never a silently
//!   truncated stream.
//! * **Fusion.** The stream yields strictly in file order and is fused: after
//!   the first `Err` — or end of input — it yields `None` forever.
//! * **Zero-copy.** Escape-free strings borrow from the caller's buffer; a
//!   string containing an escape is decoded into an owned `Cow`.
//! * **Bounds.** A literal past `MAX_NUMBER_LEN`, `MAX_HEX_DIGITS`,
//!   `MAX_STRING_LEN` or `MAX_ARRAY_ELEMS` is rejected instead of burning CPU
//!   or memory on a hostile file.
//!
//! Everything here is deterministic: no clock, no filesystem, no environment.

use std::borrow::Cow;

use maverick_toml::{
    parse, Event, Value, MAX_ARRAY_ELEMS, MAX_HEX_DIGITS, MAX_NUMBER_LEN, MAX_STRING_LEN,
};
use proptest::prelude::*;

/// The diagnostic vocabulary the loader prints verbatim. A parse fault that
/// reported anything else would reach the user as an unrecognised tag.
const KINDS: &[&str] = &[
    "header",
    "key",
    "pair",
    "value",
    "string",
    "array",
    "duplicate-key",
];

/// Collect the whole event stream, faults included.
fn stream(src: &str) -> Vec<Result<Event<'_>, maverick_toml::ParseError>> {
    parse(src).collect()
}

/// The tags of every fault in `src`, in order.
fn faults(src: &str) -> Vec<&'static str> {
    stream(src)
        .iter()
        .filter_map(|item| item.as_ref().err())
        .map(|e| e.kind)
        .collect()
}

/// The value of the only `key = value` pair in `src`, headers ignored.
fn only_pair(src: &str) -> Option<Value<'_>> {
    let mut values = stream(src).into_iter().filter_map(|item| match item {
        Ok(Event::KeyValue(_, value)) => Some(value),
        _ => None,
    });
    match (values.next(), values.next()) {
        (Some(value), None) => Some(value),
        _ => None,
    }
}

/// Text that is a plain bare key (`[A-Za-z0-9_-]`), so a generated pair is
/// always inside the accepted grammar.
fn key() -> BoxedStrategy<&'static str> {
    prop::sample::select(vec!["k", "key", "a_b-c", "normal", "commands", "stiffness"]).boxed()
}

/// Free-form string body: no quote, no backslash, no line break, so it can be
/// embedded in `"…"` and is expected back verbatim.
fn plain_text() -> BoxedStrategy<String> {
    prop_oneof!["[a-z _-]{0,8}", "[\\p{L}\\p{N} _-]{0,6}"].boxed()
}

/// One piece of TOML-ish text: fragments the strict subset accepts next to
/// fragments it must reject. Concatenated at random they reach unterminated
/// strings, arrays and headers, duplicate keys, dotted keys, single-quoted
/// strings, exponent notation, mixed-type arrays and unknown escapes.
fn piece() -> BoxedStrategy<String> {
    let k = key();
    let text = plain_text();
    prop_oneof![
        k.clone().prop_map(|k| format!("{k} = 1\n")),
        k.clone().prop_map(|k| format!("{k} = -12\n")),
        k.clone().prop_map(|k| format!("{k} = 0x1a2B\n")),
        k.clone().prop_map(|k| format!("{k} = 0.5\n")),
        k.clone().prop_map(|k| format!("{k} = true\n")),
        k.clone().prop_map(|k| format!("{k} = false\n")),
        (k.clone(), text.clone()).prop_map(|(k, t)| format!("{k} = \"{t}\"\n")),
        (k.clone(), text.clone()).prop_map(|(k, t)| format!("{k} = \"{t}\" # note\n")),
        k.clone().prop_map(|k| format!("{k} = [\"a\", \"b\"]\n")),
        k.clone().prop_map(|k| format!("{k} = [\n  1,\n  2,\n]\n")),
        k.clone()
            .prop_map(|k| format!("{k} = [[\"a\"], [\"b\", \"c\"]]\n")),
        k.clone().prop_map(|k| format!("{k} = []\n")),
        prop::sample::select(vec![
            "[general]\n",
            "[ colors ]\n",
            "[[rules]]\n",
            "[a.b]\n"
        ])
        .prop_map(|s| s.to_string()),
        Just("# comment [with] brackets\n".to_string()),
        Just("\n".to_string()),
        Just("   \t\r\n".to_string()),
        // Everything below is outside the documented subset.
        Just("[general\n".to_string()),
        Just("[[\n".to_string()),
        Just("[]\n".to_string()),
        Just("= 1\n".to_string()),
        Just("a.b = 1\n".to_string()),
        Just("key = 'single'\n".to_string()),
        Just("key = 1.0e5\n".to_string()),
        Just("key = 1__0\n".to_string()),
        Just("key = \"open\n".to_string()),
        Just("key = \"\\q\"\n".to_string()),
        Just("key = [1, \"mixed\"]\n".to_string()),
        Just("key = [1, 2\n".to_string()),
        Just("key =\n".to_string()),
        Just("key 1\n".to_string()),
        Just("key = 1 key = 2\n".to_string()),
    ]
    .boxed()
}

/// Where two occurrences of the same key sit, which is the only thing that
/// decides whether they collide.
#[derive(Clone, Copy, Debug)]
enum Shape {
    SameTable,
    SameArrayRow,
    TopLevel,
    DifferentTables,
    DifferentArrayRows,
    SingleKey,
}

/// One character of a string literal: the five escapable ones plus ordinary
/// text, and never NUL (which the scanner rejects as a terminator).
fn string_char() -> BoxedStrategy<char> {
    prop_oneof![
        Just('"'),
        Just('\\'),
        Just('\n'),
        Just('\r'),
        Just('\t'),
        Just('\u{1}'),
        Just('a'),
        Just(' '),
        Just('é'),
        Just('日'),
    ]
    .boxed()
}

/// Render `s` as a basic string using exactly the escapes the subset decodes.
/// The inverse of the parser's decode table, so a correctly encoded literal
/// round-trips back to the text it was written from.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

proptest! {
    /// Parsing is total, and every event it does produce is a member of the
    /// documented grammar: a bare key, a trimmed non-empty header name, a
    /// well-known diagnostic tag.
    #[test]
    fn parse_is_total_and_its_events_stay_inside_the_documented_grammar(
        pieces in prop::collection::vec(piece(), 0..8)
    ) {
        let src = pieces.concat();
        for event in parse(&src) {
            match event {
                Ok(Event::Section(name)) | Ok(Event::ArraySection(name)) => {
                    prop_assert!(!name.is_empty(), "empty header name in {:?}", src);
                    prop_assert_eq!(name, name.trim(), "untrimmed header name in {:?}", src);
                    prop_assert!(!name.contains(']'), "header name keeps a bracket in {:?}", src);
                }
                Ok(Event::KeyValue(key, _value)) => {
                    prop_assert!(!key.is_empty(), "empty key in {:?}", src);
                    prop_assert!(
                        key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                        "key {:?} leaves the bare-ASCII grammar in {:?}",
                        key,
                        src
                    );
                }
                Err(e) => prop_assert!(
                    KINDS.contains(&e.kind),
                    "undocumented diagnostic tag {:?} in {:?}",
                    e.kind,
                    src
                ),
            }
        }
    }

    /// The stream is fused: the first fault ends it, and once it has ended it
    /// never yields another item. Callers consume the parser exactly once, so
    /// a stream that kept going would silently re-read the rest of the file.
    #[test]
    fn the_stream_is_fused_after_a_fault_and_after_its_end(
        head in prop::collection::vec(piece(), 0..2),
        tail in prop::collection::vec(piece(), 1..4)
    ) {
        let src = head.concat() + &tail.concat();
        let mut parser = parse(&src);
        let mut faulted = false;
        let mut items = 0usize;
        for item in parser.by_ref() {
            match item {
                Ok(_) => prop_assert!(!faulted, "an event after the first fault in {:?}", src),
                Err(_) => faulted = true,
            }
            items += 1;
            prop_assert!(items <= 4_096, "the stream did not terminate on {:?}", src);
        }
        prop_assert!(parser.next().is_none(), "the parser revived after its end");
        prop_assert!(parser.next().is_none(), "the parser revived after its end");
    }

    /// A fault is reported on a line that exists in the source: `line` is
    /// 1-based, and the scanner only ever counts newlines it has consumed.
    #[test]
    fn a_fault_is_reported_on_a_line_that_exists_in_the_source(
        pieces in prop::collection::vec(piece(), 1..6)
    ) {
        let src = pieces.concat();
        let lines = src.split('\n').count();
        for item in parse(&src) {
            if let Err(e) = item {
                prop_assert!(e.line >= 1, "line {} is not 1-based in {:?}", e.line, src);
                prop_assert!(e.line <= lines, "line {} is past the {} lines of {:?}",
                    e.line, lines, src);
            }
        }
    }

    /// Where the two occurrences of a key sit is the only thing that decides
    /// whether they collide: same table (including the same `[[row]]`) is a
    /// `duplicate-key` fault, a different table is legal.
    #[test]
    fn a_repeated_key_collides_only_inside_one_table(
        shape in prop_oneof![
            Just(Shape::SameTable),
            Just(Shape::SameArrayRow),
            Just(Shape::TopLevel),
            Just(Shape::DifferentTables),
            Just(Shape::DifferentArrayRows),
            Just(Shape::SingleKey),
        ],
        key in key(),
        first in 0i64..1000,
        second in 0i64..1000,
    ) {
        let (src, expect_fault, expect_items) = match shape {
            Shape::SameTable => (
                format!("[general]\n{key} = {first}\n{key} = {second}\n"),
                true,
                2,
            ),
            Shape::SameArrayRow => (
                format!("[[rules]]\n{key} = {first}\n{key} = {second}\n"),
                true,
                2,
            ),
            Shape::TopLevel => (format!("{key} = {first}\n{key} = {second}\n"), true, 2),
            Shape::DifferentTables => (
                format!("[general]\n{key} = {first}\n[colors]\n{key} = {second}\n"),
                false,
                4,
            ),
            Shape::DifferentArrayRows => (
                format!("[[rules]]\n{key} = {first}\n[[rules]]\n{key} = {second}\n"),
                false,
                4,
            ),
            Shape::SingleKey => (format!("[general]\n{key} = {first}\n"), false, 2),
        };
        if expect_fault {
            prop_assert_eq!(faults(&src), vec!["duplicate-key"],
                "expected a duplicate-key fault in {:?}", src);
        } else {
            prop_assert!(faults(&src).is_empty(), "unexpected fault in {:?}", src);
            prop_assert_eq!(stream(&src).len(), expect_items, "events of {:?}", src);
        }
    }

    /// An escape-free string is borrowed from the caller's buffer — the reason
    /// this crate exists instead of a `serde` stack — and a string carrying an
    /// escape is decoded into an owned buffer instead.
    #[test]
    fn a_string_is_borrowed_exactly_when_it_carries_no_escape(
        head in plain_text(),
        tail in prop::collection::vec(string_char(), 0..4),
    ) {
        let text = head + &tail.into_iter().collect::<String>();
        let src = format!("[general]\ntitle = {}\n", escape(&text));
        let value = only_pair(&src);
        prop_assert!(value.is_some(), "no title pair in {:?}", src);

        let carries_escape = text.chars().any(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '\t'));
        let value = value.unwrap();
        prop_assert_eq!(value.as_str(), Some(text.as_str()), "decoded text in {:?}", src);
        let owned = matches!(value, Value::Str(Cow::Owned(_)));
        prop_assert_eq!(owned, carries_escape,
            "ownership must follow the presence of an escape in {:?}", src);
        if let Value::Str(Cow::Borrowed(s)) = &value {
            let base = src.as_ptr() as usize;
            let borrowed = s.as_ptr() as usize;
            prop_assert!(
                borrowed >= base && borrowed + s.len() <= base + src.len(),
                "an escape-free string must point into the source buffer, not a copy"
            );
        }
    }

    /// Every character a string literal can carry survives the escape/unescape
    /// round trip: encoding a decoded value and decoding it again is the
    /// identity, which is what lets a config file be edited and reloaded
    /// without a value silently changing.
    #[test]
    fn escaping_and_unescaping_a_literal_is_the_identity(
        chars in prop::collection::vec(string_char(), 0..10)
    ) {
        let text: String = chars.into_iter().collect();
        let src = format!("title = {}\n", escape(&text));
        prop_assert!(faults(&src).is_empty(), "unexpected fault in {:?}", src);
        let value = only_pair(&src);
        prop_assert_eq!(
            value.as_ref().and_then(|v| v.as_str()),
            Some(text.as_str()),
            "round trip of {:?}",
            src
        );
    }

    /// A literal sitting exactly on its documented bound is accepted, and one
    /// element past it is rejected. The bound is a promise in both directions:
    /// too tight rejects a config the subset is defined to accept, too loose
    /// lets a hostile file spend unbounded CPU in the integer scanners.
    #[test]
    fn a_literal_is_accepted_on_its_bound_and_rejected_one_past_it(
        over in prop_oneof![Just(0usize), Just(1), Just(2), Just(4)]
    ) {
        // Floats: `MAX_NUMBER_LEN` counts the sign, the point and every digit.
        let float = |len: usize| format!("f = 0.{}\n", "1".repeat(len - 2));
        prop_assert!(only_pair(&float(MAX_NUMBER_LEN)).is_some(),
            "a {}-character number is inside the bound", MAX_NUMBER_LEN);
        prop_assert_eq!(faults(&float(MAX_NUMBER_LEN + 1)), vec!["value"],
            "one character past the number bound must be rejected");

        // Hex: the bound is only observable where the decoded value still
        // fits, which is an integer array — a bare `0x…` is u32-bounded and
        // would hide the scanner's own limit behind an overflow instead.
        let hex = |digits: usize| format!("h = [0x{}]\n", "1".repeat(digits));
        let on_bound = match only_pair(&hex(MAX_HEX_DIGITS)) {
            Some(Value::IntList(v)) => v,
            other => {
                prop_assert!(false, "{} hex digits must decode, got {:?}", MAX_HEX_DIGITS, other);
                Vec::new()
            }
        };
        prop_assert_eq!(on_bound,
            vec![i64::from_str_radix(&"1".repeat(MAX_HEX_DIGITS), 16).unwrap()],
            "a hex literal on the digit bound decodes to the value it spells");

        // Arrays: the element cap, not the byte count, is what is bounded.
        let list = |n: usize| format!("s = [{}]\n", vec!["\"x\""; n].join(","));
        match only_pair(&list(MAX_ARRAY_ELEMS)) {
            Some(Value::StrList(v)) => prop_assert_eq!(v.len(), MAX_ARRAY_ELEMS),
            other => prop_assert!(false, "{} elements must be accepted, got {:?}",
                MAX_ARRAY_ELEMS, other),
        }
        prop_assert_eq!(faults(&list(MAX_ARRAY_ELEMS + 1 + over)), vec!["array"],
            "one element past the array bound must be rejected");
    }
}

// Each case of the next property materialises more than a megabyte of input,
// so the case count is small for that reason alone: the property itself is the
// same shape as the bound checks above.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// A string at the bound is accepted and one past it is rejected, so a
    /// hostile file cannot make the parser copy an unbounded buffer.
    #[test]
    fn a_string_literal_is_accepted_on_its_bound_and_rejected_one_past_it(
        over in prop_oneof![Just(1usize), Just(2), Just(64)]
    ) {
        let literal = |len: usize| format!("s = \"{}\"\n", "a".repeat(len));
        match only_pair(&literal(MAX_STRING_LEN)) {
            Some(Value::Str(s)) => prop_assert_eq!(s.len(), MAX_STRING_LEN),
            other => prop_assert!(false, "a {}-character string is inside the bound, got {:?}",
                MAX_STRING_LEN, other),
        }
        prop_assert_eq!(faults(&literal(MAX_STRING_LEN + 1 + over)), vec!["string"],
            "past the string bound the literal must be rejected");
    }
}

/// A value in the accepted subset, paired with the event it must produce.
#[derive(Clone, Debug)]
enum Kind {
    Int(i64),
    Hex(u32),
    Float(f64),
    Bool(bool),
    Text(String),
    StrList(Vec<String>),
    IntList(Vec<i64>),
    Grid(Vec<Vec<String>>),
    Empty,
}

/// Spell a float so the parser reads it back as a float.
///
/// `{}` prints a whole number without a fractional part, and `0` is an integer
/// literal — so `1/2` as `0.5` round-trips but `0/1` as `0` comes back an
/// integer. TOML keeps those two types apart, and the event model here does
/// too, so the renderer has to supply the part that carries the type.
fn render_float(v: f64) -> String {
    let text = format!("{v}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

/// Render `kind` as config text plus the value the parser owes for it.
fn render(kind: &Kind) -> (String, Value<'static>) {
    match kind {
        Kind::Int(v) => (format!("{v}"), Value::Integer(*v)),
        Kind::Hex(v) => (format!("0x{v:x}"), Value::Hex(*v)),
        Kind::Float(v) => (render_float(*v), Value::Float(*v)),
        Kind::Bool(v) => (
            if *v {
                "true".to_string()
            } else {
                "false".to_string()
            },
            Value::Boolean(*v),
        ),
        Kind::Text(s) => (format!("\"{s}\""), Value::Str(Cow::Owned(s.clone()))),
        Kind::StrList(v) => {
            let mut text = v
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            if v.len() > 1 {
                text.push(',');
            }
            let value = v.iter().map(|s| Cow::Owned(s.clone())).collect();
            (format!("[{text}]"), Value::StrList(value))
        }
        Kind::IntList(v) if v.is_empty() => empty_array(),
        Kind::IntList(v) => {
            // A trailing comma is part of the subset, so the canonical form of
            // a multi-element array carries one.
            let mut text = v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
            if v.len() > 1 {
                text.push(',');
            }
            (format!("[{text}]"), Value::IntList(v.clone()))
        }
        Kind::Grid(rows) if rows.is_empty() => empty_array(),
        Kind::Grid(rows) => {
            let text = rows
                .iter()
                .map(|row| {
                    let cells = row.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>();
                    format!("[{}]", cells.join(", "))
                })
                .collect::<Vec<_>>()
                .join(",\n");
            let value = rows
                .iter()
                .map(|row| row.iter().map(|s| Cow::Owned(s.clone())).collect())
                .collect();
            (format!("[{text}]"), Value::Grid(value))
        }
        // An empty array is reported as an empty string list; consumers check
        // emptiness, not the element kind.
        Kind::Empty => empty_array(),
    }
}

/// `[]` has no first element to classify by, so it is reported as an empty
/// string list whichever array syntax introduced it.
fn empty_array() -> (String, Value<'static>) {
    ("[]".to_string(), Value::StrList(Vec::new()))
}

fn kind() -> BoxedStrategy<Kind> {
    prop_oneof![
        any::<i64>().prop_map(Kind::Int),
        any::<u32>().prop_map(Kind::Hex),
        (-9999i64..9999, 1i64..9999).prop_map(|(a, b)| Kind::Float(a as f64 / b as f64)),
        any::<bool>().prop_map(Kind::Bool),
        plain_text().prop_map(Kind::Text),
        prop::collection::vec(plain_text(), 0..3).prop_map(Kind::StrList),
        prop::collection::vec(0i64..5000, 0..3).prop_map(Kind::IntList),
        prop::collection::vec(prop::collection::vec(plain_text(), 0..2), 0..3).prop_map(Kind::Grid),
        Just(Kind::Empty),
    ]
    .boxed()
}

proptest! {
    /// A document written in the subset parses back into exactly the events it
    /// encodes. Comments, blank lines and horizontal whitespace are not part of
    /// the event stream; everything else is, in file order.
    #[test]
    fn a_canonical_document_parses_back_into_the_events_it_encodes(
        table in prop::sample::select(vec!["general", "colors", "autostart", "a-b_c"]),
        pairs in prop::collection::vec((key(), kind()), 0..4),
        comments in prop::collection::vec(
            prop::sample::select(vec!["# note\n", "# [not] a header\n", "\n", "  \t\n"]),
            0..4,
        ),
        array_rows in prop::collection::vec(kind(), 0..2),
    ) {
        let mut src = String::new();
        let mut expected: Vec<Event<'static>> = Vec::new();
        for filler in comments {
            src.push_str(filler);
        }
        for (k, kind) in pairs {
            let (text, value) = render(&kind);
            // Inner whitespace and a trailing header comment are part of the
            // accepted subset but not of the event stream.
            src.push_str(&format!("[ {table} ] # table {k}\n{k} = {text}\n"));
            expected.push(Event::Section(table));
            expected.push(Event::KeyValue(k, value));
        }
        for kind in array_rows {
            let (text, value) = render(&kind);
            src.push_str(&format!("[[ {table} ]]\nkey = {text} # row\n"));
            expected.push(Event::ArraySection(table));
            expected.push(Event::KeyValue("key", value));
        }

        let events = stream(&src).into_iter().collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("{:?} rejected: {:?}", src, e));
        prop_assert_eq!(format!("{:?}", events), format!("{:?}", expected),
            "events of {:?}", src);
    }
}
