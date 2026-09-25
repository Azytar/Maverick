//! Unix-socket control channel for Maverick.
//!
//! The WM opens a [`UnixListener`] at [`crate::identity::sock_path`] and answers a
//! small line-based text protocol:
//! ```text
//!   ping                 -> pong <name>
//!   identify             -> JSON ficha (so a tool can tell TTYs/DISPLAYs apart)
//!   state                -> latest WM state snapshot (JSON)
//!   dispatch <action>    -> enqueue an action; replies "ok"
//!   quit                 -> enqueue quit; replies "ok", then disconnects
//!   restart              -> enqueue restart; replies "ok"
//!   reload               -> enqueue config reload; replies "ok"
//!   subscribe            -> stream event lines until the client disconnects
//! ```
//!
//! # Thread model
//!
//! [`ControlServer::spawn`] binds the socket and spawns a **background accept
//! thread** (non-blocking `UnixListener` + 50 ms poll). Each accepted
//! connection is handed to its own **per-connection thread** that blocks on
//! `BufReader::read_line` with a 500 ms read timeout. `subscribe` hijacks its
//! connection thread into [`stream_events`], which blocks on the hub
//! subscription for as long as the client stays connected.
//!
//! The server never touches WM state directly: it talks to a [`crate::hub::ControlHub`]
//! that queues [`crate::hub::ControlCommand`]s for the WM thread and caches the
//! state snapshot / event stream. The WM drains commands once per event-loop
//! iteration.
//!
//! Back-pressure is enforced via an `AtomicUsize` counter capped at 32 concurrent
//! handlers; excess accepts sleep 100 ms before retrying. Because a subscriber
//! occupies its handler slot until it disconnects, `subscribe` additionally
//! admits at most [`MAX_SUBSCRIBERS`] streams and rejects the rest, keeping
//! slots free for short commands.
//!
//! # Ownership and lifecycle
//!
//! [`ControlServer`] owns the socket path (`name`) and a shared `stop` flag
//! (`Arc<AtomicBool>`). [`ControlServer::shutdown`] sets the flag and unlinks
//! the socket; [`Drop`] calls `shutdown` so dropping the handle always cleans
//! up. The accept thread exits on the next poll after `stop` is set. Per-
//! connection threads exit on EOF, read error, or after `quit`/`subscribe`.
//!
//! # Invariants
//!
//! - The parent directory is created `0700` via [`crate::identity::set_private_dir`].
//! - A stale socket file is only unlinked if it is a socket (`FileTypeExt::is_socket`)
//!   to avoid TOCTOU symlink attacks.
//! - Commands containing `'\n'` are rejected in [`send_command`] to prevent
//!   line-protocol injection.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::hub::{ControlCommand, ControlHub};
use crate::identity::{
    self, InstanceInfo, DISPATCH_CMD, IDENTIFY_CMD, PING_CMD, QUERY_CMD, QUIT_CMD, RELOAD_CMD,
    RESTART_CMD, STATE_CMD, SUBSCRIBE_CMD,
};

const ORD: Ordering = Ordering::SeqCst;
const READ_TIMEOUT: Duration = Duration::from_millis(500);
/// Maximum concurrent connection handler threads (short commands + subscribers).
const MAX_CONCURRENT: usize = 32;
/// Maximum concurrent `subscribe` streams, enforced atomically at registration
/// by [`crate::hub::ControlHub::try_subscribe`].
///
/// Subscribers hold their handler thread forever (by design — it's a stream),
/// so without a separate cap [`MAX_CONCURRENT`] subscribers would starve all
/// short commands (`ping`/`dispatch`/`quit`): the accept loop stops accepting
/// once every handler slot is taken. A `subscribe` beyond the cap is rejected
/// with `error subscribe: too many subscribers`, always leaving half the
/// handler slots free for commands. Same-UID local only, but a wedged bar must
/// not be able to wedge WM control.
pub const MAX_SUBSCRIBERS: usize = 16;
/// Maximum accepted protocol line (64 KiB). Prevents a single client from
/// OOMing the per-connection thread with a 1 GiB `read_line`.
pub const MAX_LINE_LEN: usize = 64 * 1024;
/// Maximum accepted command length for `send_command` (same bound).
pub const MAX_CMD_LEN: usize = 64 * 1024;

/// Handle to a running control server. Dropping it removes the socket file.
///
/// See the module documentation for the thread model, ownership split and
/// invariants.
pub struct ControlServer {
    name: String,
    stop: Arc<AtomicBool>,
}

impl ControlServer {
    /// Bind the socket for `name` and start serving on a background thread.
    ///
    /// `identity_json` is returned verbatim by `identify`. `hub` is the seam to
    /// the WM thread: `dispatch`/`quit`/`restart`/`reload` become
    /// `ControlCommand`s the WM drains, `state` reads the hub snapshot, and
    /// `subscribe` streams hub events.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if the session directory cannot be created or the
    /// socket cannot be bound.
    pub fn spawn(name: &str, identity_json: String, hub: ControlHub) -> std::io::Result<Self> {
        // Validate early: reject traversal/overlong session ids before touching fs.
        let path = identity::try_sock_path(name)?;
        // Ensure the per-session dir exists (bind won't create parent dirs) and
        // is private (0700) so other UIDs can't interfere.
        let dir = identity::try_session_dir(name)?;
        std::fs::create_dir_all(&dir)?;
        identity::set_private_dir(&dir)?;
        // Stale socket from a previous crashed instance: unlink so bind works.
        // TOCTOU-hardened: use `symlink_metadata` (does NOT follow symlinks).
        // A symlink — even one pointing at a socket — reports as `symlink`,
        // not `socket`, so we never unlink attacker-planted links; bind then
        // fails safely instead of deleting arbitrary files.
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            #[cfg(unix)]
            {
                if meta.file_type().is_socket() {
                    let _ = std::fs::remove_file(&path);
                }
                // Else: not a socket — leave it alone; bind will fail clearly.
            }
            #[cfg(not(unix))]
            {
                let _ = meta;
            }
        }
        let sock = UnixListener::bind(&path)?;

        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let srv_name = name.to_string();
        let srv_stop = stop.clone();
        let srv_active = active.clone();
        thread::spawn(move || {
            let _ = sock.set_nonblocking(true);
            loop {
                if srv_stop.load(ORD) {
                    break;
                }
                if srv_active.load(ORD) >= MAX_CONCURRENT {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                match sock.accept() {
                    Ok((stream, _)) => {
                        srv_active.fetch_add(1, ORD);
                        let name = srv_name.clone();
                        let ident = identity_json.clone();
                        let hub = hub.clone();
                        let stop = srv_stop.clone();
                        let act = srv_active.clone();
                        thread::spawn(move || {
                            handle_conn(stream, &name, &ident, &hub, &stop);
                            act.fetch_sub(1, ORD);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                }
            }
        });

        Ok(Self {
            name: name.to_string(),
            stop,
        })
    }

    /// Stop the server thread and unlink the socket.
    pub fn shutdown(&self) {
        self.stop.store(true, ORD);
        // Best-effort unlink; cleanup_meta also handles it.
        // Only unlink if it really is a socket (no symlink following).
        if let Ok(p) = identity::try_sock_path(&self.name) {
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                #[cfg(unix)]
                {
                    if m.file_type().is_socket() {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Handle a single client connection on its own thread. Reads line-delimited
/// commands, dispatches them via [`dispatch_line`], and hijacks the connection
/// for [`stream_events`] on `subscribe`.
fn handle_conn(
    stream: UnixStream,
    name: &str,
    identity_json: &str,
    hub: &ControlHub,
    stop: &Arc<AtomicBool>,
) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        // Bound memory: a hostile client sending a 1 GiB line without `\n`
        // would otherwise OOM this thread via unbounded `read_line` growth.
        if line.len() > MAX_LINE_LEN {
            let _ = writer.write_all(b"error line too long\n");
            break;
        }
        // Drop the line terminator, tolerating a telnet-style trailing `\r`.
        let cmd = line.trim_end_matches(['\n', '\r']).trim();
        if cmd.is_empty() {
            continue;
        }
        // Reject interior control chars that would break framing/logs.
        if cmd.contains(['\n', '\r']) {
            let _ = writer.write_all(b"error invalid command\n");
            continue;
        }

        // `subscribe` hijacks the connection into a streaming loop.
        if cmd == SUBSCRIBE_CMD {
            // Registering IS the capacity decision: a subscriber parks its
            // handler thread until it disconnects, so the count and the
            // registration share one critical section. Reading the count first
            // and registering afterwards would let concurrent connections each
            // pass a stale check and take more than the cap allows.
            match hub.try_subscribe(MAX_SUBSCRIBERS) {
                Some(rx) => {
                    let _ = writer.write_all(b"ok subscribe\n");
                    stream_events(&mut writer, rx, stop);
                }
                None => {
                    let _ = writer.write_all(b"error subscribe: too many subscribers\n");
                }
            }
            break;
        }

        let response = dispatch_line(cmd, name, identity_json, hub);
        if writer.write_all(response.as_bytes()).is_err() {
            break;
        }
        // Quit disconnects after acknowledging.
        if cmd == QUIT_CMD {
            break;
        }
    }
}

/// Turn a single request line into a response, enqueuing commands as needed.
fn dispatch_line(cmd: &str, name: &str, identity_json: &str, hub: &ControlHub) -> String {
    // `name` comes from `--name` (external input): sanitize for the
    // line protocol so `pong evil\ninject` cannot break framing.
    let safe_name: String = name.chars().filter(|c| !c.is_control()).take(128).collect();
    // All hub-published payloads must be single-line; a WM bug emitting
    // `\n` would otherwise desync `subscribe_stream`'s `lines()` framing.
    fn single_line(s: &str) -> String {
        let mut out: String = s.replace(['\n', '\r'], " ");
        if out.len() > MAX_LINE_LEN {
            out.truncate(MAX_LINE_LEN);
        }
        out
    }
    match cmd {
        PING_CMD => format!("pong {safe_name}\n"),
        IDENTIFY_CMD => format!("{}\n", single_line(identity_json)),
        STATE_CMD => format!("{}\n", single_line(&hub.snapshot())),
        QUIT_CMD => {
            if hub.push_command(ControlCommand::Quit) {
                "ok\n".to_string()
            } else {
                "error busy: command queue full\n".to_string()
            }
        }
        RESTART_CMD => {
            if hub.push_command(ControlCommand::Restart) {
                "ok\n".to_string()
            } else {
                "error busy: command queue full\n".to_string()
            }
        }
        RELOAD_CMD => {
            if hub.push_command(ControlCommand::Reload) {
                "ok\n".to_string()
            } else {
                "error busy: command queue full\n".to_string()
            }
        }
        tmp => {
            // `dispatch <action>` — require a whitespace delimiter so that
            // `dispatchfoo` is rejected as unknown-command instead of running `foo`.
            if let Some(rest) = tmp.strip_prefix(DISPATCH_CMD) {
                if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
                    let action = rest.trim();
                    if action.is_empty() {
                        return "error dispatch: missing action\n".to_string();
                    }
                    if hub.push_command(ControlCommand::Dispatch(action.to_string())) {
                        return "ok\n".to_string();
                    }
                    return "error busy: command queue full\n".to_string();
                }
            }
            // `query <topic>` — same delimiter requirement.
            if let Some(rest) = tmp.strip_prefix(QUERY_CMD) {
                if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
                    let topic = rest.trim();
                    if topic.is_empty() {
                        return "error query: missing topic\n".to_string();
                    }
                    // The WM thread computes the reply from live state (it is the
                    // only thread allowed to touch it); we block on its answer.
                    // 2s is generous: the WM loop wakes at least every 100ms.
                    // Bounded one-shot: the WM sends exactly once, so this
                    // never grows and never blocks the WM thread.
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);
                    if !hub.push_command(ControlCommand::Query {
                        topic: topic.to_string(),
                        reply: tx,
                    }) {
                        return "error query: WM not accepting commands\n".to_string();
                    }
                    return match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                        Ok(json) => format!("{}\n", single_line(&json)),
                        Err(_) => "error query: timed out\n".to_string(),
                    };
                }
            }
            // Never echo raw input: it enables log/terminal injection and
            // unbounded replies. Truncate + strip control chars.
            let safe: String = tmp.chars().filter(|c| !c.is_control()).take(64).collect();
            format!("error unknown-command: {safe}\n")
        }
    }
}

/// Stream hub events to a subscribed client until it disconnects or the server
/// stops. Blocks on this connection's thread only.
/// Enforces single-line framing and a write timeout so one slow client
/// cannot wedge its thread forever.
fn stream_events(
    writer: &mut UnixStream,
    rx: std::sync::mpsc::Receiver<String>,
    stop: &Arc<AtomicBool>,
) {
    let _ = writer.set_write_timeout(Some(Duration::from_secs(2)));
    loop {
        if stop.load(ORD) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                let clean = line.replace(['\n', '\r'], " ");
                let mut out = clean;
                if out.len() > MAX_LINE_LEN {
                    out.truncate(MAX_LINE_LEN);
                }
                if writer.write_all(out.as_bytes()).is_err() || writer.write_all(b"\n").is_err() {
                    break;
                }
                let _ = writer.flush();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Periodic wake so we can notice `stop` / a dead socket.
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

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
    Ok(reply.trim_end_matches(['\n', '\r']).to_string())
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
/// until the reply arrives. Used by `maverick-msg query …`.
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

/// Convenience: build the identity JSON for `info` (mirrors `identity::write_meta`).
pub fn identity_json(info: &InstanceInfo) -> String {
    use crate::json::json_quote;
    format!(
        r#"{{"name":{},"session_id":{},"pid":{},"display":{},"tty_nr":{},"x_server_identity":{},"start_time":{},"exe":{},"started_at":{},"alive":{}}}"#,
        json_quote(&info.name),
        json_quote(&info.session_id),
        info.pid,
        json_quote(&info.display),
        info.tty_nr,
        json_quote(&info.x_server_identity),
        info.start_time,
        json_quote(&info.exe),
        info.started_at,
        info.alive,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::ControlCommand;
    use crate::identity::InstanceInfo;

    #[test]
    fn server_full_protocol() {
        let name = "testctl";
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
        assert!(!identity::sock_path(name).exists());
    }

    #[test]
    fn subscribe_receives_events() {
        let name = "testsub";
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
    }

    // Simultaneous `subscribe` attempts per round of the cap-race test:
    // several times the cap, so many attempts are inside the registration
    // path at the same time.
    const RACE_ATTEMPTS: usize = MAX_CONCURRENT * 2;
    // Rounds of simultaneous attempts; the cap must hold in every one.
    const RACE_ROUNDS: usize = 8;

    #[test]
    fn subscribe_cap_holds_under_concurrent_registration() {
        for round in 0..RACE_ROUNDS {
            let hub = ControlHub::new();
            let barrier = Arc::new(std::sync::Barrier::new(RACE_ATTEMPTS));
            // A busy WM thread publishing events keeps the subscriber list hot,
            // which is when a stale capacity check is most likely to slip
            // through: connections arriving while events are being emitted.
            let emitting = Arc::new(AtomicBool::new(false));
            let emitter = {
                let hub = hub.clone();
                let emitting = emitting.clone();
                std::thread::spawn(move || {
                    while !emitting.load(ORD) {
                        hub.emit("{\"event\":\"focus\"}");
                    }
                })
            };
            let mut attempts = Vec::with_capacity(RACE_ATTEMPTS);
            for _ in 0..RACE_ATTEMPTS {
                let hub = hub.clone();
                let barrier = barrier.clone();
                attempts.push(std::thread::spawn(move || {
                    // Release every attempt at once: attempts arriving one
                    // after the other would never observe a stale count.
                    barrier.wait();
                    hub.try_subscribe(MAX_SUBSCRIBERS)
                }));
            }
            // Receivers are held for the whole round, so nothing is pruned and
            // the count reflects every admitted sink.
            let admitted: Vec<_> = attempts
                .into_iter()
                .filter_map(|a| a.join().expect("attempt thread"))
                .collect();
            assert_eq!(
                admitted.len(),
                MAX_SUBSCRIBERS,
                "round {round}: exactly the cap must be admitted"
            );
            assert_eq!(
                hub.subscriber_count(),
                MAX_SUBSCRIBERS,
                "round {round}: registered sinks must never exceed the cap"
            );
            emitting.store(true, ORD);
            emitter.join().expect("emitter thread");
        }
    }

    #[test]
    fn subscribe_cap_rejects_extras_from_concurrent_connections() {
        let name = "testsubrace";
        let hub = ControlHub::new();
        let server = ControlServer::spawn(name, "{}\n".into(), hub.clone()).expect("server binds");

        // Connect every client first so the handler threads are all parked in
        // `read_line` when the commands are released.
        let barrier = Arc::new(std::sync::Barrier::new(MAX_CONCURRENT));
        let mut clients = Vec::with_capacity(MAX_CONCURRENT);
        for _ in 0..MAX_CONCURRENT {
            let s = UnixStream::connect(identity::try_sock_path(name).expect("sock path"))
                .expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            s.set_write_timeout(Some(Duration::from_secs(5))).ok();
            clients.push((s, barrier.clone()));
        }
        let mut replies = Vec::with_capacity(MAX_CONCURRENT);
        let mut attempts = Vec::with_capacity(MAX_CONCURRENT);
        for (s, barrier) in clients {
            attempts.push(std::thread::spawn(move || {
                let mut s = s;
                barrier.wait();
                s.write_all(b"subscribe\n").expect("write subscribe");
                let mut first = String::new();
                BufReader::new(&s)
                    .read_line(&mut first)
                    .expect("first reply line");
                (s, first.trim_end_matches('\n').to_string())
            }));
        }
        for attempt in attempts {
            replies.push(attempt.join().expect("client thread"));
        }

        let ok = replies
            .iter()
            .filter(|(_, line)| line == "ok subscribe")
            .count();
        let refused = replies
            .iter()
            .filter(|(_, line)| line == "error subscribe: too many subscribers")
            .count();
        assert_eq!(ok, MAX_SUBSCRIBERS, "only the cap may be admitted");
        assert_eq!(
            ok + refused,
            MAX_CONCURRENT,
            "every over-cap subscribe must be refused, not left hanging"
        );
        assert_eq!(hub.subscriber_count(), MAX_SUBSCRIBERS);

        // The subscribers must not have eaten every handler slot: short
        // commands still answer.
        assert!(ping(name).is_ok(), "commands must survive full subs");

        server.shutdown();
        drop(replies);
    }

    #[test]
    fn subscribe_cap_rejects_beyond_max() {
        let name = "testsubcap";
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
        for h in handles {
            h.join().unwrap();
        }
    }
}
