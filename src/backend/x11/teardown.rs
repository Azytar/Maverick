//! The two halves of a window manager shutdown, and the proof that separates them.
//!
//! A shutdown does two unrelated things: it releases X resources, and it makes
//! the record of this session truthful — the identity ficha, the control socket
//! and the compositor trace. The second half has nothing to do with X and must
//! happen on *every* exit path, including the one where the X server is already
//! gone.
//!
//! Those two requirements are in tension, and the tension is where the loss
//! used to come from. X teardown is best-effort by nature — a `flush` on a dead
//! connection fails immediately, a void request fails silently — so the natural
//! way to write it is one function full of `let _ =` and a `?` at the end, and
//! that shape is *also* what makes the local half skippable: one failed `flush`
//! returns out of the middle of the function and everything after it is gone.
//! Both halves of this module exist so that cannot happen again:
//!
//! - [`run_local`] carries no connection of any kind, so it is structurally
//!   incapable of issuing an X request and cannot be skipped by one.
//! - [`LiveX`] is the only way to reach the connection, and it is only
//!   constructible while the connection still works, so the X half cannot be
//!   *entered* with a dead connection.
//!
//! The two together are what let `WindowManager::shutdown` say which half it
//! skipped and why, instead of losing both and reporting nothing.

use maverick_sys::ControlServer;
use maverick_x11::XConn;

use crate::backend::x11::trace::{self, DumpReport, TraceEnd};

/// Why the window manager is shutting down. Only [`Self::XConnectionLost`] may
/// skip the X half; every variant runs the local half.
///
/// The variant is a statement about the *session*, not about which half happened
/// to succeed: it becomes the trace header's `end=`, so it is the thing a reader
/// of that file has to be told. What the X half actually managed is derived from
/// the connection rather than from this, and the two can only ever agree — see
/// [`TraceEnd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// The window manager was asked to stop: a signal, a control-socket `quit`,
    /// a keybind, or a `restart` about to `exec`.
    Clean,
    /// The X server went away first. Every X request from here on fails, so
    /// there is nothing to release and nothing to release it with.
    XConnectionLost,
}

/// Whether the X half of a shutdown may run.
///
/// Two things have to be true, and neither implies the other:
///
/// - the reason is not [`ShutdownReason::XConnectionLost`], which is the caller
///   saying the session ended *because* the X server went away. That statement
///   outranks anything the connection might report: a shutdown that ignored it
///   and issued requests anyway would be trusting a liveness check over the
///   event that produced the shutdown in the first place.
/// - the connection is still usable, because a request on a dead one does not
///   report that it failed. It reaches the server that is not there, returns
///   success, and leaves the connection in a state where libX11's I/O error
///   handler ends the process without unwinding — at the next operation that
///   waits for a reply, or at the display's close — which is after the local
///   half would have run.
///
/// The local half is not in this table because it has no condition: it runs
/// every time.
pub(crate) const fn runs_x_half(reason: ShutdownReason, x_live: bool) -> bool {
    matches!(reason, ShutdownReason::Clean) && x_live
}

/// Borrow of the X connection that exists only while it is usable.
///
/// Constructed only by [`LiveX::acquire`], the one place that asks the
/// connection whether it still works. Holding a `&LiveX` is therefore the
/// permission to issue an X request, and there is no other way to obtain one.
///
/// This is what makes "cannot be called with a dead connection" a property of
/// the types rather than of a review: the window manager's X half of a shutdown
/// takes one, and a `None` from `acquire` is the only alternative — which is the
/// local half, alone.
pub(crate) struct LiveX<'a> {
    conn: &'a XConn,
}

impl<'a> LiveX<'a> {
    /// Borrow the connection if it can still carry a request, or `None` if it
    /// cannot.
    ///
    /// `has_error` is the connection's own record of having failed, so this asks
    /// the object that would have to work rather than assuming. It is the same
    /// state the event loop ends on, which is what makes the answer stable: by
    /// the time a shutdown runs, a lost connection has already been observed
    /// through the event queue.
    pub(crate) fn acquire(conn: &'a XConn) -> Option<Self> {
        conn.has_error().is_none().then_some(LiveX { conn })
    }

    /// The connection this borrow was taken from. Every X request in the
    /// shutdown goes through here rather than through the window manager's own
    /// handle, so the request and the proof it was safe to issue are the same
    /// object and cannot drift apart.
    pub(crate) fn conn(&self) -> &XConn {
        self.conn
    }
}

/// What the local half of a shutdown did.
///
/// The local half cannot fail — every step of it is either a filesystem removal
/// or a `Drop` — but it is not *nothing*, and the exit path that has just lost
/// its X server is exactly where a silent one hides a leak. Reporting what ran
/// is what lets the caller say so out loud.
pub(crate) struct LocalTeardown {
    /// The identity record was removed, or there was none to remove. A session
    /// with no id (only reachable before init finished) has nothing to remove,
    /// and that is not a failure.
    pub(crate) ficha_removed: bool,
    /// The control socket was released by dropping its handle. Non-joining, on
    /// purpose — see [`run_local`].
    pub(crate) control_dropped: bool,
    /// The compositor trace dump. [`DumpReport::written`] is false both when
    /// tracing was never enabled and when the write failed; the two are told
    /// apart by `error`.
    pub(crate) trace: DumpReport,
}

/// The X-independent half of a shutdown: remove this session's record of itself.
///
/// Deliberately *not* a method on [`WindowManager`](super::WindowManager). As one
/// it would have `self.conn` in reach, and the whole claim — that this half
/// cannot issue an X request, and therefore cannot be skipped by one — would be a
/// property of its body rather than of its signature. Taking the two fields it
/// actually needs makes the claim checkable: there is no connection here to
/// request on.
///
/// Runs on every exit path, and in this order:
///
/// 1. the identity ficha, so a tool that lists instances stops listing a session
///    that is no longer running *before* anything that can still block is
///    dropped;
/// 2. the control socket, by dropping its handle;
/// 3. the compositor trace, which is the last thing the process knows that
///    cannot be reconstructed afterwards.
///
/// The control handle is dropped and not joined. `ControlServer::drop` stops
/// accepting and unlinks the socket, but its accept thread is deliberately not
/// joined: it is blocked in `accept` on a socket nothing will ever connect to
/// again, and joining it would turn the local half into a wait on a thread that
/// can only be released by the process exiting. The socket is unlinked by the
/// drop itself, so nothing on disk outlives the process either way.
pub(crate) fn run_local(
    session_id: &str,
    control: &mut Option<ControlServer>,
    end: TraceEnd,
) -> LocalTeardown {
    let ficha_removed = if session_id.is_empty() {
        false
    } else {
        // A no-op when the record is already gone, and deliberately not a
        // failure: this runs on the path where the process is already in
        // trouble, and a second failure to remove a file must not take the rest
        // of the teardown with it.
        maverick_sys::identity::cleanup_meta(session_id);
        true
    };
    let had_control = control.is_some();
    // `drop` runs the handle's destructor, which unlinks the socket; the thread
    // it owns is left to finish on its own (see above).
    drop(control.take());
    let trace = trace::dump(end);
    LocalTeardown {
        ficha_removed,
        control_dropped: had_control,
        trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime directory of this test's own, so a test that writes a record
    /// cannot see — or be seen by — another test's.
    fn temp_runtime() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("maverick-teardown-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp runtime dir");
        dir
    }

    /// An identity record for `sid`, written through the same call the window
    /// manager uses at startup, so the test exercises the real path and the real
    /// file permissions.
    fn write_ficha(sid: &str) -> std::path::PathBuf {
        let info = maverick_sys::identity::InstanceInfo {
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
        };
        maverick_sys::identity::write_meta(&info).expect("ficha is written");
        maverick_sys::identity::meta_path(sid)
    }

    /// The record of a live session is not the record of a dead one.
    ///
    /// This is the loss the local half exists to prevent, tested without an X
    /// server because the loss has nothing to do with X: what survives the
    /// process is two files, and both are removed by the local half alone.
    #[test]
    fn the_local_half_removes_the_record_and_the_socket() {
        let dir = temp_runtime();
        // SAFETY: single-threaded test body, and the variable is restored before
        // the test returns. `XDG_RUNTIME_DIR` is read by `identity` on every
        // call rather than cached, so pointing it at a temp dir is what puts the
        // record under test.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let sid = "teardown-local";
        let ficha = write_ficha(sid);
        let socket = maverick_sys::identity::try_sock_path(sid).expect("a socket path");
        let mut control = Some(
            maverick_sys::ControlServer::spawn(sid, String::new(), maverick_sys::ControlHub::new())
                .expect("a control server binds"),
        );
        assert!(ficha.is_file(), "the record must exist before the teardown");
        assert!(socket.exists(), "the socket must exist before the teardown");

        let report = run_local(sid, &mut control, TraceEnd::CleanExit);

        assert!(report.ficha_removed, "the ficha step did not run");
        assert!(report.control_dropped, "the control handle was not held");
        assert!(control.is_none(), "the control handle was not released");
        assert!(!ficha.exists(), "the identity record survived the teardown");
        assert!(!socket.exists(), "the control socket survived the teardown");
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    /// A session with no id has no record to remove, and that is not a failure.
    ///
    /// Only reachable before init finished, but the teardown runs on the way out
    /// of *every* path including that one, and a teardown that panicked on an
    /// empty id would take the process down on the way out.
    #[test]
    fn a_session_with_no_id_still_completes() {
        let mut control = None;
        let report = run_local("", &mut control, TraceEnd::CleanExit);
        assert!(!report.ficha_removed, "there was no record to remove");
        assert!(!report.control_dropped, "no control handle was held");
    }

    /// The X half is allowed only for a clean exit over a working connection.
    ///
    /// Each row is one way the two conditions can disagree, and both of those
    /// rows are the ones a fix for this defect tends to get wrong: a shutdown
    /// that ran the X half "and ignored the error" fails the third row, and one
    /// that trusted the connection's own liveness answer alone fails the second
    /// one only on a connection that has not noticed yet.
    #[test]
    fn the_x_half_runs_only_for_a_clean_exit_over_a_live_connection() {
        let cases = [
            // asked to stop, server still there: the normal case.
            (ShutdownReason::Clean, true, true),
            // asked to stop, but the server died first: the `flush` that used to
            // abort the whole teardown is never reached.
            (ShutdownReason::Clean, false, false),
            // The caller says the server is gone. A connection that still
            // answers `has_error` is not asked to.
            (ShutdownReason::XConnectionLost, true, false),
            // Both agree.
            (ShutdownReason::XConnectionLost, false, false),
        ];
        for (reason, live, expected) in cases {
            assert_eq!(
                runs_x_half(reason, live),
                expected,
                "{reason:?} over a {} connection",
                if live { "live" } else { "dead" }
            );
        }
    }

    /// The one thing a reason may change is the X half.
    ///
    /// A table over both reasons, and for each: whether the X half would run
    /// over a live connection, and whether the local half actually ran. The
    /// second column is the one the whole design is for — a reason that could
    /// make it `false` is a reason that leaks an identity record, and it is
    /// invisible on every run that ends cleanly.
    #[test]
    fn a_reason_may_skip_the_x_half_but_never_the_local_one() {
        struct Case {
            reason: ShutdownReason,
            end: TraceEnd,
            sid: &'static str,
            x_half_over_a_live_connection: bool,
        }
        let cases = [
            Case {
                reason: ShutdownReason::Clean,
                end: TraceEnd::CleanExit,
                sid: "teardown-clean",
                x_half_over_a_live_connection: true,
            },
            Case {
                reason: ShutdownReason::XConnectionLost,
                end: TraceEnd::XConnectionLost,
                sid: "teardown-lost",
                x_half_over_a_live_connection: false,
            },
        ];
        for case in cases {
            let dir = temp_runtime();
            // SAFETY: as above — restored before the test returns, and the body
            // is single-threaded.
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
            let ficha = write_ficha(case.sid);
            assert!(ficha.is_file(), "{} has no record to remove", case.sid);

            assert_eq!(
                runs_x_half(case.reason, true),
                case.x_half_over_a_live_connection,
                "{:?}: wrong decision for the X half",
                case.reason
            );
            let report = run_local(case.sid, &mut None, case.end);

            assert!(
                report.ficha_removed,
                "{:?} left the identity record on disk",
                case.reason
            );
            assert!(
                !ficha.exists(),
                "{:?} left the identity record on disk",
                case.reason
            );
            let _ = std::fs::remove_dir_all(&dir);
            unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        }
    }
}
