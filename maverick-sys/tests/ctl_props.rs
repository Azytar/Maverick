//! Properties of the `maverickctl` CLI entry point.
//!
//! Everything the tools do beyond argument parsing needs a live instance — a
//! socket, a ficha, a WM thread — so this suite covers the one part that is
//! decided by argv alone: which exit code a word maps to, and whether the tool
//! ever guesses at an instance it was not pointed at.

mod common;

use maverick_sys::ctl::main_with_args;
use proptest::prelude::*;
use std::process::ExitCode;

/// Every word `maverickctl` handles itself, and every word it would forward to
/// an instance.
///
/// Excluded from the random-word property for two reasons that used to be one:
/// a handled word must produce its *own* documented exit code (a separate
/// property below), and a word the tool does not know is now forwarded verbatim
/// — so a random word *is* a request to act on whatever instance the context
/// resolves to. The list has to keep up with the command surface, or the
/// property would start failing the day a verb is added.
const INSTANCE_WORDING: [&str; 26] = [
    // instance commands
    "list",
    "ls",
    "state",
    "query",
    "q",
    "msg",
    "dispatch",
    "command",
    "subscribe",
    "sub",
    "quit",
    "quit-all",
    "restart",
    "reload",
    "prune",
    // sessions
    "session",
    "sessions",
    "sess",
    "exec",
    "shell",
    "attach",
    // windows, processes and layout
    "window",
    "win",
    "process",
    "proc",
    "camera",
];

/// The groups that own a session-scoped command tree.
const GROUPS: [&str; 3] = ["session", "window", "process"];

/// A word the admin tool can be given that decides its exit code without
/// leaving the process: the help forms, and anything it does not know.
fn local_word() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => prop::sample::select(vec!["-h", "--help", "help", "h"]).prop_map(String::from),
        3 => common::text(),
    ]
}

// The entry point is total and stateless: any word is either a documented help
// form or an unknown command that fails loudly, and the same argv always
// decides the same way. An unknown word must never fall through to the
// verbatim-forwarding path, which would act on whatever instance the
// context happens to resolve to.
proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    #[test]
    fn the_admin_tool_answers_a_documented_exit_code(word in local_word()) {
        prop_assume!(
            !INSTANCE_WORDING.contains(&word.as_str()),
            "word {:?} would reach for an instance",
            word
        );
        let first = main_with_args("maverickctl", vec![word.clone()]);
        let second = main_with_args("maverickctl", vec![word.clone()]);
        prop_assert_eq!(first, second, "the same argv must decide identically");
        let wants_help = matches!(word.as_str(), "-h" | "--help" | "help" | "h");
        let want = if wants_help { ExitCode::SUCCESS } else { ExitCode::FAILURE };
        prop_assert_eq!(first, want, "word {:?} mapped to the wrong exit code", word);

        // No command at all is the documented failure, not a silent success.
        prop_assert_eq!(main_with_args("maverickctl", vec![]), ExitCode::FAILURE);
    }
}

/// Every command group answers `--help` with usage and success.
///
/// This is the one hermetic check that actually *discriminates* a handled verb
/// from a forwarded one: `maverickctl session --help` prints usage and succeeds,
/// whereas a group name that fell through to the verbatim path would try to
/// forward it, find no instance in the isolated runtime directory, and fail.
/// The property suite above cannot make that distinction — both are FAILURE —
/// so it lives here, where success is only reachable by having handled the word.
#[test]
fn every_command_group_handles_its_own_help() {
    common::isolate_runtime_dir();
    for group in GROUPS {
        assert_eq!(
            main_with_args("maverickctl", vec![group.to_string(), "--help".to_string()]),
            ExitCode::SUCCESS,
            "`{group} --help` must print its usage and succeed, not be forwarded"
        );
    }
    for help in ["-h", "--help", "help", "h"] {
        assert_eq!(
            main_with_args("maverickctl", vec![help.to_string()]),
            ExitCode::SUCCESS,
            "`{help}` must print the top-level usage and succeed"
        );
    }
}

/// A listing that cannot produce its data is a failed command.
///
/// Both `list` verbs used to print the failure and return, so `run_group` saw
/// `Ok(())` and the tool exited 0. Under `--json` that was worse than a wrong
/// exit code: stdout was a zero-byte stream, which a consumer cannot tell from
/// "this session has no windows" except by parsing nothing at all.
#[test]
fn a_listing_of_a_session_that_does_not_exist_fails() {
    common::isolate_runtime_dir();
    for (group, verb) in [("window", "list"), ("process", "list")] {
        for extra in [vec![], vec!["--json".to_string()]] {
            let mut argv = vec![group.to_string(), verb.to_string()];
            argv.push("definitely-not-a-session".to_string());
            argv.extend(extra);
            assert_eq!(
                main_with_args("maverickctl", argv.clone()),
                ExitCode::FAILURE,
                "`maverickctl {argv:?}` must fail, not report an empty listing as success"
            );
        }
    }
}

/// A refusal that arrives as a successful transport is still a failure.
///
/// The line protocol returns errors in the reply body, prefixed `error `, so a
/// reply is a successful *exchange* carrying a failed *request*. The client has
/// to classify it, or a script gets the error text as data and a zero status.
///
/// The fixture is a real instance as far as the client is concerned: a socket
/// at the path discovery scans, plus the identity file it reads. `start_time` is
/// left at 0, which liveness treats as "do not check" — the fixture process
/// standing in for the WM is this test, not the pid recorded in the file.
mod reply_classification {
    use super::*;
    use maverick_sys::identity::InstanceInfo;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    /// Stand in for an instance: answer every request with `reply`, then close.
    ///
    /// One `sid` per test — the tests run concurrently and would otherwise race
    /// for the same socket and ficha.
    fn serve(sid: &str, reply: &'static [u8]) -> std::thread::JoinHandle<()> {
        let path = maverick_sys::identity::sock_path(sid);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture session dir");
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fixture socket");
        maverick_sys::identity::write_meta(&InstanceInfo {
            name: sid.to_string(),
            session_id: sid.to_string(),
            pid: std::process::id(),
            display: String::new(),
            tty_nr: 0,
            x_server_identity: String::new(),
            start_time: 0,
            exe: String::new(),
            started_at: 0,
            alive: true,
        })
        .expect("write fixture ficha");
        std::thread::spawn(move || {
            // A generous budget, not exactly one or two. Every test shares one
            // runtime directory, so resolving any one fixture makes the client
            // ping *all* of them before it queries its own. A fixture that ran
            // out of replies would fail with "connection refused" and pass for
            // the wrong reason, so this answers far more than it needs.
            for _ in 0..64 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
                let mut stream = stream;
                let _ = stream.write_all(reply);
                let _ = stream.flush();
            }
        })
    }

    /// A fixture that accepts and then closes without writing.
    fn serve_silent(sid: &str) -> std::thread::JoinHandle<()> {
        let path = maverick_sys::identity::sock_path(sid);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture session dir");
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fixture socket");
        std::thread::spawn(move || {
            for _ in 0..64 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
            }
        })
    }

    fn query(sid: &str) -> ExitCode {
        main_with_args(
            "maverickctl",
            vec![
                "--name".to_string(),
                sid.to_string(),
                "query".to_string(),
                "state".to_string(),
            ],
        )
    }

    #[test]
    fn an_error_reply_is_a_failed_command() {
        common::isolate_runtime_dir();
        let _server = serve("errreply", b"error unknown-query: state\n");
        assert_eq!(query("errreply"), ExitCode::FAILURE);
    }

    #[test]
    fn a_json_reply_is_a_successful_command() {
        common::isolate_runtime_dir();
        let _server = serve("jsonreply", b"{\"sel_mon\":0}\n");
        assert_eq!(query("jsonreply"), ExitCode::SUCCESS);
    }

    /// The protocol owes exactly one line per request, so a peer that closes
    /// without writing has failed to answer. Reporting that as success gave a
    /// bare newline on stdout, nothing on stderr, and exit 0.
    #[test]
    fn a_silent_peer_is_a_failed_command() {
        common::isolate_runtime_dir();
        let _server = serve_silent("silentpeer");
        assert_eq!(query("silentpeer"), ExitCode::FAILURE);
    }

    #[test]
    fn a_refused_socket_is_a_failed_command() {
        common::isolate_runtime_dir();
        let path = maverick_sys::identity::sock_path("refusedsock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture session dir");
        }
        let _ = std::fs::remove_file(&path);
        maverick_sys::identity::write_meta(&InstanceInfo {
            name: "refusedsock".to_string(),
            session_id: "refusedsock".to_string(),
            pid: std::process::id(),
            display: String::new(),
            tty_nr: 0,
            x_server_identity: String::new(),
            start_time: 0,
            exe: String::new(),
            started_at: 0,
            alive: true,
        })
        .expect("write fixture ficha");
        // No listener at all: connect fails.
        assert_eq!(query("refusedsock"), ExitCode::FAILURE);
    }
}
