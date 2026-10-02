//! What makes `maverickctl restart` a command that can be repeated.
//!
//! The control socket answers `restart` from its connection thread, the moment
//! the command is *enqueued* — before the window manager has torn anything down,
//! and long before a replacement exists. The property these tests pin is
//! therefore not "the command was accepted" but "an instance is serving this
//! session id again". Without that wait every one of these fixtures reports a
//! successful restart while the instance it names is unreachable, which is what
//! made a second `restart` land in a gap it could not resolve.
//!
//! This is a separate test binary on purpose, for the reason `ctl_replies`
//! states: the fixtures here need `XDG_RUNTIME_DIR` pointed at a throwaway
//! directory, and the identity and control unit tests assert properties of the
//! real runtime-directory resolution that moving it out from under them would
//! invalidate.

use maverickctl::ctl::main_with_args;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod runtime_dir;

/// A private runtime directory, so no fixture here is visible to another test
/// binary's fixtures and no live instance can be discovered.
fn isolate_runtime_dir() {
    runtime_dir::isolate("maverick-restart");
}

/// A stand-in instance speaking the real line protocol.
///
/// `serves_state` separates the two things a bound socket can be: one whose
/// window-manager thread is publishing snapshots, and one that is only a
/// listener the replacement has bound but not yet attached to. The second
/// answers `ping` and `{}` — precisely the state a restart returns in when it is
/// acknowledged at enqueue time.
/// What a stand-in instance is doing behind its socket.
#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    /// A window manager whose event loop is publishing snapshots.
    Serving,
    /// Bound, but the WM thread has not attached to it yet — answers `ping`
    /// and `{}`, which is the state a restart returns in when it is
    /// acknowledged at enqueue time.
    BoundButSilent,
    /// Has declared its own departure and answers `error restarting`, without
    /// ever unbinding. The real instance does this for the few requests that
    /// can still reach it between `ControlHub::begin_restart` and the socket
    /// going away.
    Departing,
}

struct Instance {
    sid: String,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    mode: Arc<std::sync::Mutex<Behaviour>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Instance {
    fn start(sid: &str, behaviour: Behaviour) -> Self {
        let path = maverick_sys::identity::sock_path(sid);
        std::fs::create_dir_all(path.parent().expect("session dir")).expect("create dir");
        let _ = std::fs::remove_file(&path);
        // Discovery resolves by ficha, so a socket alone is not an instance the
        // tool can target — and a test that could not resolve its target would
        // pass for a reason unrelated to the wait under test. `start_time` is
        // left at 0, which liveness reads as "do not check".
        maverick_sys::identity::write_meta(&maverick_sys::identity::InstanceInfo {
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
        let listener = UnixListener::bind(&path).expect("bind fixture socket");
        listener.set_nonblocking(true).expect("nonblocking accept");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let mode = Arc::new(std::sync::Mutex::new(behaviour));
        let seen = Arc::clone(&mode);
        let name = sid.to_string();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut line = String::new();
                        let mode = *seen.lock().expect("behaviour lock");
                        let reply = match BufReader::new(&stream).read_line(&mut line) {
                            Err(_) => continue,
                            Ok(_) if mode == Behaviour::Departing => {
                                "error restarting\n".to_string()
                            }
                            Ok(_) if line.starts_with("ping") => format!("pong {name}\n"),
                            Ok(_) if line.starts_with("query ") => {
                                if mode == Behaviour::Serving {
                                    "{\"monitors\":[{\"index\":0}]}\n".to_string()
                                } else {
                                    "{}\n".to_string()
                                }
                            }
                            Ok(_) => "ok\n".to_string(),
                        };
                        let _ = stream.write_all(reply.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            sid: sid.to_string(),
            path,
            stop,
            mode,
            handle: Some(handle),
        }
    }

    /// Change what this same, still-bound socket answers. The point of the
    /// departure announcement is that the socket never has to go away, so the
    /// tests need a handoff that provably does not unlink.
    fn switch_to(&self, behaviour: Behaviour) {
        *self.mode.lock().expect("behaviour lock") = behaviour;
    }

    /// Model the teardown half of the handoff: the socket stops answering and
    /// its path goes away, as `identity::cleanup_meta` leaves it across `exec`.
    ///
    /// The record deliberately survives. Across an `exec` the session id has to
    /// stay addressable while the replacement starts, and the point of the
    /// restart tests is that state: an addressable session with nothing
    /// listening is a different failure from a session that never existed, and
    /// the tool reports the first one (`restart failed: No such file or
    /// directory`) and not the second. Taking the record here as well would make
    /// every restart test pass for the wrong reason.
    fn handoff_out(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.handoff_out();
        // Nothing is waiting on this session any more, so the record goes too.
        // Leaving it is what made this binary the one that did not empty its
        // runtime directory: six records per run, accumulating under `$TMPDIR`,
        // each still selectable by name — `find_by_name` matches a stale record
        // as readily as a live one, and the tool then fails at the connect with
        // an error about a session nothing is serving.
        maverick_sys::identity::cleanup_meta(&self.sid);
    }
}

fn restart(sid: &str) -> ExitCode {
    main_with_args(
        "maverickctl",
        vec!["--name".to_string(), sid.to_string(), "restart".to_string()],
    )
}

/// The regression the settle wait exists for.
///
/// An instance that never tears down never restarted. There was nothing to
/// wait for, and reporting success anyway is exactly what let a script believe
/// a restart had happened while the old process was still the one answering.
#[test]
fn an_instance_that_never_hands_off_is_not_a_finished_restart() {
    isolate_runtime_dir();
    let _instance = Instance::start("neverswapped", Behaviour::Serving);
    assert_eq!(restart("neverswapped"), ExitCode::FAILURE);
}

/// A socket the replacement has bound but not yet attached its event loop to
/// answers `ping` and `{}`. It is the window that used to swallow the next
/// command.
#[test]
fn a_rebound_socket_that_publishes_no_snapshot_is_not_a_finished_restart() {
    isolate_runtime_dir();
    let mut first = Instance::start("halfup", Behaviour::Serving);
    first.handoff_out();
    let _second = Instance::start("halfup", Behaviour::BoundButSilent);
    assert_eq!(restart("halfup"), ExitCode::FAILURE);
}

/// An `exec` that never came back leaves the session id addressable and nothing
/// listening. Success here would be indistinguishable from a hang being
/// reported as a completed restart.
#[test]
fn an_instance_that_never_returns_is_not_a_finished_restart() {
    isolate_runtime_dir();
    let mut first = Instance::start("gone", Behaviour::Serving);
    first.handoff_out();
    assert_eq!(restart("gone"), ExitCode::FAILURE);
}

/// The positive case, and the shape the others are variations of: the outgoing
/// instance tears down, the replacement comes back serving a real snapshot, and
/// only then does the tool report success.
///
/// The replacement has to appear *while* the tool is waiting. A restart
/// command is sent first, so the wait necessarily begins with the outgoing
/// instance still answering — which is why readiness is defined as coming back
/// after having gone, rather than as being up.
#[test]
fn a_replacement_that_serves_a_snapshot_finishes_the_restart() {
    isolate_runtime_dir();
    let sid = "swapped";
    let first = Instance::start(sid, Behaviour::Serving);
    let name = sid.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(80));
        drop(first);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let _second = Instance::start(&name, Behaviour::Serving);
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    assert_eq!(restart(sid), ExitCode::SUCCESS);
}

/// The handoff can be shorter than a client can poll for.
///
/// The socket is unbound for only as long as the replacement takes to start,
/// which on a warm X server is milliseconds. Waiting for the socket to *vanish*
/// therefore made a fast restart indistinguishable from one that never
/// happened, and the client reported a completed handoff as a failure.
///
/// The outgoing instance announces its departure instead of relying on a window
/// being observable: between `ControlHub::begin_restart` and the socket going
/// away it answers `error restarting`, and that is as good as being gone. This
/// fixture never unbinds at all — one socket, three behaviours — so the
/// announcement is the only evidence the handoff occurred, and a client that
/// polls for the socket to disappear can never see one.
#[test]
fn an_announced_departure_finishes_the_restart_without_ever_unbinding() {
    isolate_runtime_dir();
    let sid = "announced";
    let first = Instance::start(sid, Behaviour::Serving);
    let _name = sid.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(80));
        first.switch_to(Behaviour::Departing);
        std::thread::sleep(std::time::Duration::from_millis(80));
        // The replacement takes over the same, never-unbound socket.
        first.switch_to(Behaviour::Serving);
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    assert_eq!(restart(sid), ExitCode::SUCCESS);
}

/// An instance that announces its departure and never comes back has restarted
/// nothing, and must not be reported as though it had.
#[test]
fn an_announced_departure_that_never_returns_is_not_a_finished_restart() {
    isolate_runtime_dir();
    let sid = "announced-gone";
    let first = Instance::start(sid, Behaviour::Serving);
    let _name = sid.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(80));
        first.switch_to(Behaviour::Departing);
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    assert_eq!(restart(sid), ExitCode::FAILURE);
}

/// A handoff is not a shutdown, and the record is the whole difference.
///
/// Across an `exec` the session id has to stay addressable while the
/// replacement starts, so an addressable session with nothing listening has to
/// remain a state the tool can resolve and then fail on. Tearing the record
/// down with the socket would collapse that into "no such session", and the
/// restart assertions above only compare exit codes — both failures are
/// `FAILURE` — so the whole file would keep passing while testing something
/// else. This pins the half of the contract that keeps them honest.
#[test]
fn a_handoff_leaves_the_session_addressable() {
    isolate_runtime_dir();
    let sid = "handoffsid";
    let mut first = Instance::start(sid, Behaviour::Serving);
    first.handoff_out();
    assert!(
        !maverick_sys::identity::sock_path(sid).exists(),
        "socket must go"
    );
    assert!(
        maverick_sys::identity::read_meta(sid).is_some(),
        "record must survive the handoff: across an exec the session stays addressable"
    );
    assert!(
        maverickctl::discover::find_by_name(sid).is_some(),
        "a handoff leaves a resolvable target, not an absent one"
    );
}

/// The other half: once nothing is waiting on the session, it stops being
/// published at all.
///
/// Without this the record outlives every owner and accumulates under
/// `$TMPDIR` a file per fixture per run, and a stale one is still selectable by
/// name — `find_by_name` does not filter on liveness — so the tool resolves a
/// session nobody is serving and fails at the connect instead of reporting that
/// no such session exists.
#[test]
fn dropping_the_last_owner_unpublishes_the_session() {
    isolate_runtime_dir();
    let sid = "withdrawn";
    drop(Instance::start(sid, Behaviour::Serving));
    assert!(
        !maverick_sys::identity::sock_path(sid).exists(),
        "socket must go"
    );
    assert!(
        maverick_sys::identity::read_meta(sid).is_none(),
        "record must not outlive its owner"
    );
    assert!(
        maverickctl::discover::find_by_name(sid).is_none(),
        "an unowned session must not still be selectable"
    );
}

/// Withdrawal is per session, not per owner: retiring one fixture must leave
/// its neighbour serving.
#[test]
fn retiring_one_instance_leaves_another_serving() {
    isolate_runtime_dir();
    let retired = Instance::start("retired", Behaviour::Serving);
    let kept = Instance::start("kept", Behaviour::Serving);
    drop(retired);
    assert!(maverick_sys::identity::read_meta("retired").is_none());
    assert!(maverick_sys::identity::read_meta("kept").is_some());
    assert!(maverick_sys::identity::sock_path("kept").exists());
    assert!(
        maverickctl::client::ping("kept").is_ok(),
        "the surviving fixture is still answering"
    );
    assert!(maverickctl::discover::find_by_name("kept").is_some());
    drop(kept);
    assert!(maverick_sys::identity::read_meta("kept").is_none());
}
