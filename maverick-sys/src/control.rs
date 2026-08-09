// maverick-sys/src/control.rs
// Unix-socket control channel for Maverick.
//
// The WM opens a `UnixListener` at `identity::sock_path(name)` and answers a
// small line-based text protocol:
//   ping                 -> pong <name>
//   identify             -> JSON ficha (so a tool can tell TTYs/DISPLAYs apart)
//   state                -> latest WM state snapshot (JSON)
//   dispatch <action>    -> enqueue an action; replies "ok"
//   quit                 -> enqueue quit; replies "ok", then disconnects
//   restart              -> enqueue restart; replies "ok"
//   reload               -> enqueue config reload; replies "ok"
//   subscribe            -> stream event lines until the client disconnects
//
// The server runs on its own thread so it never blocks the X11 event loop.
// It never touches WM state directly: it talks to a `ControlHub` that queues
// commands for the WM thread and caches the state snapshot / event stream.

use std::io::{BufRead, BufReader, Write};
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

/// Handle to a running control server. Dropping it removes the socket file.
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
    pub fn spawn(name: &str, identity_json: String, hub: ControlHub) -> std::io::Result<Self> {
        let path = identity::sock_path(name);
        // Ensure the runtime dir exists (bind won't create parent dirs).
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Stale socket from a previous crashed instance: unlink so bind works.
        // Defend against TOCTOU symlink attacks: only remove if it's a socket.
        if path.exists() {
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.file_type().is_socket() {
                    let _ = std::fs::remove_file(&path);
                } else {
                    // Not a socket — leave it alone; bind will fail with a clear error.
                }
            }
        }
        let sock = UnixListener::bind(&path)?;

        let stop = Arc::new(AtomicBool::new(false));
        // Limit concurrent connection handler threads to prevent resource exhaustion.
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        const MAX_CONCURRENT: usize = 32;

        let srv_name = name.to_string();
        let srv_stop = stop.clone();
        let srv_active = active.clone();
        thread::spawn(move || {
            let _ = sock.set_nonblocking(true);
            loop {
                if srv_stop.load(ORD) {
                    break;
                }
                // Back-pressure: don't accept if too many handlers are active.
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
        let _ = std::fs::remove_file(identity::sock_path(&self.name));
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

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
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let cmd = line.trim_end().trim();
        if cmd.is_empty() {
            continue;
        }

        // `subscribe` hijacks the connection into a streaming loop.
        if cmd == SUBSCRIBE_CMD {
            let _ = writer.write_all(b"ok subscribe\n");
            stream_events(&mut writer, hub, stop);
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
    match cmd {
        PING_CMD => format!("pong {name}\n"),
        IDENTIFY_CMD => format!("{identity_json}\n"),
        STATE_CMD => format!("{}\n", hub.snapshot()),
        QUIT_CMD => {
            hub.push_command(ControlCommand::Quit);
            "ok\n".to_string()
        }
        RESTART_CMD => {
            hub.push_command(ControlCommand::Restart);
            "ok\n".to_string()
        }
        RELOAD_CMD => {
            hub.push_command(ControlCommand::Reload);
            "ok\n".to_string()
        }
        tmp => {
            // `dispatch <action>`
            if let Some(action) = tmp.strip_prefix(DISPATCH_CMD) {
                let action = action.trim();
                if action.is_empty() {
                    return "error dispatch: missing action\n".to_string();
                }
                hub.push_command(ControlCommand::Dispatch(action.to_string()));
                return "ok\n".to_string();
            }
            // `query <topic>` — ask the WM to answer a structured query now.
            if let Some(topic) = tmp.strip_prefix(QUERY_CMD) {
                let topic = topic.trim();
                if topic.is_empty() {
                    return "error query: missing topic\n".to_string();
                }
                // The WM thread computes the reply from live state (it is the
                // only thread allowed to touch it); we block on its answer.
                // 2s is generous: the WM loop wakes at least every 100ms.
                let (tx, rx) = std::sync::mpsc::channel();
                if !hub.push_command(ControlCommand::Query {
                    topic: topic.to_string(),
                    reply: tx,
                }) {
                    return "error query: WM not accepting commands\n".to_string();
                }
                return match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                    Ok(json) => format!("{json}\n"),
                    Err(_) => "error query: timed out\n".to_string(),
                };
            }
            format!("error unknown-command: {tmp}\n")
        }
    }
}

/// Stream hub events to a subscribed client until it disconnects or the server
/// stops. Blocks on this connection's thread only.
fn stream_events(writer: &mut UnixStream, hub: &ControlHub, stop: &Arc<AtomicBool>) {
    let rx = hub.subscribe();
    loop {
        if stop.load(ORD) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if writer.write_all(line.as_bytes()).is_err() || writer.write_all(b"\n").is_err() {
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
    // Reject embedded newlines to prevent command injection in the line-based protocol.
    if cmd.contains('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command contains newline",
        ));
    }
    let path = identity::sock_path(name);
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.write_all(format!("{cmd}\n").as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
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
    send_command(name, &format!("{DISPATCH_CMD} {action}"))
}

/// Run a structured `query <topic>` against a running instance ("workspaces",
/// "tree", "focused", …). The WM answers from its live state; this blocks
/// until the reply arrives. Used by `maverick-msg query …`.
pub fn query(name: &str, topic: &str) -> std::io::Result<String> {
    send_command(name, &format!("{QUERY_CMD} {topic}"))
}

/// Subscribe to the event stream of a running instance, invoking `on_line` for
/// each event line as it arrives. Blocks until the socket closes or `on_line`
/// returns `false`. Used by `maverickctl subscribe` and external bars.
pub fn subscribe_stream<F>(name: &str, mut on_line: F) -> std::io::Result<()>
where
    F: FnMut(&str) -> bool,
{
    let path = identity::sock_path(name);
    let mut stream = UnixStream::connect(&path)?;
    stream.write_all(format!("{SUBSCRIBE_CMD}\n").as_bytes())?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim_end();
        // Skip the initial "ok subscribe" acknowledgement.
        if trimmed == "ok subscribe" {
            continue;
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
        r#"{{"name":{},"pid":{},"display":{},"tty_nr":{},"exe":{},"started_at":{},"alive":{}}}"#,
        json_quote(&info.name),
        info.pid,
        json_quote(&info.display),
        info.tty_nr,
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
            pid: std::process::id(),
            display: ":9".into(),
            tty_nr: 0x1234,
            exe: "/usr/bin/maverick".into(),
            started_at: 1,
            alive: true,
        };
        let json = identity_json(&info);
        let hub = ControlHub::new();
        hub.publish_state("{\"focus\":7}");
        let server = ControlServer::spawn(name, json, hub.clone()).expect("server binds");

        // ping
        let pong = ping(name).expect("ping");
        assert!(pong.starts_with("pong testctl"), "got: {pong}");

        // identify returns our json
        let ident = identify(name).expect("identify");
        assert!(ident.contains("\"display\":\":9\""), "got: {ident}");

        // state returns the published snapshot
        let st = state(name).expect("state");
        assert_eq!(st, "{\"focus\":7}");

        // dispatch enqueues a command for the WM thread
        assert_eq!(dispatch(name, "focus-left").expect("dispatch"), "ok");

        // quit enqueues a Quit and replies ok
        assert_eq!(quit(name).expect("quit"), "ok");

        // The WM thread would drain these; verify order/content here.
        // Give the connection threads a moment to enqueue.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let cmds = hub.drain_commands();
        assert!(cmds
            .iter()
            .any(|c| matches!(c, ControlCommand::Dispatch(a) if a == "focus-left")));
        assert!(cmds.iter().any(|c| matches!(c, ControlCommand::Quit)));

        // socket file is removed on shutdown
        server.shutdown();
        assert!(!identity::sock_path(name).exists());
    }

    #[test]
    fn subscribe_receives_events() {
        let name = "testsub";
        let info = InstanceInfo {
            name: name.into(),
            pid: std::process::id(),
            display: ":9".into(),
            tty_nr: 0,
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
                // Stop after the first event so the test terminates.
                false
            });
        });

        // Wait until the subscriber has registered, then emit.
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
}
