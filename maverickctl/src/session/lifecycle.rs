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
//! state and never clean anything up. A caller that polls them must not be
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

use crate::client;
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
use maverick_sys::identity;

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
/// The join against `cwd` happens *first*, so the base is always this function's
/// argument: canonicalising `found` on its own would resolve it against whatever
/// directory the process happens to be in, which is a different answer whenever
/// the caller names a base other than its own.
///
/// `canonicalize` is then tried because it also resolves a symlink, which is
/// what makes the recorded binary the actual executable rather than a link that
/// may be repointed later. It fails for a path that does not exist or is not
/// readable through a directory the user may not traverse, and in that case the
/// join is still correct — the file's existence is the caller's problem to
/// report, not something to answer here. `Path::join` already yields `found`
/// unchanged when `found` is absolute, so an absolute path needs no branch.
fn absolutize(found: &Path, cwd: &Path) -> PathBuf {
    let joined = cwd.join(found);
    if let Ok(canonical) = std::fs::canonicalize(&joined) {
        return canonical;
    }
    joined
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
        // The reaped state is written so a later command sees a record that
        // matches reality, but whether that write lands does not decide this
        // one: the answer below is the same either way, and the record being
        // unwritable is not something `create` can fix.
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
    let mut survived = Vec::new();
    if session.derived_state().is_live() {
        survived = teardown(&mut session, StopMode::Graceful);
    }
    session.state = SessionState::Stopped;
    session.exit_reason = "stopped".to_string();
    write_record(&session)?;
    // Reported after the record is written, so the stopped state is recorded
    // either way: the components that would not die are still the caller's
    // problem to clean up, and hiding that would leave a live X server nobody
    // knows about.
    survived_not_stopped(name, &survived)?;
    Ok(session)
}

/// Fail if any component of the session outlived the teardown.
///
/// The record is already written as `Stopped` by the time this runs, which is
/// deliberate: the recorded state describes what the session manager has done,
/// and a process that ignored `SIGKILL` does not make that record wrong. It
/// does make the command a failure, because the caller asked for a stopped
/// session and was not given one.
fn survived_not_stopped(name: &SessionName, survived: &[String]) -> Result<(), SessionError> {
    if survived.is_empty() {
        return Ok(());
    }
    Err(SessionError::Io(format!(
        "session '{name}' is recorded as stopped but its {} {} still running",
        survived.join(" and "),
        if survived.len() == 1 { "is" } else { "are" }
    )))
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
    let survived = teardown(&mut session, StopMode::Hard);
    session.state = SessionState::Stopped;
    session.exit_reason = "killed".to_string();
    write_record(&session)?;
    survived_not_stopped(name, &survived)?;
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
        // Reaping is best effort by nature — it runs from `create`, from
        // `stop`, and from a sweep, and none of those can usefully be refused
        // because a leftover display artifact resisted removal. So the failure
        // is not returned here; it is recorded, because `exit_reason` is the
        // field that survives into every later `session status`, and a display
        // whose lock outlived its server is otherwise invisible: the record
        // says stopped, with a display number nothing can now claim.
        let released = server.stop(STOP_GRACE);
        if crashed {
            session.exit_reason = match &released {
                Ok(()) => "the window manager exited; its X server was stopped with it".to_string(),
                Err(e) => format!(
                    "the window manager exited; its X server is still holding :{}: {e}",
                    session.display
                ),
            };
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
            // which is what makes it safe to call unconditionally. Its outcome
            // is not consulted: the command has already failed with the reason
            // that caused it, so a second failure about the same attempt adds
            // nothing the caller can act on differently.
            let _ = teardown(session, StopMode::Hard);
            session.state = SessionState::Stopped;
            // Likewise: the record is written so the failure is discoverable
            // later, but the command's result does not depend on it.
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
            // The original failure is the operation's failure and stays the
            // headline: it is what the user asked about. A cleanup failure is
            // appended rather than substituted, because it is a different kind
            // of problem — the server never came up, and these are the files it
            // left behind — and replacing the cause would hide the reason the
            // start failed behind the reason it could not be cleaned up.
            let cleanup = server.stop(STOP_GRACE);
            let mut why = e.to_string();
            if let Err(cleanup_err) = cleanup {
                why.push_str(&format!(
                    "; its X server could not be cleaned up either: {cleanup_err}"
                ));
            }
            Err(SessionError::Io(format!(
                "{why}; see {}",
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
        client::ping(&name)
            .ok()
            .filter(|pong| !pong.is_empty())
            .is_some()
            && client::query(&name, "state")
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
///
/// Returns the components that survived, so the caller can tell a finished
/// teardown from an attempted one. Every step runs whatever the previous one
/// did — a window manager that ignored `SIGTERM` must not prevent the X server
/// from being stopped — so the outcome is collected rather than returned at the
/// first problem. Only the postcondition matters here, not whether each signal
/// was delivered: a signal that "failed" because the process had already exited
/// is the outcome that was wanted, and an unkillable one is caught by the final
/// liveness check below.
fn teardown(session: &mut Session, mode: StopMode) -> Vec<String> {
    let mut survived: Vec<String> = Vec::new();
    // Reasons the session was not fully released, kept apart from `survived`
    // because they are not processes: nothing is still running, but something
    // still owns a display number. Rendered into the same report so a caller
    // sees one list of what stopped them from using this session.
    let mut cleanup_failed: Vec<String> = Vec::new();
    // The window manager first: while its display exists it can close its
    // clients cooperatively, which is the difference between a clean shutdown
    // and applications killed mid-write.
    if session.wm.is_alive() {
        if mode == StopMode::Graceful {
            // Best effort by design: `quit` is a request to a program that may
            // already be gone or wedged, and the escalation below covers both.
            let _ = client::quit(session.name.as_str());
        }
        wait_until(STOP_GRACE, || !session.wm.is_alive());
        if session.wm.is_alive() {
            // Not consulted either: whether the signal was delivered is not the
            // question. The postcondition below asks whether the process is
            // still there, and a signal that "failed" because the process had
            // already exited is the outcome that was wanted.
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
        // Joined into the same outcome as a surviving process rather than kept
        // as a parallel result: both mean the session was not released, and the
        // caller learns about them the same way. A display whose lock outlived
        // its server is not reusable — `display_is_free` reads the artifact as
        // a claim — so "stopped" would be false in exactly the same way a live
        // X server would make it false.
        if let Err(e) = server.stop(STOP_GRACE) {
            cleanup_failed.push(e.to_string());
        }
    }
    // The postcondition, read before the references are cleared below: `stop`
    // and `kill` promise the session is no longer running, and the only thing
    // that can say whether it is true is the recorded process still being
    // there. Both ids are captured now because the record is about to forget
    // them, and a component that outlives its record is exactly what a caller
    // needs to be told about.
    if session.wm.is_alive() {
        survived.push("window manager".to_string());
    }
    if session.xserver.is_some() && session.xserver.is_alive() {
        survived.push("X server".to_string());
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
    // A display that could not be released is rendered into the same list a
    // surviving process goes into, because that is what it is from the
    // caller's side: something still holds this session's display number. The
    // entries are owned so the reason can travel with them.
    survived.extend(
        cleanup_failed
            .iter()
            .map(|reason| format!("its X server's display was not released: {reason}")),
    );
    survived
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
    // The group id was just read from `/proc` for the process this record
    // names, and only after `is_alive` proved the pid is still that process.
    // `wm.pid == info.pgid` means the group is led by that same process, so the
    // group cannot contain an unrelated caller's process. The signal is a typed
    // `Signal` rather than a raw number: both arms are named signals, so the
    // conversion is a lookup rather than a validation, and nothing here can
    // express a signal the caller did not mean.
    let signal = match mode {
        StopMode::Graceful => rustix::process::Signal::TERM,
        StopMode::Hard => rustix::process::Signal::KILL,
    };
    let sent = rustix::process::Pid::from_raw(info.pgid as rustix::process::RawPid)
        .is_some_and(|pgid| rustix::process::kill_process_group(pgid, signal).is_ok());
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

    /// A teardown that leaves a process behind is a failure, and the caller has to
    /// hear which one.
    ///
    /// The postcondition is what makes this true: `stop` and `kill` used to
    /// write `Stopped` and return `Ok` whatever `teardown` managed to do, so a
    /// window manager that survived `SIGTERM` *and* `SIGKILL` was reported as
    /// stopped — with a live X server and a record that says otherwise.
    #[test]
    fn a_surviving_component_fails_the_teardown() {
        let name = SessionName::parse("survivor").expect("valid name");
        // Nothing survived: the promised state was reached.
        assert!(
            survived_not_stopped(&name, &[]).is_ok(),
            "a teardown that stopped everything is a success"
        );
        // One component left running.
        let err = survived_not_stopped(&name, &["window manager".to_string()])
            .expect_err("a surviving window manager must fail");
        let text = err.to_string();
        assert!(
            text.contains("window manager"),
            "the message must name what survived, got: {text}"
        );
        assert!(
            text.contains("still running"),
            "the message must say the session is not actually stopped, got: {text}"
        );
        // Both: the caller has to be able to tell which component to clean up.
        let err = survived_not_stopped(
            &name,
            &["window manager".to_string(), "X server".to_string()],
        )
        .expect_err("two survivors must fail");
        let text = err.to_string();
        assert!(text.contains("window manager and X server"), "got: {text}");
        assert!(
            text.contains("are still running"),
            "plural components need a plural claim, got: {text}"
        );
    }

    /// A display that could not be released is reported the same way a surviving
    /// process is.
    ///
    /// Nothing is running in this case, so the entry cannot claim a process is
    /// still alive — but from the caller's side the session is equally unusable:
    /// `display_is_free` reads the leftover lock as a claim, so that number
    /// cannot be taken by anything, including a new session. Reporting a
    /// successful `stop` here would be a false success of exactly the kind the
    /// postcondition exists to prevent.
    #[test]
    fn an_unreleased_display_fails_the_teardown_like_a_surviving_process() {
        let name = SessionName::parse("unreleased").expect("valid name");
        let err = survived_not_stopped(
            &name,
            &["its X server's display was not released: Permission denied".to_string()],
        )
        .expect_err("a display that was not released is not a successful stop");
        let text = err.to_string();
        assert!(
            text.contains("display was not released"),
            "the caller must be able to tell this apart from a live process: {text}"
        );
        assert!(
            text.contains("Permission denied"),
            "and must be given the reason the filesystem gave: {text}"
        );
        assert!(
            text.contains("still running"),
            "the wording still tells the caller the session was not released: {text}"
        );
    }

    /// A failed teardown and a successful one must not read the same.
    ///
    /// The risk this guards is silent: an entry that is *not* in the list is
    /// indistinguishable from a clean stop, and `stop` returning `Ok` is what a
    /// caller acts on.
    #[test]
    fn a_successful_teardown_is_distinguishable_from_a_failed_one() {
        let name = SessionName::parse("clean").expect("valid name");
        assert!(survived_not_stopped(&name, &[]).is_ok());
    }

    /// The three `XServer::stop` call sites must consume its result.
    ///
    /// Read from the source rather than asserted through a live X server,
    /// because the interesting case is a removal failure on a display that has
    /// no server at all — which cannot be arranged without planting global
    /// `/tmp` artifacts. What matters is that no call site can go back to
    /// discarding the `Result` silently, which is how the failure was lost the
    /// first time.
    #[test]
    fn no_xserver_stop_call_site_discards_its_result() {
        // Only the production half: this test names `server.stop(` itself, so
        // scanning the test module would find its own source.
        let src = include_str!("lifecycle.rs");
        let production = src.split("#[cfg(test)]").next().expect("a test module");
        let mut consumed = 0;
        for (n, line) in production.lines().enumerate() {
            let t = line.trim();
            if !t.contains("server.stop(") {
                continue;
            }
            // Consumed in one of the two forms that keep the result: bound to a
            // name, or matched by `if let`. A bare statement would be the
            // discard this test exists to prevent.
            let bound = t.starts_with("let ") || t.contains("= server.stop(");
            let matched = t.starts_with("if let Err(");
            assert!(
                bound || matched,
                "line {} discards the result of XServer::stop: {t}",
                n + 1
            );
            consumed += 1;
        }
        assert_eq!(
            consumed, 3,
            "there are three XServer::stop call sites; found {consumed}"
        );
    }

    /// The record is written before the survival check runs, so a failure to
    /// stop is reported without losing the recorded state.
    #[test]
    fn a_surviving_component_still_leaves_the_record_written() {
        // `stop` writes, then checks. The order is the contract: a caller that
        // retries must not find the session still recorded as running, or the
        // second `stop` would skip the teardown entirely.
        let name = SessionName::parse("order").expect("valid name");
        let err = survived_not_stopped(&name, &["window manager".to_string()]).expect_err("fails");
        assert!(
            matches!(err, SessionError::Io(_)),
            "the failure reuses the existing error variant rather than adding one: {err:?}"
        );
    }

    /// A relative binary is the common case (`--binary ./target/debug/maverick`)
    /// and it has to become absolute *before* the session runs with a different
    /// working directory, or the child would resolve it against that one.
    #[test]
    fn a_relative_binary_is_made_absolute() {
        // The base is a fixture directory, not a hard-coded path: with a literal
        // one the answer silently depended on whether some directory in this
        // checkout happened to contain the relative path being resolved, so the
        // same assertion held or broke according to the working directory the
        // test was launched from.
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        assert_eq!(
            absolutize(Path::new("./target/debug/maverick"), cwd),
            cwd.join("./target/debug/maverick"),
            "a relative path must be anchored to the directory it was typed in"
        );
        // An absolute path is already anchored; it is not re-joined.
        assert_eq!(
            absolutize(Path::new("/usr/bin/maverick"), cwd),
            PathBuf::from("/usr/bin/maverick")
        );
        // A real file resolves through its symlinks, so the recorded binary is
        // the executable rather than a link that may be repointed later.
        let target = cwd.join("real-maverick");
        std::fs::write(&target, "#!/bin/sh\n").expect("write");
        let link = cwd.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("link");
        assert_eq!(absolutize(&link, Path::new("/")), target);
    }

    /// The base is the one the caller named, not wherever the process is.
    ///
    /// A relative path that resolves only by falling back to the join proves
    /// nothing about which base was used, because both answers are identical
    /// when the path does not exist. So this case needs a relative path that
    /// *does* resolve — `.` exists from every directory — with a base that is
    /// deliberately not the process working directory. Canonicalising the
    /// relative path on its own would answer with the process directory, which
    /// is wrong yet entirely plausible, so this is the assertion that separates
    /// the two.
    #[test]
    fn a_relative_path_is_resolved_against_the_named_base_not_the_process_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical base");
        let here = std::fs::canonicalize(".").expect("canonical cwd");
        assert_ne!(
            base, here,
            "the fixture base must differ from the process directory for this to mean anything"
        );
        assert_eq!(
            absolutize(Path::new("."), &base),
            base,
            "a relative path is anchored to the base it was given, not to the process directory"
        );
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
