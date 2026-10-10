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
/// Each test executable owns a fresh private directory under `/tmp`, not the
/// login session's `/run/user/$UID`. The short prefix leaves room for realistic
/// session ids in `sockaddr_un`. Explicit XDG paths may be under `/tmp`; the
/// production fallback is tested separately in `maverick-sys`.
/// Written once per process and read through the same `OnceLock`, so every test
/// that touches the runtime root sees the same constant. Each such test calls
/// this *before* it touches anything, which is what makes the single write
/// ordered before every read rather than racing one.
pub fn runtime_root() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("mc-")
            .tempdir_in("/tmp")
            .expect("test runtime root");
        maverick_sys::identity::set_private_dir(dir.path()).expect("private test runtime root");
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        // Verified rather than assumed: if the redirect did not take, every
        // caller below this line would publish into the user's own runtime
        // directory, so the failure has to surface here rather than as a stray
        // socket much later.
        assert!(
            maverick_sys::identity::runtime_dir().starts_with(dir.path()),
            "the test runtime root is not in effect: unit tests would publish into {}",
            maverick_sys::identity::runtime_dir().display()
        );
        dir
    })
    .path()
}

/// Retire the session directory a fixture caused to be created.
///
/// `ControlServer::shutdown` unlinks the socket and deliberately stops there:
/// in production that same directory holds the spec file, the `Xauthority`
/// cookie and the window manager's logs, so the server does not own it. A test
/// fixture does own the directory it made, and this is how it gives it back.
///
/// Exact path and non-recursive, which is what makes it safe to run while
/// another process is part-way through its own fixture in the same runtime root:
/// a directory that still holds a socket simply fails to be removed rather than
/// being emptied.
pub fn retire(name: &str) {
    let dir =
        maverick_sys::identity::try_session_dir(name).expect("a fixture owns a valid session id");
    let _ = std::fs::remove_dir(dir);
}

/// A teardown that only ever leaves an empty directory behind would pass every
/// assertion while still being the wrong thing: the moment a directory holds
/// something the fixture did not put there, a recursive removal takes that too.
/// Both halves are pinned instead — a neighbour survives, and unexpected
/// content inside the fixture's own directory stops the removal rather than
/// being destroyed with it.
#[test]
fn retiring_a_fixture_directory_is_scoped_by_ownership() {
    runtime_root();
    maverick_sys::identity::ensure_runtime_dir().expect("runtime dir");

    let owned = format!("retire-owned-{}", std::process::id());
    let neighbour = format!("retire-neighbour-{}", std::process::id());
    let owned_dir = maverick_sys::identity::try_session_dir(&owned).expect("owned dir");
    let neighbour_dir = maverick_sys::identity::try_session_dir(&neighbour).expect("neighbour dir");
    std::fs::create_dir_all(&owned_dir).expect("owned dir");
    std::fs::create_dir_all(&neighbour_dir).expect("neighbour dir");
    std::fs::write(neighbour_dir.join("keep"), b"x").expect("neighbour file");

    retire(&owned);

    assert!(
        !owned_dir.exists(),
        "the directory the fixture created must be gone"
    );
    assert!(
        neighbour_dir.join("keep").exists(),
        "cleanup reaches one directory by exact path, never a sibling"
    );

    std::fs::create_dir_all(&owned_dir).expect("owned dir again");
    std::fs::write(owned_dir.join("unexpected"), b"x").expect("unexpected file");
    retire(&owned);
    assert!(
        owned_dir.join("unexpected").exists(),
        "a directory that is not empty is left alone rather than emptied"
    );

    let _ = std::fs::remove_dir_all(&neighbour_dir);
    let _ = std::fs::remove_dir_all(&owned_dir);
}
