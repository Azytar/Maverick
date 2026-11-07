//! Bridge between the control-socket server thread and the WM's single X11 event-loop thread.
//!
//! The WM keeps all of its (non-`Send`) state on the main thread. The control
//! server runs on its own thread and must never touch that state directly.
//! [`ControlHub`] is the safe seam between them:
//!
//! * **commands** — clients send `dispatch`/`quit`/`restart`/`reload`; the server
//!   thread pushes a [`ControlCommand`] onto an MPSC queue that the WM drains
//!   once per event-loop iteration and executes there.
//! * **state** — the WM publishes a cheap JSON snapshot after each change; the
//!   server answers `state` by reading the cached string (no cross-thread
//!   access to live WM structures).
//! * **events** — the WM emits event lines (focus/workspace/layout/window);
//!   `subscribe` connections receive them as they happen.
//!
//! Everything here is plain safe `std`: [`std::sync::Arc`], [`std::sync::Mutex`],
//! and [`std::sync::mpsc`]. No `unsafe`, no extra dependencies.
//!
//! # Ownership and thread model
//!
//! [`ControlHub`] is `#[derive(Clone)]` and cloning is **cheap**: it clones an
//! `Arc<Inner>` so all clones share the same command queue, state cache, and
//! subscriber list. One clone is moved into the [`crate::control::ControlServer`]
//! accept thread (and further cloned per connection); the original stays on the
//! WM thread. No additional synchronization is needed beyond the inner
//! `Mutex`es.
//!
//! # Invariants
//!
//! - `drain_commands` never blocks (`try_recv` loop); `publish_state`/`emit` hold
//!   their `Mutex` only long enough to swap/clone.
//! - `emit` never blocks the WM thread (`try_send` only): a slow subscriber's
//!   message is dropped and counted, a dead one is pruned on the next `emit`.
//! - `push_command` never blocks (`try_send` only): a full command queue
//!   returns `false` so the server can reply `error busy` instead of growing
//!   memory without bound.
//!
//! # Back-pressure
//!
//! | Channel      | Capacity            | Producer            | Consumer              | Full policy                    | Blocking? |
//! |--------------|---------------------|---------------------|-----------------------|--------------------------------|-----------|
//! | commands     | `CMD_CAP` (128)     | server conn threads | WM event-loop thread  | `try_send` fails → `false`     | never     |
//! | subscriber   | `SUB_CAP` (64 each) | WM thread (`emit`)  | per-sub conn thread   | drop msg + count, keep sub     | never     |
//! | query reply  | 1 (one-shot)        | WM thread           | requesting conn thread| `send` into empty slot / `Err` | never*    |
//!
//! `*` the query reply is sent exactly once into an empty 1-slot channel, so it
//! never blocks; if the requester already timed out the send just fails.

use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// Maximum queued control commands (server threads → WM thread).
///
/// Each entry is a tiny enum + short `String` (<64 B action). 128 bounds the
/// queue to a few KiB while absorbing legit bursts (scripts fanning out
/// dispatches). Beyond this the server replies `error busy` instead of
/// growing memory.
pub const CMD_CAP: usize = 128;
/// Per-subscriber event queue (WM thread → each `subscribe` connection).
///
/// Events are coalescible notifications (`focus`/`workspace`); a slow client
/// drops intermediate lines and re-queries `state`. 64 × ~32 B × 16 subs is
/// bounded to tens of KiB.
pub const SUB_CAP: usize = 64;

/// A command requested by an external tool, to be executed by the WM on its
/// own thread. `Dispatch` carries an action name that the WM maps to its
/// internal `Action` vocabulary.
// NOTE: not `Eq`/`PartialEq` — `Query` carries a `SyncSender` reply channel, which
// has no meaningful equality. Callers (and tests) that need to recognise a
// queued command match on it with `matches!` instead.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    /// Ask the WM to quit cleanly.
    Quit,
    /// Ask the WM to restart (re-exec).
    Restart,
    /// Reload configuration (if the WM supports it).
    Reload,
    /// Execute a named action, e.g. `focus-left`, `cycle-layout`, `view 3`.
    Dispatch(String),
    /// A structured read-only query ("workspaces", "tree", "focused", …).
    /// The WM answers by sending the result JSON through `reply`; the server
    /// thread blocks on the channel until it arrives (2 s timeout).
    Query {
        topic: String,
        reply: SyncSender<String>,
    },
}

/// Shared hub cloned into both the server thread and the WM thread.
///
/// Cloning is cheap (it clones `Arc`s) and all clones share the same queues
/// and caches.
#[derive(Clone)]
pub struct ControlHub {
    inner: Arc<Inner>,
}

struct Inner {
    /// Sender half of the bounded command queue (server thread -> WM thread).
    cmd_tx: SyncSender<ControlCommand>,
    /// Receiver half; guarded so `drain()` can be called from the WM thread.
    cmd_rx: Mutex<Receiver<ControlCommand>>,
    /// Readable end of a self-pipe used to wake the X11 poll loop when a
    /// control command is queued.
    wake_read: Mutex<std::os::unix::net::UnixStream>,
    /// Server-thread write end. Writes are non-blocking; a full pipe already
    /// means the reader has a wakeup pending.
    wake_write: Mutex<std::os::unix::net::UnixStream>,
    /// Latest state snapshot as JSON, published by the WM.
    state: Mutex<String>,
    /// Live `subscribe` sinks. Dead ones are pruned on the next `emit`.
    subscribers: Mutex<Vec<SyncSender<String>>>,
    /// Subscriber messages dropped because a queue was full. Control-plane
    /// observability only: incremented on drop (rare), read by tests/tools.
    /// `Relaxed` is enough — an approximate count is fine.
    dropped: AtomicUsize,
    /// Set by the WM thread before it tears the control socket down to restart.
    /// The socket thread stops answering as though it were the instance from
    /// this moment, so a client waiting on the handoff learns that the outgoing
    /// process is finished rather than having to catch the window where its
    /// socket happens to be absent — a window short enough that polling can
    /// miss it, which would leave the client unable to tell "restarted" from
    /// "the old instance never left".
    restarting: AtomicBool,
}

impl ControlHub {
    /// Declare that this process is about to stop being the instance behind its
    /// control socket. Must be called before the socket is torn down; after it
    /// returns true, every read-side command answers `error restarting` until
    /// this process exits.
    pub fn begin_restart(&self) {
        self.inner.restarting.store(true, Ordering::SeqCst);
    }

    /// Whether [`Self::begin_restart`] has been called.
    pub fn is_restarting(&self) -> bool {
        self.inner.restarting.load(Ordering::SeqCst)
    }
}

impl ControlHub {
    /// Create a fresh hub with empty state and no subscribers.
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = sync_channel(CMD_CAP);
        let (wake_read, wake_write) =
            std::os::unix::net::UnixStream::pair().expect("UnixStream pair for control wakeup");
        wake_read
            .set_nonblocking(true)
            .expect("nonblocking control wake reader");
        wake_write
            .set_nonblocking(true)
            .expect("nonblocking control wake writer");
        ControlHub {
            inner: Arc::new(Inner {
                cmd_tx,
                cmd_rx: Mutex::new(cmd_rx),
                wake_read: Mutex::new(wake_read),
                wake_write: Mutex::new(wake_write),
                state: Mutex::new(String::from("{}")),
                subscribers: Mutex::new(Vec::new()),
                dropped: AtomicUsize::new(0),
                restarting: AtomicBool::new(false),
            }),
        }
    }

    /// Queue a command for the WM to execute. Called from the server thread.
    /// Never blocks. Returns `false` when the queue is full or the WM thread
    /// has gone away (receiver dropped); the caller must reply `error busy`
    /// instead of `ok` so the loss is visible.
    pub fn push_command(&self, cmd: ControlCommand) -> bool {
        if self.inner.cmd_tx.try_send(cmd).is_err() {
            return false;
        }
        // A byte is only a readiness edge; a full nonblocking pipe is already
        // readable and needs no additional notification.
        if let Ok(mut wake) = self.inner.wake_write.lock() {
            let _ = wake.write(&[1u8]);
        }
        true
    }

    /// Raw descriptor included in the WM's `poll(2)` set.
    pub fn wake_fd(&self) -> RawFd {
        if let Ok(wake) = self.inner.wake_read.lock() {
            wake.as_raw_fd()
        } else {
            -1
        }
    }

    // The self-pipe carries no payload: drain it completely so `poll(2)` does
    // not keep reporting readability from a stale wakeup.
    fn drain_wake(&self) {
        let mut buf = [0u8; 64];
        if let Ok(mut wake) = self.inner.wake_read.lock() {
            while let Ok(n) = wake.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        }
    }

    /// Read the latest published state snapshot (JSON). Called from the server
    /// thread to answer `state`.
    pub fn snapshot(&self) -> String {
        self.inner
            .state
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| String::from("{}"))
    }

    /// Register a new subscriber without any capacity check. The server thread
    /// forwards every line the receiver gets to the connected client until the
    /// client disconnects (at which point the sender fails and gets pruned).
    /// Each subscriber gets a bounded (`SUB_CAP`) queue so a slow client can
    /// never grow memory without bound — see `emit`.
    ///
    /// Uncapped: callers that must respect a subscriber budget have to use
    /// [`ControlHub::try_subscribe`], which decides and registers atomically.
    pub fn subscribe(&self) -> Receiver<String> {
        let (tx, rx) = sync_channel(SUB_CAP);
        if let Ok(mut subs) = self.inner.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }

    /// Register a new subscriber only while fewer than `max` sinks are
    /// registered, returning the receiving end (see [`ControlHub::subscribe`]).
    ///
    /// Returns `None` when the cap is already reached so the caller can reject
    /// the client instead of parking it on a stream nobody feeds. Registering
    /// through this method can never push `subscriber_count()` past `max`:
    /// the budget check and the registration share one critical section, so
    /// concurrent callers cannot each pass a stale check and overshoot the cap.
    /// A poisoned list counts as full — it never admits an unbounded number of
    /// sinks.
    pub fn try_subscribe(&self, max: usize) -> Option<Receiver<String>> {
        let (tx, rx) = sync_channel(SUB_CAP);
        let mut subs = self.inner.subscribers.lock().ok()?;
        if subs.len() >= max {
            return None;
        }
        subs.push(tx);
        Some(rx)
    }

    /// Drain all pending commands. Called once per event-loop iteration on the
    /// WM thread. Never blocks.
    ///
    /// The self-pipe is drained *first* and the queue second, because the two
    /// are one wakeup split in half: `push_command` enqueues the command and
    /// then writes the byte, so the pipe's readability and the queue describe
    /// the same arrivals and have to be taken together. Taking the queue first
    /// leaves a window between the two steps in which a command that is already
    /// enqueued has its byte discarded along with the stale ones — the WM then
    /// finds an empty pipe, blocks in `poll(2)`, and leaves the command sitting
    /// on the queue until an unrelated X event happens to wake it. `quit` then
    /// times out and `dispatch` silently does nothing.
    ///
    /// This order cannot lose one. `push_command` writes the byte *after* the
    /// command, so a byte still pending when the queue is taken necessarily
    /// belongs to a command this call has just returned, and the next call
    /// discards it: one spurious wakeup, never a missing one.
    pub fn drain_commands(&self) -> Vec<ControlCommand> {
        self.drain_wake();
        let mut out = Vec::new();
        if let Ok(rx) = self.inner.cmd_rx.lock() {
            while let Ok(cmd) = rx.try_recv() {
                out.push(cmd);
            }
        }
        out
    }

    /// Publish a new state snapshot (JSON). Called by the WM after a change.
    pub fn publish_state(&self, json: impl Into<String>) {
        if let Ok(mut s) = self.inner.state.lock() {
            *s = json.into();
        }
    }

    /// Emit an event line to every live subscriber, pruning dead ones.
    /// The `line` should be a single JSON object without a trailing newline;
    /// the server adds the newline framing.
    ///
    /// Never blocks the WM thread: `try_send` only. A `Full` queue means a
    /// slow subscriber — its message is dropped and counted (`dropped_events`),
    /// the subscriber is kept (it can re-query `state`). A `Disconnected`
    /// queue is pruned.
    pub fn emit(&self, line: impl Into<String>) {
        let line = line.into();
        if let Ok(mut s) = self.inner.subscribers.lock() {
            s.retain(|tx| match tx.try_send(line.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            });
        }
    }

    /// Number of currently registered subscribers (for tests/introspection).
    pub fn subscriber_count(&self) -> usize {
        self.inner.subscribers.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Messages dropped because a subscriber queue was full.
    /// Control-plane observability only; `Relaxed` count, may lag slightly.
    pub fn dropped_events(&self) -> usize {
        self.inner.dropped.load(Ordering::Relaxed)
    }
}

impl Default for ControlHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        let hub = ControlHub::new();
        assert!(hub.push_command(ControlCommand::Quit));
        assert!(hub.push_command(ControlCommand::Dispatch("focus-left".into())));
        let cmds = hub.drain_commands();
        assert_eq!(cmds.len(), 2);
        assert!(matches!(cmds[0], ControlCommand::Quit));
        assert!(
            matches!(&cmds[1], ControlCommand::Dispatch(a) if a == "focus-left"),
            "second command must be the queued dispatch, got {:?}",
            cmds[1]
        );
        assert!(hub.drain_commands().is_empty());
    }

    #[test]
    fn state_snapshot_publishes() {
        let hub = ControlHub::new();
        assert_eq!(hub.snapshot(), "{}");
        hub.publish_state("{\"focus\":42}");
        assert_eq!(hub.snapshot(), "{\"focus\":42}");
    }

    #[test]
    fn events_reach_subscribers_and_prune() {
        let hub = ControlHub::new();
        let rx = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 1);
        hub.emit("{\"event\":\"focus\"}");
        assert_eq!(rx.recv().unwrap(), "{\"event\":\"focus\"}");
        drop(rx);
        hub.emit("{\"event\":\"workspace\"}");
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn hub_clones_share_state() {
        let a = ControlHub::new();
        let b = a.clone();
        a.push_command(ControlCommand::Reload);
        let cmds = b.drain_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], ControlCommand::Reload));
    }

    #[test]
    fn command_wakes_the_x11_poll_loop() {
        let hub = ControlHub::new();
        assert!(hub.push_command(ControlCommand::Reload));
        assert!(crate::wait_readable_fds(
            &[hub.wake_fd()],
            Some(std::time::Duration::from_millis(100))
        ));
        let cmds = hub.drain_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], ControlCommand::Reload));
        assert!(!crate::wait_readable_fds(
            &[hub.wake_fd()],
            Some(std::time::Duration::from_millis(1))
        ));
    }

    #[test]
    fn command_queue_is_bounded_and_never_blocks() {
        let hub = ControlHub::new();
        for i in 0..CMD_CAP {
            assert!(
                hub.push_command(ControlCommand::Dispatch(format!("a{i}"))),
                "push {i} must succeed while filling"
            );
        }
        assert!(
            !hub.push_command(ControlCommand::Dispatch("overflow".into())),
            "queue must reject beyond CMD_CAP"
        );
        let cmds = hub.drain_commands();
        assert_eq!(cmds.len(), CMD_CAP);
        assert!(hub.push_command(ControlCommand::Quit));
        let cmds = hub.drain_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], ControlCommand::Quit));
    }

    #[test]
    fn slow_subscriber_drops_but_stays_connected() {
        let hub = ControlHub::new();
        let rx = hub.subscribe();
        // Never read: fill the per-subscriber queue, then overflow it.
        for _ in 0..(SUB_CAP + 10) {
            hub.emit("{\"event\":\"focus\"}");
        }
        assert_eq!(hub.subscriber_count(), 1, "slow subscriber is kept");
        assert_eq!(
            hub.dropped_events(),
            10,
            "only the overflow beyond SUB_CAP is counted"
        );
        // The queue holds the first SUB_CAP lines; drain them to prove order.
        for _ in 0..SUB_CAP {
            assert_eq!(rx.try_recv().unwrap(), "{\"event\":\"focus\"}");
        }
        hub.emit("{\"event\":\"focus\"}");
        assert_eq!(hub.dropped_events(), 10);
        assert_eq!(rx.try_recv().unwrap(), "{\"event\":\"focus\"}");
        drop(rx);
        hub.emit("{\"event\":\"workspace\"}");
        assert_eq!(hub.subscriber_count(), 0, "dead subscriber is pruned");
    }
}
