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

/// The runtime root this test process publishes, created once.
///
/// A twin of `maverick_sys`'s `prop_support::runtime_root`, for the same reason
/// the pair above is duplicated: `#[cfg(test)]` items cannot cross crates.
///
/// `maverick_sys::identity::runtime_dir` resolves `$XDG_RUNTIME_DIR/maverick` and
/// falls back to `/run/user/$UID/maverick` when that is unset, so a unit test
/// that stands up a control server would publish a session directory and a
/// socket into the user's own Maverick runtime. Nothing there belongs to the
/// test, and a fixture that cleans up too broadly takes a live instance's
/// control socket and identity record with it.
///
/// The root lives under this crate's `target/`, reached through the compile-time
/// manifest path so it does not depend on the working directory the test binary
/// was launched from. It is deliberately not under `/tmp`.
///
/// The root lives under `$XDG_RUNTIME_DIR`'s usual per-user parent, as a
/// sibling of the real `maverick` directory rather than a child of it, and
/// it is deliberately short: `sock_path_fits_sun_len` requires the longest
/// realistic sid to keep `control.sock` under the 108-byte `sockaddr_un`
/// limit, so this fixture root has only a few bytes of headroom over the
/// production fallback. A longer or more descriptive name breaks that
/// invariant rather than just looking untidy.
/// Written once per process and read through the same `OnceLock`, so every test
/// that touches the runtime root sees the same constant. Each such test calls
/// this *before* it touches anything, which is what makes the single write
/// ordered before every read rather than racing one.
pub fn runtime_root() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::path::Path::new("/run/user")
            .join(maverick_sys::identity::current_uid().to_string())
            .join("maverick-t");
        std::fs::create_dir_all(&dir).expect("test runtime root");
        maverick_sys::identity::set_private_dir(&dir).expect("private test runtime root");
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        // Verified rather than assumed: if the redirect did not take, every
        // caller below this line would publish into the user's own runtime
        // directory, so the failure has to surface here rather than as a stray
        // socket much later.
        assert!(
            maverick_sys::identity::runtime_dir().starts_with(&dir),
            "the test runtime root is not in effect: unit tests would publish into {}",
            maverick_sys::identity::runtime_dir().display()
        );
        dir
    })
    .as_path()
}
