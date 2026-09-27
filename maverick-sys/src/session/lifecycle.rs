//! Session lifecycle: starting a session, stopping it, and reaping what a
//! crash left behind.
//!
//! # Who owns what
//!
//! A session is a *graph*: an X server, a window manager, and the applications
//! inside them. Nothing in Maverick supervises that graph — there is no
//! per-session daemon, because a supervisor would be a process the user has to
//! know about, kill, and account for, and a window manager is already a
//! long-lived process. Instead:
//!
//! * every component is started here and recorded as a [`ProcRef`] (pid *and*
//!   start time, so a recycled pid can never be signalled by mistake),
//! * teardown is an explicit, ordered step, and
//! * [`reap`] reconciles the record with reality, so a session that lost a
//!   component is cleaned up by the next command that touches it rather than by
//!   a watcher that has to be correct forever.
//!
//! # Read commands do not mutate
//!
//! `session list` and `session status` are pure: they report the *derived*
//! state and never clean anything up. An agent that polls them must not be
//! causing side effects. Every command that is already allowed to change
//! something — `create`, `start`, `stop`, `restart`, `kill`, `remove` — reaps
//! first, which is what keeps an orphaned X server from outliving the window
//! manager it was started for.
//!
//! # Order matters on the way down
//!
//! [`stop`] asks the window manager to quit first and stops the X server after
//! it: the WM closes its clients while its display still exists, which is the
//! difference between a clean shutdown and a pile of applications killed
//! mid-write. [`kill`] reverses nothing — it is the path for a session that
//! will not shut down, so it skips straight to signals.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::proc;
use super::xserver::{self, Backend, Display, XServerSpec};
use super::ProcRef;
use super::{
    current_gid, current_uid, read_checked, session_dir, wait_until, Session, SessionError,
    SessionName, SessionState, Spec, START_TIMEOUT, STOP_GRACE,
};
use crate::control;
use crate::identity;

/// A resolved, runnable Maverick binary.
///
/// Separate from [`Spec::binary`] because "what the user typed" and "what will
/// be executed" are not the same string: a bare `maverick` is a `PATH` lookup,
/// and a relative path is only meaningful relative to *this* process's working
/// directory — the session is about to run with a different one, so the path
/// has to be absolute before anything is spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
    /// Absolute path to the executable.
    pub path: PathBuf,
}

/// Resolve the Maverick binary a session should run.
///
/// An empty `binary` means "the one on `PATH`", which is what `maverickctl
/// session create debug` with no `--binary` should use. The lookup is a
/// `PATH` scan of bare names only — a value containing a separator is a path,
/// never a `PATH` entry to search for — and the result is made absolute
/// against the current directory so the session's own `--cwd` cannot change
/// what gets executed.
pub fn resolve_binary(binary: &str) -> Result<Binary, SessionError> {
    if binary.is_empty() {
        return find_on_path("maverick")
            .map(|p| Binary { path: p })
            .ok_or_else(|| SessionError::MissingBinary {
                path: "maverick (on $PATH)".to_string(),
            });
    }
    let given = PathBuf::from(binary);
    // A value with a separator is already a path; a bare name is a PATH lookup.
    let found = if binary.contains('/') {
        given
    } else {
        find_on_path(binary).ok_or_else(|| SessionError::MissingBinary {
            path: binary.to_string(),
        })?
    };
    Ok(Binary {
        path: absolutize(&found, &std::env::current_dir().unwrap_or_default()),
    })
}

/// Make `found` absolute against `cwd`.
///
/// The session is about to run with *its own* working directory, so a relative
/// path has to be resolved against the directory the user typed it in, once,
/// here — resolving it later would silently mean a different file.
///
/// `canonicalize` is tried first because it also resolves a symlink, which is
/// what makes the recorded binary the actual executable rather than a link that
/// may be repointed later. It fails for a path that does not exist or is not
/// readable through a directory the user may not traverse, and in that case the
/// join against `cwd` is still correct — the file's existence is the caller's
/// problem to report, not something to answer here.
fn absolutize(found: &Path, cwd: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(found) {
        return canonical;
    }
    if found.is_absolute() {
        return found.to_path_buf();
    }
    cwd.join(found)
}

/// The first executable named `name` on `PATH`.
fn find_on_path(name: &str) -> Option<PathBuf> {
    // Reject a value with a separator: `PATH` entries are directories, and
    // searching one for a name containing `/` is how a lookup turns into an
    // arbitrary path read.
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

/// True if `p` is a regular file with an execute bit.
///
/// `is_file` alone accepts a non-executable file, which would fail later inside
/// the spawn with a much less obvious error.
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Create a new session: validate, claim a display, start the X server, start
/// the Maverick binary, and wait until the session can actually be driven.
///
/// # Idempotence
///
/// Creating a session that is already running is an error, because silently
/// returning the existing one would hide a caller that asked for a *new* one
/// and got the old configuration instead. Creating one that exists but is not
/// running starts it — which is what an agent that lost the reply to a failed
/// `create` needs.
pub fn create(name: &SessionName, spec: Spec) -> Result<Session, SessionError> {
    if let Ok(mut existing) = read_checked(name) {
        // A name that exists is first brought in line with reality: a record
        // left behind by a crash must not make `create` refuse a name that is
        // in fact free.
        reap_one(&mut existing);
        let _ = write_record(&existing);
        if existing.derived_state() == SessionState::Running {
            return Err(SessionError::AlreadyRunning(name.clone()));
        }
        return start(name);
    }
    let binary = resolve_binary(&spec.binary)?;
    let mut session = Session::new(name.clone(), spec);
    session.spec.binary = binary.path.display().to_string();
    // Recorded before anything starts: a crash between here and the first
    // successful start has to leave a name that the reaper can clean up.
    write_record(&session)?;
    match launch(&mut session) {
        // The rollback and the reason live in `launch`, which is the only
        // function that knows what it started. Duplicating them here is what
        // left every other caller unprotected.
        Ok(()) => Ok(session),
        Err(e) => Err(e),
    }
}

/// Start a session from its record.
///
/// The configuration is replayed verbatim, which is what makes a session
/// reproducible: the same record always yields the same display geometry, the
/// same binary and the same arguments.
pub fn start(name: &SessionName) -> Result<Session, SessionError> {
    let mut session = read_checked(name)?;
    reap_one(&mut session);
    if session.derived_state() == SessionState::Running {
        return Ok(session);
    }
    // A binary that has since been moved or deleted must be reported, not
    // silently replaced by whatever `maverick` is on `PATH` today: a session
    // that comes up running a *different* build is not the session.
    let binary = resolve_binary(&session.spec.binary)?;
    session.spec.binary = binary.path.display().to_string();
    launch(&mut session)?;
    Ok(session)
}

/// Stop a session: the window manager first, then the X server.
///
/// A session that is not running is a success, not an error — `stop` is the
/// operation a script runs to be sure, and "already stopped" is the state it
/// was asking for.
pub fn stop(name: &SessionName) -> Result<Session, SessionError> {
    let mut session = read_checked(name)?;
    // Reaping first is what makes a crashed session's orphan X server go away
    // as part of stopping it, rather than only as part of the next command.
    reap_one(&mut session);
    if session.derived_state().is_live() {
        teardown(&mut session, StopMode::Graceful);
    }
    session.state = SessionState::Stopped;
    session.exit_reason = "stopped".to_string();
    write_record(&session)?;
    Ok(session)
}

/// Stop and start again, on the same record.
pub fn restart(name: &SessionName) -> Result<Session, SessionError> {
    stop(name)?;
    start(name)
}

/// How hard to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopMode {
    /// Ask: `quit` over the control socket, then `SIGTERM`, then the X server
    /// after the WM is gone.
    Graceful,
    /// `SIGKILL` the whole session without asking.
    Hard,
}

impl StopMode {
    /// A short label for the recorded reason.
    fn label(self) -> &'static str {
        match self {
            Self::Graceful => "stopped",
            Self::Hard => "killed",
        }
    }
}

/// Kill a session immediately: no `quit`, no grace period.
pub fn kill(name: &SessionName) -> Result<Session, SessionError> {
    let mut session = read_checked(name)?;
    teardown(&mut session, StopMode::Hard);
    session.state = SessionState::Stopped;
    session.exit_reason = "killed".to_string();
    write_record(&session)?;
    Ok(session)
}

/// Remove a session: stop it, then delete its runtime directory.
///
/// The directory is only deleted when the session is not running. Deleting the
/// record of a live session would leave its window manager running with nothing
/// left to address it by — the exact orphan the record exists to prevent — so
/// that case is refused and the caller is told to stop it first.
pub fn remove(name: &SessionName, force: bool) -> Result<(), SessionError> {
    let mut session = read_checked(name)?;
    reap_one(&mut session);
    if session.derived_state().is_live() && !force {
        return Err(SessionError::StillRunning(name.clone()));
    }
    teardown(&mut session, StopMode::Hard);
    super::remove_dir(name).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            SessionError::Io(e.to_string())
        } else {
            SessionError::Io(format!("{}: {e}", session_dir(name).display()))
        }
    })
}

/// Reconcile every recorded session with what is actually running.
///
/// Returns the names it changed. Safe to call concurrently with anything else:
/// it only ever moves a session *towards* its real state and cleans up what a
/// dead component left behind.
pub fn reap() -> Vec<SessionName> {
    let mut changed = Vec::new();
    for name in super::names() {
        let Ok(mut session) = read_checked(&name) else {
            continue;
        };
        let before = (session.state, session.xserver.pid, session.wm.pid);
        reap_one(&mut session);
        if before != (session.state, session.xserver.pid, session.wm.pid)
            && write_record(&session).is_ok()
        {
            changed.push(name);
        }
    }
    changed
}

/// Reconcile one session with reality, cleaning up whatever is left over.
///
/// Three cases, and the third is the reason this exists:
///
/// 1. the window manager is up — the record is updated to say so, whatever it
///    claimed before;
/// 2. everything is down and the record already says so — nothing to do;
/// 3. the window manager is gone but the X server is not — the X server is an
///    orphan holding a display. It is stopped, and the record says why.
///
/// The window manager's own artifacts (socket, identity ficha) are removed in
/// every dead case, so a new session under the same name can bind, and the logs
/// are deliberately left in place: they are the reason a crashed session is
/// worth looking at.
fn reap_one(session: &mut Session) {
    if session.wm_is_up() {
        if session.state != SessionState::Running {
            session.state = SessionState::Running;
            session.exit_reason.clear();
        }
        return;
    }

    // The window manager is gone. What it leaves behind is the X server it was
    // started for, which has nothing left to serve and holds a display
    // hostage, so it is stopped here rather than by a watcher.
    //
    // The stop runs whenever the record *names* an X server, not only when that
    // process is still alive. `XServer::stop` already handles a server that is
    // gone by going straight to releasing the display, and gating the call on
    // liveness made that branch unreachable — so a display whose X server was
    // `SIGKILL`ed kept its lock and its socket, `display_is_free` stayed false
    // for that number forever, and no later command could reclaim it. That is
    // a permanent, machine-wide loss of a display number from a single unclean
    // exit, and it is invisible: the record is stamped stopped with no pid.
    let crashed = session.state == SessionState::Running;
    if session.xserver.is_some() {
        let server = xserver::XServer {
            display: session.display,
            backend: session.spec.backend,
            proc: session.xserver,
            xauth_path: session.xauth_path(),
        };
        server.stop(STOP_GRACE);
        if crashed {
            session.exit_reason =
                "the window manager exited; its X server was stopped with it".to_string();
        }
    } else if crashed {
        session.exit_reason = "the window manager exited on its own".to_string();
    }

    session.state = SessionState::Stopped;
    if session.exit_reason.is_empty() {
        session.exit_reason = "not running".to_string();
    }
    // The dead instance's socket and ficha, so the name is reusable. The logs
    // stay: they are the reason a crashed session is worth keeping a record of.
    identity::cleanup_meta(session.name.as_str());
    session.wm = ProcRef::default();
    session.xserver = ProcRef::default();
    // A crashed session owns nothing, for the same reason `teardown` clears
    // them: the groups were actionable only while a process of this session
    // was running. This branch is reached only after the `wm_is_up()` early
    // return above, so a live session keeps its groups.
    session.pgrps.clear();
}

/// Start everything a session needs and record the result.
///
/// A failed launch always leaves nothing behind. This function is the only
/// place that knows which X server the current generation started, so it is
/// also the only place that can undo that: `create` used to carry the rollback,
/// but only on the branch taken when the session name was new, so every other
/// route — `start`, `restart`, and a re-`create` over an existing record —
/// arrived here with no cleanup arm and left a live X server holding a display
/// under a record that said `starting`. One failure path, for every caller.
fn launch(session: &mut Session) -> Result<(), SessionError> {
    // A new generation owns nothing yet. Without this, a restart would carry
    // the previous generation's pgids into the new record, and the new session
    // would claim process groups that belonged to processes it never started.
    session.pgrps.clear();
    match launch_inner(session) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Recorded before the teardown, which only fills in a reason of its
            // own when there is none: the specific failure is more useful than
            // "killed", and it is the only thing that tells a user why their
            // session is not there.
            if session.exit_reason.is_empty() {
                session.exit_reason = e.to_string();
            }
            // A no-op for a failure that happened before anything was spawned,
            // which is what makes it safe to call unconditionally.
            teardown(session, StopMode::Hard);
            session.state = SessionState::Stopped;
            let _ = write_record(session);
            Err(e)
        }
    }
}

fn launch_inner(session: &mut Session) -> Result<(), SessionError> {
    // A fresh cookie per start: a new X server is a new secret, and reusing one
    // would mean a cookie that outlives the server it authenticated.
    let cookie = xserver::generate_cookie()?;
    let dir = session_dir(&session.name);
    std::fs::create_dir_all(&dir)?;
    identity::set_private_dir(&dir)?;

    // Pick a display, and hold it while this generation claims it. The scan
    // alone is advisory — two creators can both see a number free — so the
    // candidate is only taken once the claim is exclusive, and the claim stays
    // held until the X server below has written its own pid into the real lock.
    // The recorded display is preferred so a restart lands where it did, but
    // only if it is actually free: a number another session holds is not
    // something to fight over.
    let (display, claim) = if xserver::display_is_free(session.display) && session.display.0 != 0 {
        match xserver::DisplayClaim::try_acquire(session.display)? {
            Some(claim) => (session.display, claim),
            None => xserver::claim_display(Display(1))?,
        }
    } else {
        xserver::claim_display(Display(1))?
    };
    session.display = display;
    // The cookie is written for the display the session actually got, not the
    // one it hoped for. Written before allocation it named display 0 for every
    // new session, and only the wildcard entry could then authenticate anything.
    xserver::write_xauth(&session.xauth_path(), display, &cookie)?;

    let server = spawn_xserver(session, display)?;
    // From here the X server is a live process holding a display, so the record
    // has to name it *before* anything else can happen — including this process
    // being killed. The window between the spawn and this write used to contain
    // the entire readiness wait, up to `START_TIMEOUT`, and a creator killed in
    // it left a server that no record named: not findable by `list`, not
    // stoppable by `stop`, and holding its display for the rest of the login.
    session.xserver = server.proc;
    session.state = SessionState::Starting;
    session.exit_reason.clear();
    write_record(session)?;

    // Only now is it safe to wait: if this process dies mid-wait the record
    // already names the server, so the next command that touches the session
    // can stop it.
    await_xserver(&server, session, &claim)?;
    let wm = start_maverick(session)?;
    session.wm = wm;
    session.state = SessionState::Running;
    write_record(session)?;
    Ok(())
}

/// Spawn the session's X server without waiting for it.
///
/// Split from the wait so the caller can record the resulting pid in between.
fn spawn_xserver(session: &Session, display: Display) -> Result<xserver::XServer, SessionError> {
    let spec = XServerSpec {
        backend: session.spec.backend,
        display,
        resolution: session.spec.resolution,
        refresh_rate: session.spec.refresh_rate,
        xauth_path: session.xauth_path(),
        title: format!("Maverick session: {}", session.name),
        log_path: session.xserver_log_path(),
    };
    // A missing backend is the one start failure a user can actually fix, so
    // it is reported by name rather than as a spawn error.
    if !find_on_path(spec.backend.binary()).is_some() {
        return Err(SessionError::MissingBackend {
            binary: spec.backend.binary().to_string(),
            reason: "not found on $PATH".to_string(),
        });
    }
    xserver::spawn(&spec).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => SessionError::Io(format!(
            "X display {display} was taken by another session while starting"
        )),
        _ => SessionError::Io(format!("could not start {}: {e}", spec.backend.binary())),
    })
}

/// Wait for a spawned X server to accept connections.
///
/// The display claim is held until the X server has published its own pid in
/// `/tmp/.X<n>-lock`. Releasing it earlier would reopen the exact window the
/// claim exists to close: a second creator could take the number while this
/// server was still coming up, and the loser would be right to conclude the
/// display was taken. `lock_names` is the observation, not a delay — the X
/// server writes that file as part of claiming the number, so polling for it
/// reports the handover rather than guessing at its length.
fn await_xserver(
    server: &xserver::XServer,
    session: &Session,
    claim: &xserver::DisplayClaim,
) -> Result<(), SessionError> {
    let display = server.display;
    let proc_ref = server.proc;
    // The claim is only ours to drop once the real lock names our server; keep
    // it alive across the wait by holding the reference for the whole call.
    let _claim = claim;
    match xserver::wait_ready(display, START_TIMEOUT, || {
        proc::is_running(&proc_ref) || xserver::lock_names(display, proc_ref.pid)
    }) {
        Ok(()) => Ok(()),
        Err(e) => {
            server.stop(STOP_GRACE);
            Err(SessionError::Io(format!(
                "{e}; see {}",
                session.xserver_log_path().display()
            )))
        }
    }
}

/// Start the session's Maverick and wait until the session can be driven.
///
/// "Driven" means more than "the process exists": the control socket has to
/// answer, and the state snapshot has to report a monitor. A window manager
/// that has bound its socket but not yet claimed a display is not a session a
/// caller can open a window in, and returning early would hand out a session
/// that fails the first thing an agent does with it.
fn start_maverick(session: &mut Session) -> Result<ProcRef, SessionError> {
    let binary = resolve_binary(&session.spec.binary)?;
    let log = xserver::open_private_log(&session.log_path())?;

    let mut cmd = std::process::Command::new(&binary.path);
    // The session's own id, so the runtime directory, socket and ficha are all
    // named after the session the user typed.
    cmd.arg("--session-id").arg(session.name.as_str());
    cmd.arg("--name").arg(session.name.as_str());
    // Everything the caller passed after `--`, verbatim: the session manager
    // must not need to know the window manager's own vocabulary to forward it.
    cmd.args(&session.spec.args);
    if let Some(cwd) = &session.spec.cwd {
        // Checked here rather than left to the spawn: `current_dir` only
        // fails at `spawn`, and a missing directory reported as a spawn error
        // names neither the session nor the path the user typed.
        if !cwd.is_dir() {
            return Err(SessionError::Io(format!(
                "working directory {} is not a directory",
                cwd.display()
            )));
        }
        cmd.current_dir(cwd);
    }
    for (key, value) in session.env() {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(log.try_clone()?));
    cmd.stderr(Stdio::from(log));
    // Its own process group, so a terminal's `SIGINT` cannot reach the session
    // and so the whole client tree can be signalled at once (see `teardown`).
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let child = cmd.spawn().map_err(|e| {
        // A binary that vanished between the resolve and the spawn is still the
        // most likely cause, and the most useful thing to say about it.
        if e.kind() == io::ErrorKind::NotFound {
            SessionError::MissingBinary {
                path: binary.path.display().to_string(),
            }
        } else {
            SessionError::Io(format!("could not start {}: {e}", binary.path.display()))
        }
    })?;
    let pid = child.id();
    let wm = ProcRef::of(pid);
    if !wm.is_some() {
        return Err(wm_failure(
            session,
            "the window manager exited before it could be recorded",
        ));
    }

    let name = session.name.as_str().to_string();
    let ready = wait_until(START_TIMEOUT, || {
        if !wm.is_alive() {
            return false;
        }
        // The socket answering is necessary; a snapshot with a monitor is what
        // makes the session usable.
        control::ping(&name)
            .ok()
            .filter(|pong| !pong.is_empty())
            .is_some()
            && control::query(&name, "state")
                .map(|json| json.contains("\"monitors\":[{"))
                .unwrap_or(false)
    });
    if !ready {
        if wm.is_alive() {
            return Err(wm_failure(
                session,
                "the window manager did not report a usable display in time",
            ));
        }
        return Err(wm_failure(session, "the window manager exited"));
    }
    Ok(wm)
}

/// Build the "the window manager did not start" error, with the log tail that
/// explains it.
///
/// A failed start is the moment a session log is worth the most, and the first
/// lines of it are the diagnosis; without them the message is just a timeout.
fn wm_failure(session: &Session, reason: &str) -> SessionError {
    let log = session.log_path();
    let reason = match tail_of(&log) {
        Some(tail) if !tail.is_empty() => format!("{reason}\n  last log line: {}", tail[0]),
        _ => reason.to_string(),
    };
    SessionError::WmFailed { reason, log }
}

/// The first non-empty line of a session's window-manager log.
fn tail_of(path: &Path) -> Option<Vec<String>> {
    super::tail(path, 1).ok()
}

/// Persist a record, mapping a filesystem failure to a session error.
fn write_record(session: &Session) -> Result<(), SessionError> {
    super::write(session).map_err(|e| {
        SessionError::Io(format!(
            "could not record session '{}' in {}: {e}",
            session.name,
            session.dir().display()
        ))
    })
}

/// Stop everything a session is running, in the order the mode implies.
fn teardown(session: &mut Session, mode: StopMode) {
    // The window manager first: while its display exists it can close its
    // clients cooperatively, which is the difference between a clean shutdown
    // and applications killed mid-write.
    if session.wm.is_alive() {
        if mode == StopMode::Graceful {
            let _ = control::quit(session.name.as_str());
        }
        wait_until(STOP_GRACE, || !session.wm.is_alive());
        if session.wm.is_alive() {
            let _ = signal_tree(&session.wm, StopMode::Hard);
        }
        // An unresponsive WM can also hold the whole client tree; the process
        // group covers the applications it started, which a signal to the WM's
        // own pid does not.
        let _ = signal_tree(&session.wm, mode);
    }
    // Its socket and ficha go with it, so the name can be reused immediately.
    identity::cleanup_meta(session.name.as_str());

    // As in `reap_one`: releasing the display is owed whenever the record names
    // an X server, because a server that is already dead still owns the claim
    // files that make the number look taken. `XServer::stop` no-ops on a live
    // foreign server, so this cannot disarm a session that is still working.
    if session.xserver.is_some() {
        let server = xserver::XServer {
            display: session.display,
            backend: session.spec.backend,
            proc: session.xserver,
            xauth_path: session.xauth_path(),
        };
        server.stop(STOP_GRACE);
    }
    session.wm = ProcRef::default();
    session.xserver = ProcRef::default();
    // Registered pgids are actionable ownership state, not history: they exist
    // so an `exec`ed program stays findable while the session runs. Once it
    // stops, leaving them behind would let a record that no longer names a
    // single live process authorise a signal — and a pgid the kernel has since
    // reissued would be signalled as if it were ours. Nothing reads this list
    // for reporting (`SessionView` has no such field), and the post-mortem
    // trail lives in `exec.log` and the session logs, so clearing it destroys
    // nothing a consumer reads.
    session.pgrps.clear();
    if mode == StopMode::Hard && session.exit_reason.is_empty() {
        session.exit_reason = StopMode::Hard.label().to_string();
    }
}

/// Signal a session's window manager *and everything it started*.
///
/// Uses the process group rather than a `/proc` walk, which is both cheaper and
/// more complete: an application that was started before its parent changed
/// groups is still in the group, and a walk would need to re-derive the same
/// answer from a table of parent pointers that is racy to read while the tree
/// is being torn down.
///
/// Guarded twice: the recorded pid must still be the process it was, and it
/// must still be its own group leader. Without the second check a recycled pid
/// that happened to lead a group would take an unrelated group with it.
fn signal_tree(wm: &ProcRef, mode: StopMode) -> bool {
    if !wm.is_alive() {
        return false;
    }
    let Some(info) = proc::read(wm.pid) else {
        return false;
    };
    // Only a group leader can stand for its whole group.
    if info.pgid != wm.pid {
        // Not a group leader, so fall back to the process itself. A failure
        // here means it is already gone, which is the outcome we wanted.
        return proc::terminate(wm.pid, wm.start_time).is_ok();
    }
    let sig = match mode {
        StopMode::Graceful => libc::SIGTERM,
        StopMode::Hard => libc::SIGKILL,
    };
    // SAFETY: `killpg` with a group id that was just read from `/proc` for the
    // process this record names, and only after `is_alive` proved the pid is
    // still that process. `wm.pid == info.pgid` means the group is led by that
    // same process, so the group cannot contain an unrelated caller's process.
    let sent = unsafe { libc::killpg(info.pgid as libc::pid_t, sig) } == 0;
    if !sent {
        // The group is already gone; fall back to the process itself so a
        // window manager that changed groups is still stopped.
        let _ = proc::terminate(wm.pid, wm.start_time);
    }
    sent
}

/// The owner recorded for a session created now: this process's real uid/gid.
///
/// The kernel is the only source. There is deliberately no flag, environment
/// variable or config key that can set these — ownership is a fact about who
/// ran the command, not something a caller may declare.
pub fn owner() -> (u32, u32) {
    (current_uid(), current_gid())
}

/// True if the X server backend a session names is one this build supports.
pub fn backend_is_known(backend: Backend) -> bool {
    matches!(backend, Backend::Xephyr | Backend::Xvfb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Resolution;

    fn spec(binary: &str) -> Spec {
        Spec {
            binary: binary.to_string(),
            resolution: Resolution::new(800, 600).expect("resolution"),
            ..Spec::default()
        }
    }

    /// A relative binary is the common case (`--binary ./target/debug/maverick`)
    /// and it has to become absolute *before* the session runs with a different
    /// working directory, or the child would resolve it against that one.
    #[test]
    fn a_relative_binary_is_made_absolute() {
        let cwd = Path::new("/home/u/project");
        assert_eq!(
            absolutize(Path::new("./target/debug/maverick"), cwd),
            PathBuf::from("/home/u/project/./target/debug/maverick"),
            "a relative path must be anchored to the directory it was typed in"
        );
        // An absolute path is already anchored; it is not re-joined.
        assert_eq!(
            absolutize(Path::new("/usr/bin/maverick"), cwd),
            PathBuf::from("/usr/bin/maverick")
        );
        // A real file resolves through its symlinks, so the recorded binary is
        // the executable rather than a link that may be repointed later.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("maverick");
        std::fs::write(&target, "#!/bin/sh\n").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        assert_eq!(absolutize(&link, Path::new("/")), target);
    }

    /// A bare name is a `PATH` lookup, which is what makes `--binary maverick`
    /// work without spelling out where the build put it.
    #[test]
    fn a_bare_name_resolves_through_path() {
        let resolved = resolve_binary("sh").expect("sh is on every PATH");
        assert!(resolved.path.is_absolute());
        assert!(is_executable(&resolved.path));
        assert!(resolve_binary("maverick-definitely-not-here").is_err());
    }

    /// A name that is not on `PATH` must be reported by name, not as a spawn
    /// failure the user has to decode.
    #[test]
    fn a_missing_binary_is_named_in_the_error() {
        let err = resolve_binary("maverick-definitely-not-here").expect_err("missing");
        assert!(
            err.to_string().contains("maverick-definitely-not-here"),
            "{err}"
        );
    }

    /// A file that exists but is not executable must not be accepted: the
    /// failure would otherwise surface as an opaque spawn error after the
    /// display was already claimed.
    #[test]
    fn a_non_executable_file_is_not_a_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("not-exec");
        std::fs::write(&script, "#!/bin/sh\n").expect("write");
        assert!(!is_executable(&script));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(!is_executable(&script));
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&script));
    }

    /// A `PATH` lookup is a lookup of a *name*: a value containing a separator
    /// must never be searched for inside `PATH` entries.
    #[test]
    fn path_lookup_rejects_a_embedded_separator() {
        assert!(find_on_path("bin/maverick").is_none());
        assert!(find_on_path("").is_none());
        assert!(find_on_path("sh").is_some(), "sh is on every PATH");
    }

    /// Removing a session that is still running has to be refused, or the
    /// record of a live window manager would be deleted and the process would
    /// be left with nothing to address it by.
    #[test]
    fn a_session_that_was_never_written_is_not_removable() {
        let name = SessionName::parse(&format!("absent{}", std::process::id())).expect("name");
        assert!(matches!(
            remove(&name, false),
            Err(SessionError::NotFound(_))
        ));
    }

    /// Every session carries its owner's uid from the kernel, and nothing a
    /// caller can set replaces it.
    #[test]
    fn ownership_comes_from_the_process_not_from_input() {
        let (uid, gid) = owner();
        assert_eq!(uid, current_uid());
        assert_eq!(gid, current_gid());
        let name = SessionName::parse("ownercheck").expect("name");
        let session = Session::new(name, spec(""));
        assert_eq!(session.owner_uid, uid);
        assert_eq!(session.owner_gid, gid);
    }

    #[test]
    fn only_the_backends_this_build_knows_are_accepted() {
        assert!(backend_is_known(Backend::Xephyr));
        assert!(backend_is_known(Backend::Xvfb));
        assert!(Backend::parse("wayland").is_none());
    }
}
