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
//! handlers; excess accepts sleep 100 ms before retrying. A handler holds its
//! slot until its thread finishes, including when the handler unwinds, so a
//! panicking connection cannot permanently consume capacity. Because a
//! subscriber occupies its handler slot until it disconnects, `subscribe`
//! additionally admits at most [`MAX_SUBSCRIBERS`] streams and rejects the
//! rest, keeping slots free for short commands.
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
//! - Every reply and event is cut back to at most [`MAX_LINE_LEN`] bytes on a
//!   character boundary ([`single_line`]), so a hub payload can never make a
//!   handler unwind mid-write.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// The uid of the process on the other end of a connected Unix socket.
///
/// `SO_PEERCRED` reports the credentials the kernel recorded *at connect time*,
/// which is the only part of a peer's identity that cannot be asserted by the
/// peer itself. That is what makes it usable as an authorization decision
/// rather than as a hint: there is no way for a process to connect and claim to
/// be somebody else.
///
/// # Errors
///
/// Returns an error if the credentials cannot be read, which callers must treat
/// as "not authorised" — an unverifiable peer is not an authorised one.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` is a correctly sized, writable `ucred` and `len` says so.
    // `getsockopt` writes at most `len` bytes into it. The descriptor is a
    // live `AF_UNIX` socket, which is the one family `SO_PEERCRED` answers for.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // A short answer is not an answer: `pid` is checked because a zero pid is
    // what an uninitialised buffer looks like, and reading one anyway would
    // authorise whoever happens to hold uid 0's slot.
    if (len as usize) < std::mem::size_of::<libc::ucred>() || cred.pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERCRED returned a short answer",
        ));
    }
    Ok(cred.uid)
}

/// Set a file's mode, for the socket this server owns.
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// Handle to a running control server. Dropping it removes the socket file.
///
/// See the module documentation for the thread model, ownership split and
/// invariants.
pub struct ControlServer {
    name: String,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    /// The uid allowed to talk to this instance, from the kernel.
    owner_uid: u32,
}

impl ControlServer {
    /// The uid this server accepts connections from, as read from the kernel.
    ///
    /// Exposed so the behaviour can be *checked* rather than only claimed: a
    /// test asserts this is what `peer_uid` reports for a real connection.
    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Bind the socket for `name` and start serving on a background thread.
    ///
    /// `identity_json` is returned verbatim by `identify`. `hub` is the seam to
    /// the WM thread: `dispatch`/`quit`/`restart`/`reload` become
    /// `ControlCommand`s the WM drains, `state` reads the hub snapshot, and
    /// `subscribe` streams hub events.
    ///
    /// # Ownership
    ///
    /// Only the uid this process runs as may talk to the socket, and that uid
    /// is read from the kernel rather than passed in — a caller that could
    /// declare its own identity would make the check meaningless. The socket is
    /// also `0600` and its directory `0700`, so the kernel check in
    /// [`peer_uid`] is defence in depth against a permissive umask, a shared
    /// runtime directory, or a socket reached by a path this code does not
    /// control.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if the session directory cannot be created or the
    /// socket cannot be bound.
    pub fn spawn(name: &str, identity_json: String, hub: ControlHub) -> std::io::Result<Self> {
        // The kernel is the only acceptable source for "who owns this socket".
        // Real uid, because that is what `SO_PEERCRED` answers with; see
        // `identity::current_uid`.
        let owner_uid = identity::current_uid();
        // Validate early: reject traversal/overlong session ids before touching fs.
        let path = identity::try_sock_path(name)?;
        // The parent directory every session lives in; `0700` so a name is not
        // a list other users can read.
        identity::ensure_runtime_dir()?;
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
        // `bind` creates the socket with the process umask, which on many
        // systems is 022 — world-writable-adjacent. A control socket is a
        // one-user channel; make that explicit rather than inherited.
        set_mode(&path, 0o600)?;

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
                        // The peer's identity is read from the kernel and
                        // compared *here*, before a handler thread exists and
                        // before a byte is read: a rejected peer gets no
                        // reader, no writer and no protocol, so there is nothing
                        // for it to probe. `SO_PEERCRED` cannot be forged from
                        // userspace, which is what makes this a real boundary
                        // and not a check a caller can lie to.
                        match peer_uid(&stream) {
                            Ok(uid) if uid == owner_uid => {}
                            // Another uid: closed on drop, never spoken to. A
                            // failure to *read* the credentials is treated the
                            // same way — an unverifiable peer is not an
                            // authorised one.
                            _ => continue,
                        }
                        // Count the slot here, before the handler thread can
                        // exist, so the gate above is not a window where a
                        // second accept passes on a stale count. The guard then
                        // travels into the handler and is released there, even
                        // if the handler unwinds.
                        let slot = HandlerSlot::take(&srv_active);
                        let name = srv_name.clone();
                        let ident = identity_json.clone();
                        let hub = hub.clone();
                        let stop = srv_stop.clone();
                        thread::spawn(move || {
                            let _slot = slot;
                            handle_conn(stream, &name, &ident, &hub, &stop);
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
            owner_uid,
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

/// One live control-connection handler slot, against the `active` count the
/// accept loop gates on.
///
/// The count is the server's whole back-pressure mechanism, and it is only
/// correct if every handler gives its slot back on the way out. A bare
/// `fetch_sub` after the handler call is skipped when that call unwinds, and
/// nothing would ever return the slot: `MAX_CONCURRENT` of those stop the
/// accept loop outright, so the control server silently stops answering and
/// the window manager becomes unmanageable. Releasing from `Drop` instead makes
/// the accounting unwind-safe without catching anything — the panic still
/// propagates, and the connection dies with it, but the slot comes back.
struct HandlerSlot(Arc<AtomicUsize>);

impl HandlerSlot {
    /// Take one slot, on the accept thread, so the gate sees the new handler
    /// before it exists. The returned guard is moved into that handler's
    /// thread, which is where it is dropped.
    fn take(active: &Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, ORD);
        Self(active.clone())
    }
}

impl Drop for HandlerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, ORD);
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

/// Flatten a payload the hub published into one bounded protocol line.
///
/// All hub-published payloads must be single-line; a WM bug emitting `\n` would
/// otherwise desync `subscribe_stream`'s `lines()` framing. The bound is in
/// bytes but the payload is UTF-8, so the cut can land inside a multi-byte
/// character, and [`String::truncate`] panics on any offset that is not a
/// character boundary. Walk back to the last boundary at or before the limit: a
/// character is at most 4 bytes, so the walk settles within three steps, and
/// cutting a byte prefix leaves what is sent a valid, still-bounded prefix of
/// the payload.
fn single_line(s: &str) -> String {
    let mut out: String = s.replace(['\n', '\r'], " ");
    if out.len() > MAX_LINE_LEN {
        let mut cut = MAX_LINE_LEN;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

/// Turn a single request line into a response, enqueuing commands as needed.
fn dispatch_line(cmd: &str, name: &str, identity_json: &str, hub: &ControlHub) -> String {
    // `name` comes from `--name` (external input): sanitize for the
    // line protocol so `pong evil\ninject` cannot break framing.
    let safe_name: String = name.chars().filter(|c| !c.is_control()).take(128).collect();
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
/// Enforces single-line framing, a [`MAX_LINE_LEN`] byte bound and a write
/// timeout so one slow client cannot wedge its thread forever.
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
                // The event text is whatever the WM put on the hub, which traces
                // back to window titles and `WM_NAME`: the same framing and byte
                // bound the reply path needs, and for the same reason — the cut
                // must not land inside a multi-byte character.
                let out = single_line(&line);
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

    /// The peer check is the second line of the socket's security, and it has
    /// to be *verifiable*, not merely present: the test connects from this
    /// process and asserts the uid the kernel reports is the one the server
    /// recorded, which is the only way a same-uid process can check it. The
    /// cross-uid half needs a second account and is covered by
    /// `tests/session-security.sh`.
    #[test]
    fn the_server_records_this_process_uid_as_the_only_authorised_peer() {
        use std::os::unix::net::UnixStream;
        let name = format!("peercred{}", std::process::id());
        let _ = std::fs::remove_file(identity::sock_path(&name));
        let hub = ControlHub::new();
        let srv = ControlServer::spawn(&name, "{}".to_string(), hub).expect("spawn");
        let me = identity::current_uid();
        assert_eq!(srv.owner_uid(), me, "the owner must come from the kernel");

        // A connection from this process is the owner, and the kernel agrees.
        let path = identity::sock_path(&name);
        let stream = UnixStream::connect(&path).expect("connect");
        assert_eq!(peer_uid(&stream).expect("peer creds"), me);
        drop(stream);
        srv.shutdown();
        let _ = std::fs::remove_file(path);
    }

    /// The credentials are of the *peer process*, not of some path, so a socket
    /// pair reports this process too. That is the property the check rests on:
    /// there is no way for a connecting process to assert a different identity.
    #[test]
    fn peer_credentials_identify_the_process_not_the_path() {
        use std::os::unix::net::UnixStream;
        let (a, _b) = UnixStream::pair().expect("socketpair");
        assert_eq!(
            peer_uid(&a).expect("socketpair peers have credentials"),
            identity::current_uid()
        );
    }

    /// A descriptor the kernel cannot describe must be an error, never a uid.
    /// The accept loop treats both the same way — a peer it cannot identify is
    /// not an authorised one — so the error path is the one that has to hold:
    /// an uninitialised buffer reads as uid 0, and answering with that would
    /// authorise root's slot.
    #[test]
    fn an_undescribable_descriptor_is_an_error_not_a_uid() {
        use std::os::unix::io::AsRawFd;
        // A regular file is a real, open descriptor that is not a socket.
        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        assert!(file.as_raw_fd() >= 0);
        // The same bytes on a real socket must answer, so this is the
        // descriptor that fails and not the call.
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("pair");
        assert!(peer_uid(&a).is_ok(), "a real socket must answer");
    }

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

    // `active` is the count the accept loop checks against `MAX_CONCURRENT`, so
    // a slot lost on the way out of a handler is lost for the life of the
    // process: enough of them and the control server stops accepting anything,
    // which leaves the window manager unmanageable. The slot therefore has to
    // come back down on *every* exit, including while the thread unwinds — and
    // the panic itself still has to reach the runtime instead of being caught.
    #[test]
    fn a_handler_slot_is_released_even_when_the_handler_unwinds() {
        let active = Arc::new(AtomicUsize::new(0));

        std::thread::spawn({
            let active = active.clone();
            move || {
                let _slot = HandlerSlot::take(&active);
            }
        })
        .join()
        .expect("a handler that returns normally must not unwind");
        assert_eq!(
            active.load(ORD),
            0,
            "a returned handler must give its slot back"
        );

        let unwinding = std::thread::spawn({
            let active = active.clone();
            move || {
                let _slot = HandlerSlot::take(&active);
                panic!("handler unwind");
            }
        });
        assert!(
            unwinding.join().is_err(),
            "the unwind must propagate, not be swallowed at the slot"
        );
        assert_eq!(
            active.load(ORD),
            0,
            "a panicking handler must give its slot back"
        );
    }

    // The event stream is the one reply path whose payload the server does not
    // choose: `hub.emit` carries client-supplied text (window titles,
    // `WM_NAME`), so a multi-byte character can straddle the byte bound the
    // frame has to be cut at. Replayed more times than there are handler slots,
    // a subscriber that used to unwind on every oversized event must leave the
    // server still serving commands.
    #[test]
    fn subscribers_streaming_oversized_events_leave_the_server_serving() {
        let name = "testsubbig";
        let hub = ControlHub::new();
        let server = ControlServer::spawn(name, "{}\n".into(), hub.clone()).expect("server binds");
        // A 2-byte character laid across the bound: the offset the raw cut
        // cannot land on. The frame is the head without it, never a split of it.
        let kept = "x".repeat(MAX_LINE_LEN - 1);
        let payload = format!("{kept}é");
        for round in 0..(MAX_CONCURRENT * 2) {
            // Sinks are pruned by `emit`, so a connection that has only just
            // ended may still be counted: the round's own registration is the
            // one that has to be *new*, hence the baseline.
            let before = hub.subscriber_count();
            let mut s = UnixStream::connect(identity::try_sock_path(name).expect("sock path"))
                .expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            s.set_write_timeout(Some(Duration::from_secs(5))).ok();
            s.write_all(b"subscribe\n").expect("write subscribe");
            let mut lines = BufReader::new(&s).lines();
            assert_eq!(
                lines.next().expect("ack line").expect("read ack"),
                "ok subscribe",
                "round {round}: the subscribe was refused, so no slot was ever taken"
            );
            for _ in 0..250 {
                if hub.subscriber_count() > before {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Published only once the sink is registered, or the event goes to
            // an empty subscriber list and this client waits for a line that
            // was never sent.
            hub.emit(payload.clone());
            assert_eq!(
                lines.next().expect("event line").expect("read event"),
                kept,
                "round {round}: the subscriber did not get a bounded prefix of the event"
            );
        }
        assert!(
            ping(name).is_ok(),
            "every handler slot must be back after every connection"
        );
        server.shutdown();
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

/// Properties of the reply path, over the pure function that turns one request
/// line into one reply.
#[cfg(test)]
mod reply_props {
    use super::*;
    use crate::prop_support::{config, text};
    use proptest::prelude::*;

    /// A payload long enough to cross the protocol's line bound, with the
    /// character that lands on the truncation index generated freely — so the
    /// cut is exercised both on a character boundary and in the middle of a
    /// multi-byte one.
    fn oversized_payload() -> impl Strategy<Value = String> {
        (MAX_LINE_LEN - 2..=MAX_LINE_LEN + 2, any::<char>(), text()).prop_map(
            |(pad_len, boundary, tail)| {
                let mut s = "x".repeat(pad_len);
                s.push(boundary);
                s.push_str(&tail);
                s
            },
        )
    }

    // Every verb the server answers, plus the shapes only a malformed client
    // sends. `query <topic>` is the one request that is not a pure function of
    // its input — it blocks on the WM thread — so only its argument-less form
    // appears here.
    const VERBS: [&str; 12] = [
        PING_CMD,
        IDENTIFY_CMD,
        STATE_CMD,
        QUIT_CMD,
        RESTART_CMD,
        RELOAD_CMD,
        "dispatch kill",
        "dispatch",
        "query",
        "query ",
        "no-such-command",
        "dispatchfoo",
    ];

    // The client reads each reply with a single `read_line`, so one request must
    // produce exactly one frame: a second newline, a stray CR or an over-long
    // reply would desync every following command on the connection. The
    // instance name, the identity ficha and the state snapshot are all
    // attacker-influenced text the server has to flatten. Every verb is held to
    // the same frame with the same hostile inputs, so a hole in one branch
    // cannot hide behind the odds of the generator picking that branch.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn every_reply_is_exactly_one_bounded_frame(
            name in text(),
            identity in text(),
            snapshot in text(),
        ) {
            let hub = ControlHub::new();
            hub.publish_state(snapshot);
            for cmd in VERBS {
                assert_one_frame(cmd, &dispatch_line(cmd, &name, &identity, &hub))?;
            }
        }
    }

    // `MAX_LINE_LEN` exists so a single oversized payload cannot make the server
    // answer with a frame the client will mis-read: the reply has to be cut
    // back to one line whatever the payload holds, and cutting it must never
    // fail — including where the cut lands inside a multi-byte character.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn an_oversized_payload_is_still_answered_in_one_bounded_line(
            verb in prop_oneof![Just(IDENTIFY_CMD), Just(STATE_CMD)],
            name in text(),
            identity in oversized_payload(),
            snapshot in oversized_payload(),
        ) {
            let hub = ControlHub::new();
            hub.publish_state(snapshot);
            assert_one_frame(verb, &dispatch_line(verb, &name, &identity, &hub))?;
        }
    }

    // The shrinking target of the property above, made deterministic: a payload
    // that lays a multi-byte character across the byte bound, where the cut has
    // to stop at the character start instead of splitting it. Each width gets
    // its own case, and each case is walked to the byte before the bound, the
    // bound itself and the byte after it, so a regression cannot hide behind a
    // generator that rarely lands on a straddling offset.
    #[test]
    fn a_character_straddling_the_bound_is_cut_back_to_its_prefix() {
        for wide in ['\u{00e9}', '\u{20ac}', '\u{1f600}'] {
            // Where the payload ends relative to the bound: one byte short,
            // exactly on it, one byte past it. Only the last one straddles the
            // cut, since the character's last bytes are then past the bound
            // while its first ones are not.
            for over in [-1i64, 0, 1] {
                let head = MAX_LINE_LEN as i64 + over - wide.len_utf8() as i64;
                let payload = format!("{}{wide}", "x".repeat(head as usize));
                for verb in [IDENTIFY_CMD, STATE_CMD] {
                    let label = format!("{verb} with {wide:?} reaching {over:+}");
                    let hub = ControlHub::new();
                    hub.publish_state(payload.clone());
                    let reply = dispatch_line(verb, "testctl", &payload, &hub);
                    if let Err(e) = assert_one_frame(verb, &reply) {
                        panic!("{label} broke the frame contract: {e:?}");
                    }
                    let body = reply
                        .strip_suffix('\n')
                        .expect("reply is newline terminated");
                    // A payload that already ends inside the bound is answered
                    // whole; one that reaches past it loses the straddling
                    // character entirely, since half of it is not a reply.
                    let kept = if over > 0 {
                        head as usize
                    } else {
                        payload.len()
                    };
                    assert!(
                        std::str::from_utf8(body.as_bytes()).is_ok(),
                        "{label} answered with bytes that do not decode"
                    );
                    assert_eq!(
                        body,
                        &payload[..kept],
                        "{label} kept {} bytes, which is not the last character \
                         boundary at or before the bound",
                        body.len()
                    );
                }
            }
        }
    }

    /// A reply is one frame: a single trailing newline, no CR anywhere, and at
    /// most `MAX_LINE_LEN` bytes of payload in front of it.
    fn assert_one_frame(cmd: &str, reply: &str) -> Result<(), TestCaseError> {
        prop_assert!(
            reply.ends_with('\n'),
            "{:?} reply is not newline terminated: {:?}",
            cmd,
            reply
        );
        prop_assert_eq!(
            reply.matches('\n').count(),
            1,
            "{:?} reply carries more than one frame: {:?}",
            cmd,
            reply
        );
        prop_assert!(
            !reply.contains('\r'),
            "{cmd:?} reply carries a CR: {reply:?}"
        );
        prop_assert!(
            reply.len() <= MAX_LINE_LEN + 1,
            "{:?} reply is {} bytes, past the {} byte line bound",
            cmd,
            reply.len(),
            MAX_LINE_LEN
        );
        Ok(())
    }

    // Raw input is never echoed back: a client could otherwise inject control
    // sequences into a terminal or log that prints the error, and an unbounded
    // echo would turn the reply into an amplifier. The echoed token keeps the
    // printable part of what was sent, nothing more.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn unknown_command_replies_echo_nothing_but_its_printable_prefix(
            suffix in prop_oneof![3 => text(), 1 => "[ -~]{0,300}"],
        ) {
            // A glued-on non-blank suffix keeps `dispatch`/`query` from being a
            // verb with a delimiter, which is the refusal under test.
            prop_assume!(!suffix.is_empty() && !suffix.starts_with(char::is_whitespace));
            let hub = ControlHub::new();
            for cmd in [format!("dispatch{suffix}"), format!("query{suffix}")] {
                let reply = dispatch_line(&cmd, "testctl", "{}", &hub);
                let echoed = reply
                    .strip_prefix("error unknown-command: ")
                    .expect("a glued protocol word must be an unknown command")
                    .strip_suffix('\n')
                    .expect("reply is newline terminated");
                prop_assert!(
                    echoed.chars().count() <= 64,
                    "echo of {} chars is past the bound: {echoed:?}",
                    echoed.chars().count()
                );
                prop_assert!(
                    echoed.chars().all(|c| !c.is_control()),
                    "echo smuggles a control character: {echoed:?}"
                );
                prop_assert!(
                    is_subsequence(
                        echoed.chars(),
                        cmd.chars().filter(|c| !c.is_control())
                    ),
                    "echo invented characters that were never sent: {echoed:?} from {cmd:?}"
                );
            }
        }
    }

    /// True when every character of `needle` appears in `haystack` in order.
    fn is_subsequence(
        mut needle: impl Iterator<Item = char>,
        haystack: impl Iterator<Item = char>,
    ) -> bool {
        let mut haystack = haystack.peekable();
        needle.all(|c| haystack.by_ref().find(|&h| h == c).is_some())
    }
}

/// Properties of the event stream, the one output path whose payload the server
/// does not choose: `hub.emit` carries client-supplied text (window titles,
/// `WM_NAME`, whatever the WM puts in a title event), so the byte the frame has
/// to be cut at can land anywhere, including inside a multi-byte character.
#[cfg(test)]
mod event_props {
    use super::*;
    use crate::prop_support::{config, text};
    use proptest::prelude::*;

    /// Run one payload through [`stream_events`] and return what the subscriber
    /// saw on the wire.
    ///
    /// The sender is dropped before the call, so the loop writes the queued
    /// event and then leaves on `Disconnected` instead of parking on its recv
    /// timeout: a test observes one whole frame and nothing else. Reading the
    /// peer afterwards rather than before is safe for the same reason — the
    /// payload is bounded well under a socket buffer, so no write can block on
    /// a reader that has not started yet.
    fn stream_through(payload: String) -> Vec<u8> {
        let (mut writer, mut peer) = UnixStream::pair().expect("socket pair");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(payload).expect("queue the event");
        drop(tx);
        let stop = Arc::new(AtomicBool::new(false));
        stream_events(&mut writer, rx, &stop);
        drop(writer);
        let mut seen = Vec::new();
        peer.read_to_end(&mut seen).expect("drain the stream");
        seen
    }

    // The client reads the stream with one `read_line` per event, so one hub
    // event has to produce exactly one frame: a single trailing newline, no CR
    // anywhere, and at most `MAX_LINE_LEN` bytes of payload in front of it. And
    // a cut frame is still the head of what was published — never bytes the
    // WM did not emit, and never the same character twice.
    fn assert_one_event_frame(payload: &str, frame: &str) -> Result<(), TestCaseError> {
        prop_assert!(
            frame.ends_with('\n'),
            "event frame is not newline terminated: {frame:?}"
        );
        prop_assert_eq!(
            frame.matches('\n').count(),
            1,
            "event frame carries more than one frame: {:?}",
            frame
        );
        prop_assert!(!frame.contains('\r'), "event frame carries a CR: {frame:?}");
        let body = frame.strip_suffix('\n').expect("newline terminated");
        prop_assert!(
            body.len() <= MAX_LINE_LEN,
            "event body is {} bytes, past the {MAX_LINE_LEN} byte line bound",
            body.len()
        );
        // The framing turns a CR or LF into a space; both are one byte, so the
        // flattening moves no character boundary and leaves the bound and the
        // prefix relation to compare against the published payload.
        let clean = payload.replace(['\n', '\r'], " ");
        prop_assert!(
            clean.starts_with(body),
            "event body is not the head of the published payload: {frame:?}"
        );
        Ok(())
    }

    // The shrinking target of the property below, made deterministic: a payload
    // that lays a character across the byte bound, where the cut has to stop at
    // the character start instead of splitting it. Every character width gets
    // its own case, and each is walked to the byte before the bound, the bound
    // itself and the byte after it, so a regression cannot hide behind a
    // generator that rarely lands on a straddling offset.
    #[test]
    fn an_event_straddling_the_bound_is_streamed_as_its_utf8_prefix() {
        for wide in ['a', '\u{00e9}', '\u{20ac}', '\u{1f600}'] {
            // Where the payload ends relative to the bound: one byte short,
            // exactly on it, one byte past it. Only the last one straddles the
            // cut, since the character's last bytes are then past the bound
            // while its first ones are not.
            for over in [-1i64, 0, 1] {
                let head = MAX_LINE_LEN as i64 + over - wide.len_utf8() as i64;
                let payload = format!("{}{wide}", "x".repeat(head as usize));
                let label = format!("{wide:?} reaching {over:+}");
                let seen = stream_through(payload.clone());
                let frame = String::from_utf8(seen.clone())
                    .unwrap_or_else(|e| panic!("{label} wrote bytes that do not decode: {e}"));
                if let Err(e) = assert_one_event_frame(&payload, &frame) {
                    panic!("{label} broke the frame contract: {e:?}");
                }
                // A payload that already ends inside the bound is streamed
                // whole; one reaching past it loses the straddling character
                // entirely, since half of it is not a frame.
                let kept = if over > 0 {
                    head as usize
                } else {
                    payload.len()
                };
                assert_eq!(
                    frame.strip_suffix('\n').expect("newline terminated"),
                    &payload[..kept],
                    "{label} kept {} bytes, which is not the last character \
                     boundary at or before the bound",
                    frame.len()
                );
            }
        }
    }

    // That case is the shape that used to unwind the handler thread; this one
    // is the shape nobody thought of. The character landing on the bound, its
    // width and everything after it are generated freely, so the cut is
    // exercised on a boundary and inside every width a UTF-8 payload can have.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn an_event_of_any_character_width_is_streamed_as_one_bounded_frame(
            over in 0usize..8,
            boundary in prop_oneof![Just('a'), any::<char>()],
            tail in text(),
        ) {
            let mut payload = "x".repeat(MAX_LINE_LEN - over);
            payload.push(boundary);
            payload.push_str(&tail);
            let seen = stream_through(payload.clone());
            let frame = String::from_utf8(seen.clone())
                .unwrap_or_else(|e| panic!("a {} byte event wrote bytes that do not decode: {e}", seen.len()));
            assert_one_event_frame(&payload, &frame)?;
            // Exactly the last character boundary at or before the bound, and
            // not one byte less: a cut that under- or over-shoots still frames
            // correctly, but hands the client an event the WM never sent.
            let clean = payload.replace(['\n', '\r'], " ");
            let mut cut = MAX_LINE_LEN.min(clean.len());
            while !clean.is_char_boundary(cut) {
                cut -= 1;
            }
            prop_assert_eq!(frame.strip_suffix('\n').expect("terminated"), &clean[..cut]);
        }
    }
}
