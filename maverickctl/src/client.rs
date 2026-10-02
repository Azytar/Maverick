//! Control-socket client: how `maverickctl` (and only it) talks to a running instance.
//!
//! The window manager never calls anything in this module: it owns the server
//! half (`maverick_sys::control`) and only ever reads its own socket. Every
//! function here opens a fresh `UnixStream` to another process's
//! `control.sock`, so this code lives in the tool crate rather than in the
//! shared IPC surface. Moved verbatim from `maverick-sys::control`; the only
//! edits are the `maverick_sys::` paths below.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use maverick_sys::control::{MAX_CMD_LEN, MAX_LINE_LEN};
use maverick_sys::identity::{
    self, DISPATCH_CMD, IDENTIFY_CMD, PING_CMD, QUERY_CMD, QUIT_CMD, RELOAD_CMD, RESTART_CMD,
    STATE_CMD, SUBSCRIBE_CMD,
};

/// Connect to a running instance's control socket and send one command,
/// returning the first reply line. Used by discovery/ctl tools.
pub fn send_command(name: &str, cmd: &str) -> std::io::Result<String> {
    // Reject embedded CR/LF to prevent command injection in the line protocol,
    // and bound length to avoid amplifying a huge caller string.
    if cmd.contains(['\n', '\r']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command contains newline",
        ));
    }
    if cmd.len() > MAX_CMD_LEN {
        return Err(std::io::Error::new(
            io::ErrorKind::InvalidInput,
            "command too long",
        ));
    }
    // Validate session id before touching the filesystem (traversal-safe).
    let path = identity::try_sock_path(name)?;
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream.write_all(format!("{cmd}\n").as_bytes())?;
    let reader = BufReader::new(stream);
    let mut reply = String::new();
    // Bound the reply as well: a compromised server must not OOM the client.
    let mut limited = reader.take((MAX_LINE_LEN + 16) as u64);
    limited.read_line(&mut reply)?;
    if reply.len() > MAX_LINE_LEN + 16 {
        return Err(std::io::Error::new(
            io::ErrorKind::InvalidData,
            "reply too long",
        ));
    }
    let reply = reply.trim_end_matches(['\n', '\r']).to_string();
    // The protocol owes exactly one line per request: every arm of
    // `dispatch_line` returns one, and the server writes it before closing. The
    // shortest real reply is `ok`, so a zero-byte read means the peer went away
    // mid-exchange — not an empty answer. Reporting that as `Ok("")` made a
    // silent exit 0 with a bare newline on stdout and nothing on stderr, which
    // is worse than a refused socket: that already fails.
    if reply.is_empty() {
        return Err(std::io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "no reply from the instance",
        ));
    }
    Ok(reply)
}

/// Probe a running instance: connect, `ping`, and confirm it answers.
/// Returns the `pong` reply (e.g. `pong default`) or an error if dead.
pub fn ping(name: &str) -> std::io::Result<String> {
    send_command(name, PING_CMD)
}

/// Ask a running instance for its identity ficha JSON.
pub fn identify(name: &str) -> std::io::Result<String> {
    send_command(name, IDENTIFY_CMD)
}

/// Ask a running instance to quit. Returns Ok if the socket answered.
pub fn quit(name: &str) -> std::io::Result<String> {
    send_command(name, QUIT_CMD)
}

/// Ask a running instance to restart (re-exec).
pub fn restart(name: &str) -> std::io::Result<String> {
    send_command(name, RESTART_CMD)
}

/// Ask a running instance to reload its config.
pub fn reload(name: &str) -> std::io::Result<String> {
    send_command(name, RELOAD_CMD)
}

/// Fetch the current WM state snapshot (JSON) from a running instance.
pub fn state(name: &str) -> std::io::Result<String> {
    send_command(name, STATE_CMD)
}

/// Send a `dispatch <action>` to a running instance (execute an action as if
/// it were a keybind). Returns the server reply.
///
/// `Ok("ok")` is a **receipt**, not a result: the instance acknowledges a
/// dispatch the moment it is queued, and the action grammar is applied later, on
/// the window manager's own thread. An action that grammar does not recognise is
/// refused there in the window manager's log, which this process cannot read —
/// so the caller learns that the request was accepted and nothing more. What the
/// transport can refuse — a malformed line, a full command queue — arrives as an
/// `error …` body inside an `Ok`, and the caller has to classify it.
pub fn dispatch(name: &str, action: &str) -> std::io::Result<String> {
    if action.contains(['\n', '\r']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "action contains newline",
        ));
    }
    send_command(name, &format!("{DISPATCH_CMD} {action}"))
}

/// Run a structured `query <topic>` against a running instance ("workspaces",
/// "tree", "focused", …). The WM answers from its live state; this blocks
/// until the reply arrives. Used by `maverickctl query …`.
pub fn query(name: &str, topic: &str) -> std::io::Result<String> {
    if topic.contains(['\n', '\r']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "topic contains newline",
        ));
    }
    send_command(name, &format!("{QUERY_CMD} {topic}"))
}

/// Subscribe to the event stream of a running instance, invoking `on_line` for
/// each event line as it arrives. Blocks until the socket closes or `on_line`
/// returns `false`. Used by `maverickctl subscribe` and external bars.
pub fn subscribe_stream<F>(name: &str, mut on_line: F) -> std::io::Result<()>
where
    F: FnMut(&str) -> bool,
{
    let path = identity::try_sock_path(name)?;
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream.write_all(format!("{SUBSCRIBE_CMD}\n").as_bytes())?;
    let reader = BufReader::new(stream);
    let mut acked = false;
    for line in reader.lines() {
        let line = line?;
        if line.len() > MAX_LINE_LEN {
            continue;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        // Skip the initial "ok subscribe" acknowledgement. A server-side
        // rejection (e.g. subscriber cap) arrives here instead: surface it
        // as an error rather than feeding it to `on_line` as an event.
        if !acked {
            acked = true;
            if trimmed == "ok subscribe" {
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("subscribe rejected: {trimmed}"),
            ));
        }
        if !on_line(trimmed) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maverick_sys::control::{identity_json, ControlServer, MAX_SUBSCRIBERS};
    use maverick_sys::hub::{ControlCommand, ControlHub};
    use maverick_sys::identity::{self, InstanceInfo};

    #[test]
    fn subscribe_receives_events() {
        let name = "testsub";
        crate::test_support::runtime_root();
        let info = InstanceInfo {
            name: name.into(),
            session_id: name.into(),
            pid: std::process::id(),
            display: ":9".into(),
            tty_nr: 0,
            x_server_identity: "?".into(),
            start_time: 0,
            exe: String::new(),
            started_at: 1,
            alive: true,
        };
        let hub = ControlHub::new();
        let server =
            ControlServer::spawn(name, identity_json(&info), hub.clone()).expect("server binds");

        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let got_c = got.clone();
        let nm = name.to_string();
        let handle = std::thread::spawn(move || {
            let _ = subscribe_stream(&nm, |line| {
                got_c.lock().unwrap().push(line.to_string());
                false
            });
        });

        // Poll for subscriber registration before emitting, otherwise the
        // event is published to an empty sink list.
        for _ in 0..50 {
            if hub.subscriber_count() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        hub.emit("{\"event\":\"focus\",\"win\":5}");
        handle.join().unwrap();

        let events = got.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"focus\""));

        server.shutdown();
        crate::test_support::retire(name);
    }

    #[test]
    fn subscribe_cap_rejects_beyond_max() {
        let name = "testsubcap";
        crate::test_support::runtime_root();
        let info = InstanceInfo {
            name: name.into(),
            session_id: name.into(),
            pid: std::process::id(),
            display: ":9".into(),
            tty_nr: 0,
            x_server_identity: "?".into(),
            start_time: 0,
            exe: String::new(),
            started_at: 1,
            alive: true,
        };
        let hub = ControlHub::new();
        let server =
            ControlServer::spawn(name, identity_json(&info), hub.clone()).expect("server binds");

        let mut handles = Vec::new();
        for _ in 0..MAX_SUBSCRIBERS {
            let nm = name.to_string();
            handles.push(std::thread::spawn(move || {
                let _ = subscribe_stream(&nm, |_| true);
            }));
        }
        for _ in 0..250 {
            if hub.subscriber_count() >= MAX_SUBSCRIBERS {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(hub.subscriber_count(), MAX_SUBSCRIBERS);

        // The next subscribe must be rejected (as an Err, not an event line),
        // and short commands must still work on the remaining slots.
        let err = subscribe_stream(name, |_| true).expect_err("cap must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
        assert!(ping(name).is_ok(), "commands must survive full subs");

        server.shutdown();
        crate::test_support::retire(name);
        for h in handles {
            h.join().unwrap();
        }
    }
    #[test]
    fn server_full_protocol() {
        let name = "testctl";
        crate::test_support::runtime_root();
        let info = InstanceInfo {
            name: name.into(),
            session_id: name.into(),
            pid: std::process::id(),
            display: ":9".into(),
            tty_nr: 0x1234,
            x_server_identity: "?".into(),
            start_time: 0,
            exe: "/usr/bin/maverick".into(),
            started_at: 1,
            alive: true,
        };
        let json = identity_json(&info);
        let hub = ControlHub::new();
        hub.publish_state("{\"focus\":7}");
        let server = ControlServer::spawn(name, json, hub.clone()).expect("server binds");

        let pong = ping(name).expect("ping");
        assert!(pong.starts_with("pong testctl"), "got: {pong}");

        let ident = identify(name).expect("identify");
        assert!(ident.contains("\"display\":\":9\""), "got: {ident}");

        let st = state(name).expect("state");
        assert_eq!(st, "{\"focus\":7}");

        assert_eq!(dispatch(name, "focus-left").expect("dispatch"), "ok");

        assert_eq!(quit(name).expect("quit"), "ok");

        // The WM thread would drain these; verify order/content here.
        // Give the connection threads a moment to enqueue.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let cmds = hub.drain_commands();
        assert!(cmds
            .iter()
            .any(|c| matches!(c, ControlCommand::Dispatch(a) if a == "focus-left")));
        assert!(cmds.iter().any(|c| matches!(c, ControlCommand::Quit)));

        server.shutdown();
        crate::test_support::retire(name);
        assert!(!identity::sock_path(name).exists());
    }

    /// A peer that accepts the connection and then says nothing must not be able
    /// to hold the client forever.
    ///
    /// `send_command` bounds its own read, so the wait ends there rather than in
    /// the peer. That bound is what keeps every caller finite: discovery pings
    /// every instance it finds, so a peer that never answers would otherwise put
    /// an unbounded wait in front of an unrelated lookup.
    ///
    /// The peer here never replies and never closes. It holds the connection
    /// open until the test releases it, so the only thing that can end the wait
    /// is the client's own timeout — EOF is deliberately not available as an
    /// explanation, which is what separates this from a peer that answers with
    /// nothing and hangs up.
    #[test]
    fn a_peer_that_never_answers_is_bounded_by_the_client_timeout() {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let name = "testsilent";
        crate::test_support::runtime_root();
        let path = identity::sock_path(name);
        // Same directory setup `ControlServer::spawn` performs: private (0700)
        // so this fixture does not leave a world-readable session directory in
        // the real runtime dir.
        let dir = identity::try_session_dir(name).expect("valid fixture name");
        std::fs::create_dir_all(&dir).expect("session dir");
        identity::set_private_dir(&dir).expect("private session dir");
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path).expect("bind a socket to answer");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let accepted = std::sync::Arc::new(AtomicUsize::new(0));
        let accepted_c = std::sync::Arc::clone(&accepted);
        let peer = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the client connects");
            accepted_c.fetch_add(1, Ordering::SeqCst);
            let mut request = String::new();
            let _ = BufReader::new(&stream).read_line(&mut request);
            // Parked until the test has watched the client give up. The reply is
            // never written and the connection is never closed while the client
            // is waiting, so the client cannot be released by the peer.
            let _ = release_rx.recv();
            drop(stream);
        });

        // The command runs off-thread so that losing the timeout fails with a
        // diagnosis instead of hanging the test binary forever.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let target = name.to_string();
        let caller = std::thread::spawn(move || {
            let _ = done_tx.send(send_command(&target, PING_CMD));
        });
        let reply = done_rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| {
                panic!("send_command gave up on nothing: a peer that never replies must be bounded")
            });
        caller.join().expect("the client thread returns");

        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "the peer must have taken the connection, or the failure below would be the connect"
        );
        let err = reply.expect_err("a peer that answers nothing is an error, not an empty reply");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::WouldBlock,
            "only the read timeout can end this wait; a peer that hung up would be UnexpectedEof"
        );

        release_tx.send(()).expect("release the peer");
        peer.join().expect("the peer thread returns");
        let _ = std::fs::remove_file(&path);
        crate::test_support::retire(name);
    }
}
