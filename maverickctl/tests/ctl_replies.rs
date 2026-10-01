//! How `maverickctl` classifies a control-protocol reply.
//!
//! A reply is a successful *exchange* carrying either data or a refusal, and
//! the client has to tell them apart. These tests need a real socket to talk
//! to, so they stand up a fixture instance rather than a live window manager.
//!
//! This is a separate test binary on purpose. The fixtures publish an
//! identity file in the runtime directory, and `ctl_props` asserts that a word
//! the tool does not know fails — a claim that only holds when no discoverable
//! instance is present. Sharing a runtime directory between the two made that
//! assertion depend on whether a fixture happened to be up.

use maverickctl::ctl::main_with_args;
use std::process::ExitCode;

mod instance;
mod runtime_dir;

use instance::{Instance, Published};

/// A private runtime directory, so nothing here is visible to another test
/// binary's fixtures and no live instance can be discovered.
fn isolate_runtime_dir() {
    runtime_dir::isolate("maverick-ctl-replies");
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
///
/// Stand in for an instance: answer every request with `reply`, then close.
///
/// One `sid` per test — the tests run concurrently and would otherwise race
/// for the same socket and ficha.
///
/// The reply budget is a generous 64, not one or two. Every test shares one
/// runtime directory, so resolving any one fixture makes the client ping *all*
/// of them before it queries its own. A fixture that ran out of replies would
/// fail with "connection refused" and pass for the wrong reason, so this answers
/// far more than it needs.
fn serve(sid: &str, reply: &'static str) -> Instance {
    Instance::serve(sid, Published::Instance, Some(64), move |_| {
        Some(reply.to_string())
    })
}

/// A fixture that accepts and then closes without writing, publishing no record.
///
/// No record, so the tool fails at resolution rather than at the protocol. See
/// the test that uses it.
fn serve_silent(sid: &str) -> Instance {
    Instance::serve(sid, Published::SocketOnly, Some(64), |_| None)
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
    isolate_runtime_dir();
    let _server = serve("errreply", "error unknown-query: state\n");
    assert_eq!(query("errreply"), ExitCode::FAILURE);
}

#[test]
fn a_json_reply_is_a_successful_command() {
    isolate_runtime_dir();
    let _server = serve("jsonreply", "{\"sel_mon\":0}\n");
    assert_eq!(query("jsonreply"), ExitCode::SUCCESS);
}

/// A peer that cannot be addressed as an instance is a failed command.
///
/// The protocol owes exactly one line per request, so a peer that closes
/// without writing has failed to answer, and reporting that as success gave a
/// bare newline on stdout, nothing on stderr, and exit 0.
///
/// What this fixture actually exercises is the step *before* the protocol: it
/// publishes a socket and no record, so the tool never resolves an instance to
/// ask. The silent-peer branch of the protocol — reachable, but answering
/// nothing — is not covered here, and `a_refused_socket_is_a_failed_command`
/// covers the mirror case of a record with no socket.
#[test]
fn a_silent_peer_is_a_failed_command() {
    isolate_runtime_dir();
    let _server = serve_silent("silentpeer");
    assert_eq!(query("silentpeer"), ExitCode::FAILURE);
}

#[test]
fn a_refused_socket_is_a_failed_command() {
    isolate_runtime_dir();
    // A record and no listener: discovery resolves the instance and the connect
    // is refused. A socket with no record would fail earlier, at resolution.
    let _server = Instance::serve("refusedsock", Published::RecordOnly, None, |_| None);
    assert_eq!(query("refusedsock"), ExitCode::FAILURE);
}
