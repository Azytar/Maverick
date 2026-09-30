//! Test-only strategies for this crate's property suites.
//!
//! A twin of the `{config, text}` pair in `maverick-sys`'s `prop_support`:
//! `#[cfg(test)]` items cannot cross crates, so each crate carries its own
//! copy. The two must stay equivalent; both exist only to feed hostile text
//! to CLI payloads and to persist counterexamples under `tests/`.

use proptest::prelude::*;
use proptest::string::string_regex;

/// Where a failing property persists its counterexample: under this
/// crate's `tests/`, next to the integration property suites.
pub fn config() -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::SourceParallel(
                "tests/proptest-regressions",
            ),
        )),
        ..proptest::test_runner::Config::default()
    }
}

/// Free-form text a user or a client can put into a CLI word: quotes,
/// backslashes, separators, control bytes and non-ASCII are
/// over-represented, because those are the characters that decide whether
/// a payload survives the CLI parser and the line framing of the control
/// protocol.
pub fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => proptest::collection::vec(
                prop_oneof![
                    any::<char>(),
                    Just('"'),
                    Just('\\'),
                    Just('/'),
                    Just('\n'),
                    Just('\r'),
                    Just('\t'),
                    Just('\u{0000}'),
                    Just('\u{000c}'),
                    Just('\u{001f}'),
                    Just('\u{007f}'),
                    Just('\u{00e9}'),
                    Just('\u{1f600}'),
                ],
                0..24,
            )
            .prop_map(|cs| cs.into_iter().collect()),
        2 => string_regex("[\"\\\\,{}: \x00-\x1f]{0,16}").expect("static pattern"),
        1 => string_regex("[^\x00-\x7f]{0,12}").expect("static pattern"),
        1 => string_regex(".").expect("static pattern"),
    ]
}
