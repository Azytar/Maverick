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

use maverick_sys::identity::InstanceInfo;
use maverickctl::ctl::main_with_args;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::ExitCode;

/// A private runtime directory, so nothing here is visible to another test
/// binary's fixtures and no live instance can be discovered.
fn isolate_runtime_dir() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("maverick-ctl-replies-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
    });
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
    isolate_runtime_dir();
    let _server = serve("errreply", b"error unknown-query: state\n");
    assert_eq!(query("errreply"), ExitCode::FAILURE);
}

#[test]
fn a_json_reply_is_a_successful_command() {
    isolate_runtime_dir();
    let _server = serve("jsonreply", b"{\"sel_mon\":0}\n");
    assert_eq!(query("jsonreply"), ExitCode::SUCCESS);
}

/// The protocol owes exactly one line per request, so a peer that closes
/// without writing has failed to answer. Reporting that as success gave a
/// bare newline on stdout, nothing on stderr, and exit 0.
#[test]
fn a_silent_peer_is_a_failed_command() {
    isolate_runtime_dir();
    let _server = serve_silent("silentpeer");
    assert_eq!(query("silentpeer"), ExitCode::FAILURE);
}

#[test]
fn a_refused_socket_is_a_failed_command() {
    isolate_runtime_dir();
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
