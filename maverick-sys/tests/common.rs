//! Shared strategies for this crate's JSON property suite.
//!
//! A twin of the text generators in `maverickctl/tests/common`: each crate's
//! integration tests must be self-contained (`#[cfg(test)]` items cannot cross
//! crates), so the `{nasty_char, regex_s, text}` generators exist in both
//! places and must stay equivalent. Only what the JSON escaper tests need
//! lives here; CLI runtime isolation lives with the CLI.

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
