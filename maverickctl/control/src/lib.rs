//! Control wire contract and client-side identity/JSON helpers.
//! The reference server exists only for independent regression tests.
pub mod identity;
pub mod json;
pub mod protocol;
pub use identity::{self_info, InstanceInfo, DEFAULT_NAME};
#[cfg(feature = "test-server")]
pub mod control;
#[cfg(feature = "test-server")]
pub mod hub;
#[cfg(feature = "test-server")]
pub use control::ControlServer;
#[cfg(feature = "test-server")]
pub use hub::{ControlCommand, ControlHub};
#[cfg(feature = "test-server")]
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(feature = "test-server")]
pub fn wait_readable_fds(
    fds: &[std::os::unix::io::RawFd],
    timeout: Option<std::time::Duration>,
) -> bool {
    wait_fds(fds, timeout, None)
}

#[cfg(feature = "test-server")]
fn wait_fds(
    fds: &[std::os::unix::io::RawFd],
    timeout: Option<std::time::Duration>,
    signal: Option<&OwnedFd>,
) -> bool {
    if fds.is_empty() && signal.is_none() {
        return true;
    }
    let mut pfds: Vec<rustix::event::PollFd<'_>> = fds
        .iter()
        .copied()
        .chain(signal.map(AsRawFd::as_raw_fd))
        // SAFETY: every descriptor here is one the caller is already holding
        // open and passes in solely to be waited on, and the borrow does not
        // outlive this call.
        .map(|fd| unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) })
        .map(|fd| rustix::event::PollFd::from_borrowed_fd(fd, rustix::event::PollFlags::IN))
        .collect();
    let ts = timeout.map(|v| {
        // `tv_nsec` is a remainder, not a total: `poll(2)` reads the pair as
        // one value and a `tv_nsec` of 2e9 is undefined rather than 2 seconds.
        let total = v.as_secs().min(i64::MAX as u64);
        let nanos = v.subsec_nanos();
        rustix::event::Timespec {
            tv_sec: total as i64,
            tv_nsec: nanos as i64,
        }
    });
    let r = rustix::event::poll(&mut pfds, ts.as_ref());
    if let Some(signal) = signal.filter(|_| {
        pfds.last()
            .is_some_and(|p| p.revents().contains(rustix::event::PollFlags::IN))
    }) {
        // One eventfd read atomically drains the accumulated counter. A signal
        // arriving afterwards leaves a new readable counter for the next wait.
        let _ = rustix::io::read(signal, &mut [0_u8; 8]);
    }
    match r {
        Ok(0) => false,
        Ok(_) => pfds
            .iter()
            .any(|p| p.revents().contains(rustix::event::PollFlags::IN)),
        // Any error is a wakeup, including EINTR: the caller re-checks its
        // flags either way, and a busy loop here would spin the WM.
        Err(_) => true,
    }
}

#[cfg(test)]
pub(crate) mod prop_support {
    use proptest::prelude::*;
    use proptest::string::string_regex;

    /// Where a failing property persists its counterexample.
    ///
    /// `proptest!` always stamps the config with `file!()`, so a property
    /// compiled into the library would drop its regression file under the crate
    /// root next to the module it covers. The persistence root is redirected
    /// into `tests/` instead, keeping every counterexample in this crate under
    /// the same tree as the integration property suites.
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

    /// Free-form text a user, a client or a window can put into a field:
    /// instance names, window titles, executable paths, CLI words. Quotes,
    /// backslashes, separators, control bytes and non-ASCII are
    /// over-represented, because those are the characters that decide whether a
    /// payload survives the JSON escaper, the line framing of the control
    /// protocol and the hand-rolled ficha reader.
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
    /// [`crate::identity::runtime_dir`] resolves `$XDG_RUNTIME_DIR/maverick` and
    /// falls back to `/run/user/$UID/maverick` when that is unset — so a unit
    /// test here that stands up a control server publishes a session directory
    /// and a socket into the *user's* Maverick runtime. Nothing there is owned by
    /// the test, and a fixture that cleans up too broadly takes a live instance's
    /// control socket and identity record with it.
    ///
    /// Each test executable owns a fresh private directory under `/tmp`, not
    /// the login session's `/run/user/$UID`. The short prefix leaves room for
    /// the longest realistic session id in `sockaddr_un`. Explicit XDG paths
    /// are allowed under `/tmp`; the production fallback is tested separately.
    /// Written once per process and read through the same `OnceLock`, so every
    /// test that touches the runtime root sees the same constant. Each such test
    /// calls this *before* it touches anything, which is what makes the single
    /// write ordered before every read rather than racing one.
    pub fn runtime_root() -> &'static std::path::Path {
        use std::sync::OnceLock;
        static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
        ROOT.get_or_init(|| {
            let dir = tempfile::Builder::new()
                .prefix("ms-")
                .tempdir_in("/tmp")
                .expect("test runtime root");
            crate::identity::set_private_dir(dir.path()).expect("private test runtime root");
            std::env::set_var("XDG_RUNTIME_DIR", dir.path());
            // Verified rather than assumed: if the redirect did not take,
            // every caller below this line would publish into the user's
            // own runtime directory, so the failure has to surface here
            // rather than as a stray socket much later.
            assert!(
                crate::identity::runtime_dir().starts_with(dir.path()),
                "the test runtime root is not in effect: unit tests would publish into {}",
                crate::identity::runtime_dir().display()
            );
            dir
        })
        .path()
    }

    /// Retire the session directory a fixture caused to be created.
    ///
    /// `ControlServer::shutdown` unlinks the socket and deliberately stops
    /// there: in production that same directory holds the spec file, the
    /// `Xauthority` cookie and the window manager's logs, so the server does not
    /// own it. A test fixture does own the directory it made, and this is how it
    /// gives it back.
    ///
    /// Exact path and non-recursive, which is what makes it safe to run while
    /// another process is part-way through its own fixture in the same runtime
    /// root: a directory that still holds a socket simply fails to be removed
    /// rather than being emptied.
    pub fn retire(name: &str) {
        let dir =
            crate::identity::try_session_dir(name).expect("a fixture owns a valid session id");
        let _ = std::fs::remove_dir(dir);
    }

    /// A teardown that only ever leaves an empty directory behind would pass
    /// every assertion while still being the wrong thing: the moment a directory
    /// holds something the fixture did not put there, a recursive removal takes
    /// that too. Both halves are pinned instead — a neighbour survives, and
    /// unexpected content inside the fixture's own directory stops the removal
    /// rather than being destroyed with it.
    #[test]
    fn retiring_a_fixture_directory_is_scoped_by_ownership() {
        runtime_root();
        crate::identity::ensure_runtime_dir().expect("runtime dir");

        let owned = format!("retire-owned-{}", std::process::id());
        let neighbour = format!("retire-neighbour-{}", std::process::id());
        let owned_dir = crate::identity::try_session_dir(&owned).expect("owned dir");
        let neighbour_dir = crate::identity::try_session_dir(&neighbour).expect("neighbour dir");
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
}
