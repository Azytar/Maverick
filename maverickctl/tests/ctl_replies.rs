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

/// A `SocketOnly` fixture publishes no record, so discovery cannot observe one
/// at all — there is nothing for it to read, and inventing a record to make it
/// observable would change the very contract under test. The socket is the whole
/// of what was published, so the socket is the only thing its ownership can be
/// judged on.
///
/// And it is the only resource whose loss is silent. A handle that unlinked the
/// wrong session's socket would leave a worker still answering on a path nobody
/// can name, which breaks no assertion anywhere: until this test, `silentpeer`
/// was the only `SocketOnly` fixture in the suite, so there was no foreign socket
/// for a bad cleanup to destroy, and deleting that fixture's socket outright went
/// unnoticed too.
#[test]
fn dropping_a_socket_only_fixture_leaves_the_other_answering() {
    isolate_runtime_dir();
    let dropped = serve_silent("socketonly-dropped");
    let kept = Instance::serve("socketonly-kept", Published::SocketOnly, Some(64), |_| {
        Some("pong socketonly-kept\n".to_string())
    });
    let dropped_sock = maverick_sys::identity::sock_path("socketonly-dropped");
    let kept_sock = maverick_sys::identity::sock_path("socketonly-kept");
    assert!(
        dropped_sock.exists() && kept_sock.exists(),
        "both fixtures publish a socket"
    );

    drop(dropped);

    assert!(
        !dropped_sock.exists(),
        "retiring a fixture must unpublish it"
    );
    assert!(
        kept_sock.exists(),
        "retiring one fixture must not unlink another's socket: its worker would \
         go on answering on a path nothing can name"
    );
    assert_eq!(
        maverickctl::client::send_command("socketonly-kept", "ping").ok(),
        Some("pong socketonly-kept".to_string()),
        "the surviving fixture is still serving its own replies"
    );
    drop(kept);
}

/// The same boundary from the other side: which member of the pair is retired
/// must not decide whether the other one survives.
#[test]
fn the_socket_only_ownership_boundary_holds_in_either_direction() {
    isolate_runtime_dir();
    let silent = serve_silent("either-silent");
    let answering = Instance::serve("either-answering", Published::SocketOnly, Some(64), |_| {
        Some("pong either-answering\n".to_string())
    });
    drop(answering);

    assert!(!maverick_sys::identity::sock_path("either-answering").exists());
    assert!(
        maverick_sys::identity::sock_path("either-silent").exists(),
        "the silent fixture's socket belongs to it, not to the one just retired"
    );
    // A silent peer proves it is alive by accepting the connection and writing
    // nothing, which the client reports as a zero-byte read. A socket that had
    // been unlinked out from under it would instead fail to connect at all —
    // so the two are distinguishable, and this is the assertion that can tell
    // "still serving, still mute" from "gone".
    assert_eq!(
        maverickctl::client::send_command("either-silent", "ping")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof,
        "the silent fixture is still accepting connections"
    );
    drop(silent);
}

/// The same boundary on the branch that has a record.
///
/// `SocketOnly` is only the half of the teardown that unlinks a socket by hand;
/// `Instance` and `RecordOnly` go through `identity::cleanup_meta`. That branch
/// was never probed against a neighbour either: a cleanup over-broad enough to
/// sweep a second fixture's whole session survived every test in this binary and
/// in `ctl_refusals`, and the only reason anything noticed at all was a race
/// between concurrent tests. A published instance is the easier one to prove
/// wrong, because both discovery and `ping` can see it.
#[test]
fn dropping_a_published_instance_leaves_the_other_serving() {
    isolate_runtime_dir();
    let dropped = serve("isolation-dropped", "error gone\n");
    let kept = serve("isolation-kept", "pong isolation-kept\n");
    assert!(maverickctl::discover::find_by_name("isolation-dropped").is_some());
    assert!(maverickctl::discover::find_by_name("isolation-kept").is_some());

    drop(dropped);

    assert!(maverickctl::discover::find_by_name("isolation-dropped").is_none());
    assert!(
        maverickctl::discover::find_by_name("isolation-kept").is_some(),
        "retiring one fixture must not remove another's identity record: \
         it would erase a session that is still being served"
    );
    assert_eq!(
        maverickctl::client::ping("isolation-kept").unwrap_or_default(),
        "pong isolation-kept",
        "the surviving fixture is still answering with its own reply"
    );
    drop(kept);
    assert!(
        maverickctl::discover::find_by_name("isolation-kept").is_none(),
        "and it is unpublished once its own owner is gone"
    );
}
