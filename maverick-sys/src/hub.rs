// maverick-sys/src/hub.rs
// The bridge between the control-socket server thread and the WM's single
// X11 event-loop thread.
//
// The WM keeps all of its (non-Send) state on the main thread. The control
// server runs on its own thread and must never touch that state directly.
// `ControlHub` is the safe seam between them:
//
//   * commands  — clients send `dispatch`/`quit`/`restart`/`reload`; the server
//     thread pushes a `ControlCommand` onto an MPSC queue that the WM drains
//     once per event-loop iteration and executes there.
//   * state     — the WM publishes a cheap JSON snapshot after each change; the
//     server answers `state` by reading the cached string (no cross-thread
//     access to live WM structures).
//   * events    — the WM emits event lines (focus/workspace/layout/window);
//     `subscribe` connections receive them as they happen.
//
// Everything here is plain safe std: `Arc`, `Mutex`, and `mpsc`. No `unsafe`,
// no extra dependencies.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// A command requested by an external tool, to be executed by the WM on its
/// own thread. `Dispatch` carries an action name that the WM maps to its
/// internal `Action` vocabulary.
// NOTE: not `Eq`/`PartialEq` — `Query` carries a `Sender` reply channel, which
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
    /// thread blocks on the channel until it arrives.
    Query {
        topic: String,
        reply: Sender<String>,
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
    /// Sender half of the command queue (server thread -> WM thread).
    cmd_tx: Sender<ControlCommand>,
    /// Receiver half; guarded so `drain()` can be called from the WM thread.
    cmd_rx: Mutex<Receiver<ControlCommand>>,
    /// Latest state snapshot as JSON, published by the WM.
    state: Mutex<String>,
    /// Live `subscribe` sinks. Dead ones are pruned on the next `emit`.
    subscribers: Mutex<Vec<Sender<String>>>,
}

impl ControlHub {
    /// Create a fresh hub with empty state and no subscribers.
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = channel();
        ControlHub {
            inner: Arc::new(Inner {
                cmd_tx,
                cmd_rx: Mutex::new(cmd_rx),
                state: Mutex::new(String::from("{}")),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    // ── server thread side ────────────────────────────────────────────────

    /// Queue a command for the WM to execute. Called from the server thread.
    /// Returns `false` if the WM thread has gone away (receiver dropped).
    pub fn push_command(&self, cmd: ControlCommand) -> bool {
        self.inner.cmd_tx.send(cmd).is_ok()
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

    /// Register a new subscriber. Returns the receiving end; the server thread
    /// forwards every line it gets to the connected client until the client
    /// disconnects (at which point the sender send fails and gets pruned).
    pub fn subscribe(&self) -> Receiver<String> {
        let (tx, rx) = channel();
        if let Ok(mut subs) = self.inner.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }

    // ── WM thread side ────────────────────────────────────────────────────

    /// Drain all pending commands. Called once per event-loop iteration on the
    /// WM thread. Never blocks.
    pub fn drain_commands(&self) -> Vec<ControlCommand> {
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
    pub fn emit(&self, line: impl Into<String>) {
        let line = line.into();
        // Clone the subscribers list outside the lock so we never block the WM
        // thread while sending to potentially slow subscriber channels.
        let subs: Vec<_> = self
            .inner
            .subscribers
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        let mut dead = Vec::new();
        for (i, tx) in subs.iter().enumerate() {
            if tx.send(line.clone()).is_err() {
                dead.push(i);
            }
        }
        // Prune dead subscribers under lock.
        if !dead.is_empty() {
            if let Ok(mut s) = self.inner.subscribers.lock() {
                for &i in dead.iter().rev() {
                    if i < s.len() {
                        s.remove(i);
                    }
                }
            }
        }
    }

    /// Number of currently registered subscribers (for tests/introspection).
    pub fn subscriber_count(&self) -> usize {
        self.inner.subscribers.lock().map(|s| s.len()).unwrap_or(0)
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
        // Draining again yields nothing.
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
        // Drop the receiver: next emit should prune the dead subscriber.
        drop(rx);
        hub.emit("{\"event\":\"workspace\"}");
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn hub_clones_share_state() {
        let a = ControlHub::new();
        let b = a.clone();
        a.push_command(ControlCommand::Reload);
        // Drained from the other clone -> same underlying queue.
        let cmds = b.drain_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], ControlCommand::Reload));
    }
}
