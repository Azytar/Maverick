//! Shared strategies for the `maverick-sys` property suite.
//!
//! The text strategies below are deliberately hostile: quotes, backslashes,
//! separators, control bytes and non-ASCII are over-represented, because those
//! are exactly the characters that decide whether a payload survives the JSON
//! escaper, the line framing of the control protocol and the hand-rolled ficha
//! reader.

#![allow(dead_code)]

use proptest::prelude::*;
use proptest::string::string_regex;

/// Compile one of the literal patterns used below. The `expect` records the one
/// precondition they share: no construct proptest cannot turn into a strategy
/// may creep into these patterns.
pub fn regex_s(pattern: &'static str) -> impl Strategy<Value = String> {
    string_regex(pattern).expect("pattern must be expressible as a strategy")
}

/// Characters that decide a JSON string's grammar and a line frame's integrity,
/// plus both ends of the C0 range the escaper encodes as `\uXXXX`.
fn nasty_char() -> impl Strategy<Value = char> {
    prop_oneof![
        any::<char>(),
        Just('"'),
        Just('\\'),
        Just('/'),
        Just('\n'),
        Just('\r'),
        Just('\t'),
        Just('\u{0000}'),
        Just('\u{0008}'),
        Just('\u{000c}'),
        Just('\u{001f}'),
        Just('\u{0020}'),
        Just('\u{007f}'),
        Just('\u{00e9}'),
        Just('\u{20ac}'),
        Just('\u{1f600}'),
    ]
}

/// Free-form text a user, a client or a window can put into a field: instance
/// names, window titles, executable paths, CLI words.
pub fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => proptest::collection::vec(nasty_char(), 0..24).prop_map(|cs| cs.into_iter().collect()),
        2 => regex_s("[\"\\\\,{}: \x00-\x1f]{0,16}"),
        1 => regex_s("[^\x00-\x7f]{0,12}"),
        1 => regex_s("."),
    ]
}

/// Text built only from escape material — lone backslashes, `\u` runs of every
/// length, quotes. No WM ever emits it; it arrives from a hand-edited, stale or
/// hostile ficha, and it is what the decoder's branches are written for.
pub fn escape_soup() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => proptest::collection::vec(nasty_char(), 0..24).prop_map(|cs| cs.into_iter().collect()),
        3 => regex_s("(\\\\u|\\\\|\\\\u[0-9a-fA-F]{0,4}|[0-9a-fA-F]|\"|[bfrnt/]){0,16}"),
    ]
}

/// Free-form text long enough to cross the protocol's line bound, so the paths
/// that truncate a payload are reached instead of merely approached.
pub fn oversized_text(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
    (min_len..=max_len, text()).prop_map(|(pad_len, tail)| {
        let mut s = "x".repeat(pad_len);
        s.push_str(&tail);
        s
    })
}

/// Point `XDG_RUNTIME_DIR` at an empty throwaway directory for this test binary.
///
/// The CLI entry point resolves its target by reading the runtime directory, and
/// an unknown word is *forwarded* to whatever instance that resolves to. Without
/// isolation the properties here would depend on whether the machine running
/// them happens to have a live window manager — and, worse, `list` and `prune`
/// would read and delete the developer's real sessions.
///
/// The value is set once and to the same path for every test in the binary, so
/// two tests racing to set it cannot disagree.
pub fn isolate_runtime_dir() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("maverick-ctl-props-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
    });
}
