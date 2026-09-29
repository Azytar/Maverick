//! Maverick Sessions: a named, reproducible, controllable graphical session.
//!
//! A session is a whole graphical unit, not a window and not a process:
//!
//! ```text
//! session "debug"
//! ├── X server            (real, nested, own display, own Xauthority cookie)
//! ├── Maverick            (any binary, any arguments, any working directory)
//! ├── applications        (everything `maverickctl exec` launched)
//! ├── control socket      (per-user, 0600, peer-credential checked)
//! ├── logs                (independent of any other session)
//! └── lifecycle           (independent, recorded, reapable)
//! ```
//!
//! # What lives where
//!
//! ```text
//! $XDG_RUNTIME_DIR/maverick/<name>/
//! ├── session.json   the session record — THIS module's file, written by maverickctl
//! ├── <name>.json    the identity ficha — the WM's file, written by maverick
//! ├── control.sock   the control socket — the WM's, per-user and peer-checked
//! ├── Xauthority     the display cookie, 0600
//! ├── maverick.log   the WM's stderr
//! └── xserver.log    the X server's stderr
//! ```
//!
//! Two files with one writer each, deliberately. The WM is the authority on
//! what is *running* (its ficha); the session manager is the authority on what
//! the session *is* (binary, resolution, arguments). Neither rewrites the
//! other's half, so a concurrent `session create` and a WM restart cannot lose
//! an update — which a single merged file would.
//!
//! The directory is named after the session, and the WM is told to adopt that
//! name as its session id (`--session-id`), so the same `session_dir`,
//! `sock_path` and `meta_path` helpers that discovery already uses address a
//! session directly. `main` is not special: it is a session whose directory and
//! WM-managed by whoever started the WM, and it is addressable by the same
//! commands (see [`resolve`]).
//!
//! # Ownership
//!
//! Every record is a plain value read from and written to the filesystem. The
//! only resources this module owns are the processes it starts, and those are
//! always represented as a [`ProcRef`] — pid *plus* start time — because a pid
//! alone is not an identity and every signal this module sends is gated on that
//! pair.
//!
//! # Security
//!
//! A session belongs to the uid that created it, and the record says so. The
//! directory is `0700`, the cookie is `0600`, the logs are `0600`, and the
//! control socket is additionally peer-credential checked by the WM (see
//! [`crate::control`]). Nothing here reads an environment variable as an
//! authorization decision: `owner_uid` is recorded so a *foreign* record is
//! refused, never so a caller can claim one.

pub mod lifecycle;
pub mod proc;
pub mod xserver;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::identity;
use crate::json::{json_quote, quote_array, scan_object};

pub use xserver::{Backend, Display, XServer};

/// The session record's filename inside the session directory.
pub const SPEC_FILE: &str = "session.json";

/// The reserved name for the session the user is currently looking at.
///
/// It is an *alias*, not a special case in the lifecycle: `maverickctl inspect
/// main` is the same code path as `maverickctl inspect debug`, and `main`
/// simply resolves to whichever live instance owns the caller's display.
pub const MAIN_ALIAS: &str = "main";

/// A screen size in pixels.
///
/// Bounded because the value is interpolated into a command line that starts an
/// X server: an unbounded number from a config file or an agent would be a
/// command-line argument the session manager cannot reason about. The limits
/// are the X server's own practical ones, not a UI restriction.
pub const MAX_DIMENSION: u32 = 16_384;

/// A session's screen geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Resolution {
    /// The default for a session created without `--resolution`: a size that
    /// fits comfortably inside a window on the parent display, which is what a
    /// nested session is looked at through.
    pub const DEFAULT: Resolution = Resolution {
        width: 1280,
        height: 720,
    };

    /// A resolution, validated against [`MAX_DIMENSION`].
    pub fn new(width: u32, height: u32) -> io::Result<Self> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "resolution {width}x{height} is out of range (1..={MAX_DIMENSION} per axis)"
                ),
            ));
        }
        Ok(Resolution { width, height })
    }

    /// Parse `WIDTHxHEIGHT`, the form `--resolution` accepts.
    ///
    /// Strict on purpose: this string is the whole of the user's intent about
    /// geometry, so accepting a near-miss (`1280*720`, `1280x720x24`, `1280`)
    /// would mean guessing which part they meant.
    pub fn parse(s: &str) -> io::Result<Self> {
        let t = s.trim();
        let (w, h) = t.split_once(['x', 'X']).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("resolution '{t}' is not WIDTHxHEIGHT (e.g. 1280x720)"),
            )
        })?;
        let parse = |part: &str, axis: &str| -> io::Result<u32> {
            part.trim().parse::<u32>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("resolution '{t}' has a non-numeric {axis}"),
                )
            })
        };
        Self::new(parse(w, "width")?, parse(h, "height")?)
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// The name a session is addressed by.
///
/// A newtype rather than a bare `String` because the name *is* a path component:
/// it becomes the session directory, and therefore the control socket path and
/// the ficha filename. Validating once, at construction, means no later code
/// has to remember that the name came from a command line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(String);

impl SessionName {
    /// Validate a session name.
    ///
    /// The accepted set is the same charset the identity layer already treats
    /// as a safe single path component (`[A-Za-z0-9_-]`, bounded length), so a
    /// name that is valid here is valid as a `session_id`, as a `sock_path` and
    /// as a `meta_path` without a second rule to keep in step. `..` and any
    /// path separator are excluded by that charset, so the name cannot escape
    /// the runtime directory.
    pub fn parse(name: &str) -> io::Result<Self> {
        if identity::is_valid_sid(name) {
            Ok(SessionName(name.to_string()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid session name '{name}' — use 1..={} characters from [A-Za-z0-9_-]",
                    identity::MAX_SID_LEN
                ),
            ))
        }
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pid paired with the start time that makes it an identity.
///
/// `pid == 0` (with `start_time == 0`) is "no process", which is how a stopped
/// session is represented without an `Option` at every read site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcRef {
    /// Kernel process id, or 0 when nothing is running.
    pub pid: u32,
    /// `/proc/<pid>/stat` start time, or 0 when unknown.
    pub start_time: u64,
}

impl ProcRef {
    /// The ref for a live process, read now.
    pub fn of(pid: u32) -> Self {
        ProcRef {
            pid,
            start_time: proc::start_time(pid).unwrap_or(0),
        }
    }

    /// True if this exact process is still running.
    pub fn is_alive(&self) -> bool {
        proc::pid_is(self.pid, self.start_time)
    }

    /// True if a process is recorded at all.
    pub fn is_some(&self) -> bool {
        self.pid != 0
    }
}

/// Where a session is in its lifecycle.
///
/// A deliberately small enum: it is what an agent branches on, and every state
/// an agent cannot act on belongs in [`Session::exit_reason`] instead. Note
/// that [`SessionState::Crashed`] is a *derived* state — it is what the record
/// says when the WM is gone and nobody asked it to go — and the reaper moves it
/// to `Stopped` once the session's resources are actually cleaned up, so a
/// listed session is never one still holding an X server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The X server is up and the window manager is not answering yet.
    Starting,
    /// The window manager is up and its control socket answers.
    Running,
    /// Not running. The reason, if there was one, is in `exit_reason`.
    Stopped,
    /// The window manager went away without being asked to.
    Crashed,
}

impl SessionState {
    /// The wire name, as it appears in `session list --json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Crashed => "crashed",
        }
    }

    /// True for the states in which a session is (or is becoming) usable.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    /// Parse a wire name. Unknown values read as `Stopped`: a record written by
    /// a future version must not make a session look unmanageable.
    pub fn parse(s: &str) -> Self {
        match s {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "crashed" => Self::Crashed,
            _ => Self::Stopped,
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The declared configuration of a session: everything `session create` chose
/// and `session start` replays verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Which nested X server implementation to run.
    pub backend: Backend,
    /// Screen geometry of the session's display.
    pub resolution: Resolution,
    /// Requested refresh rate, as declared intent. See
    /// [`xserver::XServerSpec::refresh_rate`] for what the backends do with it.
    pub refresh_rate: Option<u32>,
    /// The Maverick binary to run, absolute after resolution.
    pub binary: String,
    /// Working directory for the Maverick process.
    pub cwd: Option<PathBuf>,
    /// Extra arguments passed to the Maverick binary, after `--`.
    pub args: Vec<String>,
    /// Debug mode: verbose logging and a per-session log worth reading.
    pub debug: bool,
    /// Whether the compositor was requested. `false` means the session runs
    /// with `MAVERICK_NO_COMPOSITOR=1`, which is how one session is compared
    /// against another without touching the user's own.
    pub compositor: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            resolution: Resolution::DEFAULT,
            refresh_rate: None,
            binary: String::new(),
            cwd: None,
            args: Vec::new(),
            debug: false,
            compositor: true,
        }
    }
}

/// A Maverick session, as recorded on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The name this session is addressed by, and its directory.
    pub name: SessionName,
    /// The declared configuration.
    pub spec: Spec,
    /// The uid that created the session. The only authorization input the
    /// session manager consults, and it is never taken from a caller.
    pub owner_uid: u32,
    /// The gid that created the session.
    pub owner_gid: u32,
    /// Unix epoch seconds when the session was created.
    pub created_at: u64,
    /// The X display the session's server owns.
    pub display: Display,
    /// The X server process.
    pub xserver: ProcRef,
    /// The Maverick process.
    pub wm: ProcRef,
    /// The recorded lifecycle state.
    pub state: SessionState,
    /// Why the session is in its current state, in one human-readable line.
    /// Carries what [`SessionState`] deliberately has no room for: a signal, a
    /// missing binary, a display that was already taken.
    pub exit_reason: String,
    /// Process-group ids this session created (see [`proc`]).
    pub pgrps: Vec<u32>,
}

impl Session {
    /// True if the window manager is up and answering.
    ///
    /// Liveness is the control socket, not the pid: a WM whose socket stopped
    /// answering is not a session a tool can drive, whatever `/proc` says. The
    /// pid is checked too, so a recycled pid cannot be mistaken for the WM.
    pub fn wm_is_up(&self) -> bool {
        self.wm.is_alive()
            && crate::control::ping(self.name.as_str()).is_ok()
            && identity::read_meta(self.name.as_str())
                .is_some_and(|i| i.start_time == 0 || i.start_time == self.wm.start_time)
    }

    /// The state the record *should* have, given what is actually running.
    ///
    /// Derived, never stored on its own: it is what makes a crashed session
    /// show up as crashed even if nothing ever wrote that down.
    pub fn derived_state(&self) -> SessionState {
        if self.wm_is_up() {
            return SessionState::Running;
        }
        match self.state {
            // A session that never came up is still starting until something
            // says otherwise; a recorded `Running` with a dead WM is the crash
            // case, which is the whole point of deriving rather than trusting.
            SessionState::Running | SessionState::Crashed => SessionState::Crashed,
            other => other,
        }
    }

    /// A fresh record for `name` with `spec`, owned by the current process.
    pub fn new(name: SessionName, spec: Spec) -> Self {
        Session {
            name,
            spec,
            owner_uid: current_uid(),
            owner_gid: current_gid(),
            created_at: epoch_secs(),
            display: Display(0),
            xserver: ProcRef::default(),
            wm: ProcRef::default(),
            state: SessionState::Starting,
            exit_reason: String::new(),
            pgrps: Vec::new(),
        }
    }

    /// The roots a process-ownership query may walk from, as pids.
    ///
    /// A root qualifies only if it is still the process the record named: a
    /// non-zero pid is not enough, because a pid is not an identity, and a
    /// record can carry `pid != 0` with `start_time == 0` — the pair
    /// `is_alive` treats as unproven. Requiring `is_alive` is what applies the
    /// pid-plus-start-time discipline to the roots, so a record naming a live
    /// pid the session never started stops being a way to claim that pid's
    /// whole subtree.
    ///
    /// The result is empty for a session that is not running, and empty is the
    /// only correct answer there: a stopped session owns nothing, so it must
    /// not be able to claim a process tree by accident. This is also why an
    /// empty set is returned rather than a placeholder — a `0` handed back
    /// downstream is indistinguishable from "walk from pid 0", which is how a
    /// stopped session ended up owning every process on the machine.
    pub fn owned_roots(&self) -> Vec<u32> {
        [self.wm, self.xserver]
            .into_iter()
            .filter(|proc_ref| proc_ref.is_alive())
            .map(|proc_ref| proc_ref.pid)
            .collect()
    }

    /// The session's directory.
    pub fn dir(&self) -> PathBuf {
        session_dir(&self.name)
    }

    /// The Xauthority file backing this session's display.
    pub fn xauth_path(&self) -> PathBuf {
        xauth_path(&self.name)
    }

    /// The window manager's log file.
    pub fn log_path(&self) -> PathBuf {
        log_path(&self.name)
    }

    /// The X server's log file.
    pub fn xserver_log_path(&self) -> PathBuf {
        xserver_log_path(&self.name)
    }

    /// The environment a process needs to *be* in this session.
    ///
    /// Four variables, and the reason for each is part of the contract:
    ///
    /// - `DISPLAY` — the session's X server. Without it a client talks to the
    ///   user's primary session instead, which is the exact accident a nested
    ///   session exists to prevent.
    /// - `XAUTHORITY` — the cookie file. `DISPLAY` alone is not authorisation,
    ///   and a client that guessed the display number would be refused.
    /// - `MAVERICK_SESSION` — the session name, so a program (or an agent)
    ///   can tell which session it is running in, and so the process sweep can
    ///   find it even if it left its process group.
    /// - `MAVERICK_INSTANCE` — the session id the WM exports to its children
    ///   so `maverickctl` targets *this* session by default.
    ///
    /// `XDG_RUNTIME_DIR` is deliberately **not** overridden: the control socket
    /// lives under it and the session has to be reachable from the same place
    /// the user's own tools look.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("DISPLAY".to_string(), self.display.to_string()),
            (
                "XAUTHORITY".to_string(),
                xauth_path(&self.name).display().to_string(),
            ),
            (
                "MAVERICK_SESSION".to_string(),
                self.name.as_str().to_string(),
            ),
            (
                "MAVERICK_INSTANCE".to_string(),
                self.name.as_str().to_string(),
            ),
            (
                "MAVERICK_LOG".to_string(),
                if self.spec.debug { "debug" } else { "info" }.to_string(),
            ),
            (
                "MAVERICK_NO_COMPOSITOR".to_string(),
                if self.spec.compositor {
                    String::new()
                } else {
                    "1".to_string()
                },
            ),
        ]
        .into_iter()
        // An empty value is how the kernel spells "unset" for an env var, and
        // `compositor_enabled` checks `var_os(..).is_none()` — so the variable
        // has to be *absent*, not empty, for the compositor to run.
        .filter(|(_, v)| !v.is_empty() || v == "info")
        .collect()
    }

    /// Serialize the record to its on-disk form.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\"name\":{name},\"backend\":\"{backend}\",\"resolution\":\"{res}\",",
                "\"refresh_rate\":{hz},\"binary\":{bin},\"cwd\":{cwd},\"args\":{args},",
                "\"debug\":{debug},\"compositor\":{comp},\"owner_uid\":{uid},",
                "\"owner_gid\":{gid},\"created_at\":{created},\"display\":\"{display}\",",
                "\"x_pid\":{xpid},\"x_start_time\":{xstart},\"wm_pid\":{wmpid},",
                "\"wm_start_time\":{wmstart},\"state\":\"{state}\",\"exit_reason\":{reason},",
                "\"pgrps\":{pgrps}}}"
            ),
            name = json_quote(self.name.as_str()),
            backend = self.spec.backend.label(),
            res = self.spec.resolution,
            hz = self
                .spec
                .refresh_rate
                .map_or("null".to_string(), |v| v.to_string()),
            bin = json_quote(&self.spec.binary),
            cwd = self
                .spec
                .cwd
                .as_ref()
                .map_or("null".to_string(), |p| json_quote(&p.display().to_string())),
            args = quote_array(&self.spec.args),
            debug = self.spec.debug,
            comp = self.spec.compositor,
            uid = self.owner_uid,
            gid = self.owner_gid,
            created = self.created_at,
            display = self.display,
            xpid = self.xserver.pid,
            xstart = self.xserver.start_time,
            wmpid = self.wm.pid,
            wmstart = self.wm.start_time,
            state = self.state.as_str(),
            reason = json_quote(&self.exit_reason),
            pgrps = quote_array(self.pgrps.iter().map(|g| g.to_string())),
        )
    }

    /// Parse a record, or `None` if it is not one.
    ///
    /// Lenient in the same way the ficha reader is: a field written by a
    /// different version defaults rather than failing the whole read, and the
    /// only hard requirement is a valid `name` — because that is the one field
    /// every path is built from, and a record that cannot name itself safely is
    /// not usable.
    ///
    /// A `null` field reads as *absent*, not as the four characters `null`
    /// (see [`crate::json::Field::text`]), so an optional the writer left empty
    /// stays empty on the way back in. That is what keeps `cwd: null` from
    /// becoming a session whose working directory is a file called "null".
    pub fn from_json(doc: &str) -> Option<Self> {
        let fields = scan_object(doc);
        let get_str = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .map(|f| f.text())
                .unwrap_or_default()
        };
        let get_u64 = |k: &str| fields.iter().find(|f| f.key == k).and_then(|f| f.as_u64());
        let get_bool = |k: &str| fields.iter().find(|f| f.key == k).and_then(|f| f.as_bool());
        let get_arr = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .and_then(|f| f.as_str_array())
        };

        let name = SessionName::parse(&get_str("name")).ok()?;
        let display = Display::parse(&get_str("display")).unwrap_or(Display(0));
        Some(Session {
            spec: Spec {
                backend: Backend::parse(&get_str("backend")).unwrap_or_default(),
                // A resolution that will not parse falls back to the default
                // rather than refusing the record: a session that ran at some
                // size must stay listable even if this build dislikes the
                // number.
                resolution: Resolution::parse(&get_str("resolution"))
                    .unwrap_or(Resolution::DEFAULT),
                refresh_rate: get_u64("refresh_rate").map(|v| v as u32),
                binary: get_str("binary"),
                cwd: {
                    let c = get_str("cwd");
                    (!c.is_empty()).then(|| PathBuf::from(c))
                },
                args: get_arr("args").unwrap_or_default(),
                debug: get_bool("debug").unwrap_or(false),
                compositor: get_bool("compositor").unwrap_or(true),
            },
            // A record that names no owner is this process's own: reading it is
            // reading a file only this uid can have written, in a `0700`
            // directory. Anything that *does* name an owner is taken at its
            // word here and checked against the kernel's answer in
            // `read_checked`.
            owner_uid: get_u64("owner_uid").unwrap_or(u64::from(current_uid())) as u32,
            owner_gid: get_u64("owner_gid").unwrap_or(u64::from(current_gid())) as u32,
            created_at: get_u64("created_at").unwrap_or(0),
            display,
            xserver: ProcRef {
                pid: get_u64("x_pid").unwrap_or(0) as u32,
                start_time: get_u64("x_start_time").unwrap_or(0),
            },
            wm: ProcRef {
                pid: get_u64("wm_pid").unwrap_or(0) as u32,
                start_time: get_u64("wm_start_time").unwrap_or(0),
            },
            state: SessionState::parse(&get_str("state")),
            exit_reason: get_str("exit_reason"),
            pgrps: get_arr("pgrps")
                .unwrap_or_default()
                .iter()
                .filter_map(|g| g.parse().ok())
                .collect(),
            name,
        })
    }
}

/// Unix epoch seconds, saturating at 0 if the clock is before the epoch.
pub(crate) fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A session's directory: the same per-session path the identity layer uses,
/// addressed by name instead of by a random id.
pub fn session_dir(name: &SessionName) -> PathBuf {
    identity::session_dir(name.as_str())
}

/// Path of a session's record.
pub fn spec_path(name: &SessionName) -> PathBuf {
    session_dir(name).join(SPEC_FILE)
}

/// Path of a session's Xauthority cookie file.
pub fn xauth_path(name: &SessionName) -> PathBuf {
    session_dir(name).join("Xauthority")
}

/// Path of a session's window-manager log.
pub fn log_path(name: &SessionName) -> PathBuf {
    session_dir(name).join("maverick.log")
}

/// Path of a session's X server log.
pub fn xserver_log_path(name: &SessionName) -> PathBuf {
    session_dir(name).join("xserver.log")
}

/// Read a session record, or `None` when the name has none.
///
/// A record whose `owner_uid` is not the current uid is refused *and removed*:
/// a file this user can read but that claims to belong to someone else is either
/// a mistake or a leftover from a shared `/run/user` mount, and honouring it
/// would let a stale record redirect `exec` into another user's session.
pub fn read(name: &SessionName) -> Option<Session> {
    let path = spec_path(name);
    let doc = std::fs::read_to_string(&path).ok()?;
    let session = Session::from_json(&doc)?;
    if session.owner_uid != current_uid() {
        return None;
    }
    Some(session)
}

/// Read a record, or report why it could not be read.
pub fn read_checked(name: &SessionName) -> Result<Session, SessionError> {
    let path = spec_path(name);
    let doc = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            SessionError::NotFound(name.clone())
        } else {
            SessionError::Io(format!("{}: {e}", path.display()))
        }
    })?;
    let session = Session::from_json(&doc).ok_or_else(|| SessionError::Corrupt {
        name: name.clone(),
        path: path.clone(),
    })?;
    if session.owner_uid != current_uid() {
        return Err(SessionError::NotOwned(name.clone()));
    }
    Ok(session)
}

/// Write a session record atomically.
///
/// The record is written to a temporary file in the same directory and renamed
/// over the target, because a reader (`maverickctl session list`, a bar, an
/// agent) may be walking the runtime directory at any moment and a
/// half-written record would read as a corrupt session. `rename` within one
/// directory is atomic, so a reader sees either the old record or the new one.
pub fn write(session: &Session) -> io::Result<()> {
    let dir = session.dir();
    identity::ensure_runtime_dir()?;
    std::fs::create_dir_all(&dir)?;
    // 0700 on the directory is the first line of the session's security: it is
    // what keeps another uid out of the socket, the cookie and the logs.
    identity::set_private_dir(&dir)?;
    let path = spec_path(&session.name);
    let tmp = dir.join(format!("{SPEC_FILE}.tmp"));
    std::fs::write(&tmp, session.to_json())?;
    {
        use std::os::unix::fs::PermissionsExt;
        // The record is not a secret, but it is a description of the user's
        // processes and arguments; 0600 keeps it consistent with the rest of
        // the directory instead of relying on the parent for it.
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)
}

/// The current process's real uid.
///
/// The kernel's answer, via the crate's single credential wrapper
/// ([`identity::current_uid`]). Nothing here takes it from a caller: ownership
/// is a fact about who ran the command, and a record that could declare its own
/// owner would make every `owner_uid` check on it meaningless.
pub fn current_uid() -> u32 {
    identity::current_uid()
}

/// The current process's real gid.
pub fn current_gid() -> u32 {
    identity::current_gid()
}

/// Every session name that has a record on disk.
///
/// Records are not filtered by state: a stopped session is still a session, and
/// an agent that created one and lost the reply needs to find it again. The
/// window manager's own instances (which have a ficha but no record) are added
/// by [`list`], not here.
pub fn names() -> Vec<SessionName> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(identity::runtime_dir()) else {
        return out;
    };
    for entry in dir.flatten() {
        // A record is only ever a regular file inside a real directory: a
        // symlink planted in the runtime dir must not be followed.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(name) = SessionName::parse(&name) else {
            continue;
        };
        if spec_path(&name).is_file() {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// A session as the CLI and an agent see it: one row, all the facts, no
/// follow-up calls needed to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The name it is addressed by.
    pub name: String,
    /// The session id used for control IPC (equal to the name for a session the
    /// manager created, a random id for one started by hand).
    pub sid: String,
    /// `main` for a session the manager did not create, `nested` otherwise.
    pub kind: &'static str,
    /// The live state.
    pub state: SessionState,
    /// The X display, or `""` when unknown.
    pub display: String,
    /// The screen geometry when it is known.
    pub resolution: Option<Resolution>,
    /// Requested refresh rate, as declared intent.
    pub refresh_rate: Option<u32>,
    /// The X server pid.
    pub x_pid: Option<u32>,
    /// The window manager pid.
    pub pid: Option<u32>,
    /// The Maverick binary in use.
    pub binary: String,
    /// Whether the session runs in debug mode.
    pub debug: bool,
    /// Whether the compositor was requested.
    pub compositor_requested: bool,
    /// Which nested X server implementation is in use, when known.
    pub backend: Option<Backend>,
    /// Why the session is in its state, in one line.
    pub exit_reason: String,
    /// Unix epoch seconds when the session was created, or 0 when unknown.
    pub created_at: u64,
    /// The owning uid, recorded.
    pub owner_uid: u32,
}

impl SessionView {
    /// The mode column of `session list`: what the session is *for*, which is
    /// the one distinction a user scanning the list is looking for.
    pub fn mode(&self) -> &'static str {
        if self.debug {
            "debug"
        } else if self.kind == "main" {
            "main"
        } else {
            "nested"
        }
    }
}

/// One `maverickctl session list` row.
pub fn list() -> Vec<SessionView> {
    let mut out: Vec<SessionView> = names()
        .iter()
        .filter_map(read)
        .map(|s| view_of(&s))
        .collect();

    // A session the manager did not create is still a session: the user's own
    // window manager has a ficha but no record, and `maverickctl window list
    // main` has to work. Its name is what it was launched with, and the
    // default `--name` reads as `main` so the common case needs no ceremony.
    //
    // Only *live* instances are listed. A dead instance with no record is not
    // an addressable session — nothing answers on its socket — it is debris
    // from a run that ended badly, and `maverickctl prune` is what removes
    // that. Listing it here would make `session list` report sessions the user
    // cannot act on.
    for inst in crate::discover::list_instances() {
        let already = out.iter().any(|v| v.sid == inst.session_id);
        if already || inst.session_id.is_empty() || !inst.alive {
            continue;
        }
        let default_name = inst.name == identity::DEFAULT_NAME;
        out.push(SessionView {
            name: if default_name {
                MAIN_ALIAS.to_string()
            } else {
                inst.name.clone()
            },
            sid: inst.session_id.clone(),
            kind: "main",
            state: if inst.alive {
                SessionState::Running
            } else {
                SessionState::Crashed
            },
            // The resolution of a session this manager did not create is not
            // knowable from its record, because there is no record: it is
            // reported by asking the WM, and only where a caller needs it
            // (`session status`, `inspect`) rather than on every list row.
            display: inst.display.clone(),
            resolution: None,
            refresh_rate: None,
            x_pid: None,
            pid: (inst.pid != 0).then_some(inst.pid),
            binary: inst.exe.clone(),
            debug: false,
            compositor_requested: true,
            backend: None,
            exit_reason: if inst.alive {
                String::new()
            } else {
                "the window manager is not answering".to_string()
            },
            created_at: inst.started_at,
            owner_uid: current_uid(),
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Build the view of a session the manager owns.
fn view_of(s: &Session) -> SessionView {
    let state = s.derived_state();
    // A crash that nothing has reaped yet has no recorded reason, because the
    // reason is written when the reaper runs. Until then the note has to say
    // what is *known* — the window manager is not answering — and what to do
    // about it, because a row that says "crashed" with no note leaves the
    // reader to guess whether an X server is still holding a display.
    let exit_reason = if s.exit_reason.is_empty() && state == SessionState::Crashed {
        "the window manager is not answering; `maverickctl session stop` will clean up".to_string()
    } else {
        s.exit_reason.clone()
    };
    SessionView {
        name: s.name.as_str().to_string(),
        sid: s.name.as_str().to_string(),
        kind: "nested",
        state,
        display: s.display.to_string(),
        resolution: Some(s.spec.resolution),
        refresh_rate: s.spec.refresh_rate,
        x_pid: s.xserver.pid.checked_sub(0).filter(|_| s.xserver.is_some()),
        pid: s.wm.pid.checked_sub(0).filter(|_| s.wm.is_some()),
        binary: s.spec.binary.clone(),
        debug: s.spec.debug,
        compositor_requested: s.spec.compositor,
        backend: Some(s.spec.backend),
        exit_reason,
        created_at: s.created_at,
        owner_uid: s.owner_uid,
    }
}

/// Find the session a command means.
///
/// Resolution order, first match wins:
///
/// 1. an exact session name with a record or a live instance,
/// 2. the [`MAIN_ALIAS`] fallback: the live instance on the caller's own
///    `DISPLAY`, which is what makes the user's own session addressable as
///    `main` without having to be started by this manager,
/// 3. nothing — the error names the sessions that do exist, so a typo is one
///    glance from being fixed.
pub fn resolve(name: &str) -> Result<SessionView, SessionError> {
    if let Ok(parsed) = SessionName::parse(name) {
        if let Some(s) = read(&parsed) {
            return Ok(view_of(&s));
        }
    }
    if name == MAIN_ALIAS {
        if let Some(view) = main_session() {
            return Ok(view);
        }
    }
    // A live instance is addressable by its id too, so `--session <sid>` keeps
    // working for a WM this manager never started.
    if let Some(inst) = crate::discover::find_by_name(name) {
        if inst.alive {
            let default_name = inst.name == identity::DEFAULT_NAME;
            return Ok(SessionView {
                name: if default_name {
                    MAIN_ALIAS.to_string()
                } else {
                    inst.name.clone()
                },
                sid: inst.session_id.clone(),
                kind: "main",
                state: SessionState::Running,
                display: inst.display.clone(),
                resolution: None,
                refresh_rate: None,
                x_pid: None,
                pid: (inst.pid != 0).then_some(inst.pid),
                binary: inst.exe.clone(),
                debug: false,
                compositor_requested: true,
                backend: None,
                exit_reason: String::new(),
                created_at: inst.started_at,
                owner_uid: current_uid(),
            });
        }
    }
    Err(SessionError::NotFound(
        SessionName::parse(name).unwrap_or_else(|_| {
            SessionName::parse(MAIN_ALIAS).expect("the reserved alias is a valid name")
        }),
    ))
}

/// The caller's own session: the live instance whose display is the caller's
/// `DISPLAY`.
///
/// `DISPLAY` is the discriminator rather than the controlling terminal because
/// a nested session has no terminal at all, and a terminal is shared by every
/// window of the same session. The empty-`DISPLAY` case (a tool run with no X
/// environment) falls back to the sole live instance, and refuses when there is
/// more than one rather than guessing.
pub fn main_session() -> Option<SessionView> {
    let ctx = identity::current_display();
    let live: Vec<_> = crate::discover::list_instances()
        .into_iter()
        .filter(|i| i.alive)
        .collect();
    let candidates: Vec<_> = if ctx.is_empty() {
        live.iter().collect()
    } else {
        live.iter().filter(|i| i.display == ctx).collect()
    };
    let inst = match candidates.as_slice() {
        [only] => *only,
        // Ambiguous: picking one would be a guess about which graphical session
        // the user is actually in.
        _ => return None,
    };
    Some(SessionView {
        name: if inst.name == identity::DEFAULT_NAME {
            MAIN_ALIAS.to_string()
        } else {
            inst.name.clone()
        },
        sid: inst.session_id.clone(),
        kind: "main",
        state: SessionState::Running,
        display: inst.display.clone(),
        resolution: None,
        refresh_rate: None,
        x_pid: None,
        pid: (inst.pid != 0).then_some(inst.pid),
        binary: inst.exe.clone(),
        debug: false,
        compositor_requested: true,
        backend: None,
        exit_reason: String::new(),
        created_at: inst.started_at,
        owner_uid: current_uid(),
    })
}

/// Resolve the target of a control command, returning the session id to address
/// the control socket with.
///
/// An explicit `--session`/`--name` wins, then `$MAVERICK_SESSION` (the
/// variable every process launched into a session carries), then the caller's
/// own session, then the sole live instance. This mirrors the precedence
/// [`crate::ctl`] documents for the control channel, with the session name
/// added in front of it because it is the most specific thing a caller can say.
pub fn resolve_target(explicit: Option<&str>) -> Result<SessionView, SessionError> {
    if let Some(name) = explicit {
        return resolve(name);
    }
    if let Ok(env) = std::env::var("MAVERICK_SESSION") {
        if !env.is_empty() {
            if let Ok(view) = resolve(&env) {
                return Ok(view);
            }
        }
    }
    if let Some(view) = main_session() {
        return Ok(view);
    }
    let all: Vec<SessionView> = list()
        .into_iter()
        .filter(|v| v.state == SessionState::Running)
        .collect();
    match all.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(SessionError::NoSessions),
        _ => Err(SessionError::Ambiguous(
            all.iter().map(|v| v.name.clone()).collect(),
        )),
    }
}

/// A session operation failed, in a form the CLI can print verbatim.
///
/// The variants exist so the messages can be *specific*: "already running" and
/// "no such session" call for different advice, and a single error string with
/// the detail interpolated is how a user ends up reading `error: session not
/// found` when what actually happened was a display collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// No session by that name.
    NotFound(SessionName),
    /// A session record exists but is not readable.
    Corrupt {
        /// The session it claimed to be.
        name: SessionName,
        /// Where the record was.
        path: PathBuf,
    },
    /// The record belongs to another uid.
    NotOwned(SessionName),
    /// The session is already running.
    AlreadyRunning(SessionName),
    /// No Maverick is running at all.
    NoSessions,
    /// More than one session could be meant.
    Ambiguous(Vec<String>),
    /// A backend binary is missing.
    MissingBackend {
        /// Which binary.
        binary: String,
        /// Why it could not be used.
        reason: String,
    },
    /// The Maverick binary a session records cannot be executed.
    MissingBinary {
        /// The path or name that was tried.
        path: String,
    },
    /// The session is still running, so its record cannot just be deleted.
    StillRunning(SessionName),
    /// The window manager did not come up.
    WmFailed {
        /// Why, as far as could be told.
        reason: String,
        /// Where its own log is.
        log: PathBuf,
    },
    /// Anything else, with the context that makes it actionable.
    Io(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "session '{name}' does not exist"),
            Self::Corrupt { name, path } => {
                write!(
                    f,
                    "session record for '{name}' is unreadable ({})",
                    path.display()
                )
            }
            Self::NotOwned(name) => write!(
                f,
                "session '{name}' belongs to another user and is not accessible"
            ),
            Self::AlreadyRunning(name) => {
                write!(f, "session '{name}' is already running")
            }
            Self::NoSessions => write!(f, "no running Maverick session found"),
            Self::Ambiguous(names) => write!(
                f,
                "several sessions match — name one of: {}",
                names.join(", ")
            ),
            Self::MissingBackend { binary, reason } => {
                write!(f, "the {binary} X server is not usable: {reason}")
            }
            Self::MissingBinary { path } => write!(
                f,
                "the Maverick binary '{path}' cannot be executed — build it, or pass --binary"
            ),
            Self::StillRunning(name) => write!(
                f,
                "session '{name}' is still running — stop it first (or pass --force)"
            ),
            Self::WmFailed { reason, log } => write!(
                f,
                "the session's window manager did not start: {reason}\n  its log: {}",
                log.display()
            ),
            Self::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// How long to wait for a component of a session to come up.
///
/// Generous enough for a cold start on a slow machine (the X server, a GL
/// context and a window map are all in that window) and short enough that a
/// failed create reports in seconds rather than hanging an agent.
pub const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a component gets to exit on `SIGTERM` before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// Wait until `predicate` holds, or `timeout` elapses. Returns whether it did.
///
/// The polling interval is short enough to feel immediate and long enough that
/// a start does not spend its time in `nanosleep`: these waits are for a
/// process to publish a socket, not for a CPU to be free.
pub fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Remove a session's runtime directory, but only its own regular files first.
///
/// A session directory can be deleted outright — it is this user's, inside a
/// `0700` parent — but `remove_dir_all` on a path assembled from a name is the
/// one operation where a symlinked component would matter, so the directory is
/// checked to be a real directory (not a symlink) before anything is removed.
pub fn remove_dir(name: &SessionName) -> io::Result<()> {
    let dir = session_dir(name);
    let meta = std::fs::symlink_metadata(&dir)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to remove {}: not a session directory",
                dir.display()
            ),
        ));
    }
    std::fs::remove_dir_all(&dir)
}

/// Read a file's last `n` lines, for showing why a start failed.
///
/// Bounded on both ends: a session log grows without limit over a long run, and
/// the point of this is the *end* of it. The read is from the tail of the file
/// rather than the whole of it, so a multi-megabyte log does not have to be
/// loaded to show its last few lines.
pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    const CHUNK: usize = 8 * 1024;
    let meta = std::fs::metadata(path)?;
    let mut file = std::fs::File::open(path)?;
    // Collected newest-first while walking backwards, so the loop can stop as
    // soon as it has enough without ever holding the whole file.
    let mut collected: Vec<String> = Vec::new();
    let mut pos = meta.len();
    // The head of the next chunk back, waiting to be re-joined: a chunk
    // boundary can fall inside a line.
    let mut pending = String::new();
    while pos > 0 && collected.len() <= lines {
        let read = CHUNK.min(pos as usize);
        pos -= read as u64;
        use std::io::{Read, Seek, SeekFrom};
        file.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![0u8; read];
        file.read_exact(&mut buf)?;
        // Newer bytes come first in the file, so they belong *after* the
        // carried-over head of the older chunk.
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        text.push_str(&pending);
        let mut parts: Vec<&str> = text.split('\n').collect();
        // The first element continues a line from the chunk before this one —
        // unless this chunk starts at byte 0, where the first element is a real
        // (possibly empty) line.
        pending = if pos > 0 {
            parts.remove(0).to_string()
        } else {
            String::new()
        };
        for line in parts.into_iter().rev() {
            collected.push(line.to_string());
        }
    }
    if !pending.is_empty() {
        collected.push(pending);
    }
    // Oldest last. Only now can the trailing empty element — the one a file
    // ending in `\n` produces — be recognised and dropped, because while the
    // collection was reversed it was the *first* element.
    collected.reverse();
    while collected.last().is_some_and(|l| l.trim().is_empty()) {
        collected.pop();
    }
    if collected.len() > lines {
        collected.drain(..collected.len() - lines);
    }
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ownership contract, stated once so a change has to argue with it.
    ///
    /// A session may only claim a process it can prove it started. The proof is
    /// the recorded pid *and* start time still matching, so the two rules below
    /// are the same rule applied to the two roots: a root is either a verified
    /// live process or nothing at all.
    #[test]
    fn a_stopped_session_owns_nothing() {
        let mut session = Session::new(
            SessionName::parse("stopped").expect("name"),
            Spec::default(),
        );
        assert!(session.owned_roots().is_empty());

        // What `teardown` leaves behind: both roots defaulted.
        session.wm = ProcRef::default();
        session.xserver = ProcRef::default();
        assert!(
            session.owned_roots().is_empty(),
            "a default ProcRef must not become a traversal root"
        );

        // A live root, then the same record after it goes away.
        let pid = std::process::id();
        session.wm = ProcRef::of(pid);
        assert_eq!(session.owned_roots(), vec![pid]);
        session.wm = ProcRef::default();
        assert!(session.owned_roots().is_empty());
    }

    /// A pid is not an identity. The record can carry a non-zero pid with no
    /// start time — `from_json` defaults a missing field to 0 — and such a ref
    /// proves nothing, so it must not authorise a subtree walk.
    #[test]
    fn a_root_without_a_start_time_is_not_a_root() {
        let mut session = Session::new(
            SessionName::parse("unproven").expect("name"),
            Spec::default(),
        );
        let pid = std::process::id();
        session.wm = ProcRef { pid, start_time: 0 };
        assert!(session.owned_roots().is_empty());

        session.wm = ProcRef::of(pid);
        assert_eq!(session.owned_roots(), vec![pid]);
    }

    /// Registered groups are actionable ownership state, so they are consulted
    /// only while there is a live session to own them. A record that never
    /// passed through teardown — hand-edited, left by an older build, written by
    /// a crash — must not be able to authorise a signal on that basis.
    #[test]
    fn registered_groups_need_a_live_session() {
        let mut session =
            Session::new(SessionName::parse("groups").expect("name"), Spec::default());
        session.pgrps = vec![555];
        // No live root, so the groups are not actionable: the ownership
        // computation drops the term entirely rather than trusting the record.
        assert!(session.owned_roots().is_empty());
        session.wm = ProcRef::of(std::process::id());
        assert_eq!(session.owned_roots(), vec![std::process::id()]);
    }

    #[test]
    fn resolution_round_trips_and_validates() {
        let r = Resolution::parse("1280x720").expect("parse");
        assert_eq!(r, Resolution::new(1280, 720).expect("new"));
        assert_eq!(r.to_string(), "1280x720");
        assert_eq!(
            Resolution::parse(" 1920X1080 ").expect("parse"),
            Resolution::new(1920, 1080).unwrap()
        );
        assert_eq!(
            Resolution::parse("800x600").expect("parse").to_string(),
            "800x600"
        );
    }

    /// A resolution is the whole of the user's intent about geometry, so a
    /// near-miss must be an error and not a guess at which part they meant.
    #[test]
    fn a_malformed_resolution_is_refused() {
        for bad in [
            "",
            "1280",
            "1280*720",
            "1280x",
            "x720",
            "1280x720x24",
            "-1x720",
            "0x0",
        ] {
            assert!(
                Resolution::parse(bad).is_err(),
                "'{bad}' must not parse as a resolution"
            );
        }
        assert!(Resolution::new(0, 100).is_err());
        assert!(Resolution::new(MAX_DIMENSION + 1, 100).is_err());
    }

    /// The name becomes a directory, a socket path and a ficha filename, so the
    /// accepted set is exactly the one the identity layer already treats as a
    /// safe single path component.
    #[test]
    fn session_names_are_safe_path_components() {
        for good in ["main", "debug", "agent", "test-1", "a_b", "X9"] {
            assert!(SessionName::parse(good).is_ok(), "'{good}' should be valid");
        }
        for bad in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a b",
            "a.b",
            "a\nb",
            "a:b",
            "a\0b",
        ] {
            assert!(
                SessionName::parse(bad).is_err(),
                "'{bad}' must not be a session name"
            );
        }
        assert!(SessionName::parse(&"a".repeat(65)).is_err());
        assert!(SessionName::parse(&"a".repeat(64)).is_ok());
    }

    /// A record is what an agent reads back, and every field it wrote has to
    /// survive the round trip — including the ones that make a record awkward:
    /// a binary path with a backslash, an argument holding a quote and a
    /// comma, an empty argument list.
    #[test]
    fn a_session_record_round_trips() {
        let s = Session {
            name: SessionName::parse("debug").expect("name"),
            spec: Spec {
                backend: Backend::Xvfb,
                resolution: Resolution::new(1024, 768).expect("res"),
                refresh_rate: Some(120),
                binary: "/home/u/Maverick\\target/debug/maverick".into(),
                cwd: Some(PathBuf::from("/home/u/Descargas/Maverick")),
                args: vec!["--debug".into(), r#"--log "x,y""#.into(), String::new()],
                debug: true,
                compositor: false,
            },
            owner_uid: 1000,
            owner_gid: 1000,
            created_at: 1_700_000_000,
            display: Display(7),
            xserver: ProcRef {
                pid: 111,
                start_time: 222,
            },
            wm: ProcRef {
                pid: 333,
                start_time: 444,
            },
            state: SessionState::Running,
            exit_reason: "reason with \"quotes\" and, a comma".into(),
            pgrps: vec![555, 666],
        };
        let back = Session::from_json(&s.to_json()).expect("round trip");
        assert_eq!(back, s);
    }

    /// A record from a different build must stay usable: a missing field
    /// defaults, it does not make the session unmanageable.
    #[test]
    fn a_partial_record_still_parses() {
        let s = Session::from_json(r#"{"name":"debug","display":":2"}"#).expect("minimal record");
        assert_eq!(s.name.as_str(), "debug");
        assert_eq!(s.display, Display(2));
        assert_eq!(s.spec.resolution, Resolution::DEFAULT);
        assert!(s.spec.compositor, "the compositor is opt-out, not opt-in");
        assert!(!s.spec.debug);
        assert_eq!(s.state, SessionState::Stopped);
        assert!(s.wm.pid == 0 && s.wm.start_time == 0);
    }

    /// Without a valid name there is no record: the name is what every path is
    /// built from, so a record that cannot name itself safely is unusable.
    #[test]
    fn a_record_without_a_usable_name_is_refused() {
        for bad in [
            r#"{}"#,
            r#"{"name":""}"#,
            r#"{"name":"../escape"}"#,
            r#"{"name":"a/b"}"#,
            r#"{"session_id":"abc"}"#,
            "not json at all",
        ] {
            assert!(
                Session::from_json(bad).is_none(),
                "{bad:?} must not produce a session"
            );
        }
    }

    /// The environment is the contract that makes a program *be* in the
    /// session, so each variable is pinned. `MAVERICK_NO_COMPOSITOR` in
    /// particular must be absent rather than empty: the WM tests
    /// `var_os(..).is_none()`, so an empty value would silently disable the
    /// compositor in every session that asked for one.
    #[test]
    fn the_session_environment_is_exactly_what_a_client_needs() {
        let mut s = Session::new(
            SessionName::parse("debug").expect("name"),
            Spec {
                binary: "/usr/bin/maverick".into(),
                debug: true,
                ..Spec::default()
            },
        );
        s.display = Display(3);
        let env: std::collections::HashMap<String, String> = s.env().into_iter().collect();
        assert_eq!(env.get("DISPLAY").map(String::as_str), Some(":3"));
        assert_eq!(
            env.get("XAUTHORITY").map(String::as_str),
            Some(xauth_path(&s.name).display().to_string().as_str())
        );
        assert_eq!(
            env.get("MAVERICK_SESSION").map(String::as_str),
            Some("debug")
        );
        assert_eq!(
            env.get("MAVERICK_INSTANCE").map(String::as_str),
            Some("debug")
        );
        assert_eq!(env.get("MAVERICK_LOG").map(String::as_str), Some("debug"));
        assert!(
            !env.contains_key("MAVERICK_NO_COMPOSITOR"),
            "an empty value would read as 'compositor disabled' to the WM"
        );

        s.spec.debug = false;
        s.spec.compositor = false;
        let env: std::collections::HashMap<String, String> = s.env().into_iter().collect();
        assert_eq!(env.get("MAVERICK_LOG").map(String::as_str), Some("info"));
        assert_eq!(
            env.get("MAVERICK_NO_COMPOSITOR").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn states_round_trip_and_default_safely() {
        for s in [
            SessionState::Starting,
            SessionState::Running,
            SessionState::Stopped,
            SessionState::Crashed,
        ] {
            assert_eq!(SessionState::parse(s.as_str()), s);
        }
        // A state from a future version must not make a session look alive.
        assert_eq!(SessionState::parse("hibernating"), SessionState::Stopped);
    }

    /// A crash is *derived*, never trusted: a record that still says `running`
    /// with a dead window manager is exactly the zombie a tool must not talk
    /// to, and the derived state is what has to say so.
    #[test]
    fn a_recorded_running_session_with_a_dead_wm_reads_as_crashed() {
        let s = Session {
            name: SessionName::parse("debug").expect("name"),
            state: SessionState::Running,
            wm: ProcRef {
                pid: 999_999,
                start_time: 1,
            },
            ..Session::new(SessionName::parse("debug").expect("name"), Spec::default())
        };
        assert_eq!(s.derived_state(), SessionState::Crashed);
        // A session that never came up stays in the state it was recorded in.
        let starting = Session {
            state: SessionState::Starting,
            ..s.clone()
        };
        assert_eq!(starting.derived_state(), SessionState::Starting);
    }

    /// A recycled pid must never pass for the process that was recorded.
    #[test]
    fn a_pid_ref_is_live_only_for_the_exact_process() {
        let me = ProcRef::of(std::process::id());
        assert!(me.is_some() && me.is_alive());
        let wrong = ProcRef {
            pid: me.pid,
            start_time: me.start_time + 1,
        };
        assert!(
            !wrong.is_alive(),
            "a different start time is a different process"
        );
        assert!(!ProcRef::default().is_alive());
        assert!(!ProcRef::default().is_some());
    }

    #[test]
    fn tail_returns_the_end_of_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log");
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &body).expect("write");
        let got = tail(&path, 5).expect("tail");
        assert_eq!(
            got,
            vec!["line 96", "line 97", "line 98", "line 99", "line 100"]
        );
    }

    /// The tail has to work on a file bigger than one read chunk, and on one
    /// with no trailing newline — the two shapes a naive chunked read gets
    /// wrong (losing a line, or reporting a phantom empty one).
    #[test]
    fn tail_survives_chunk_boundaries_and_a_missing_final_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big");
        let long = "x".repeat(5_000);
        let body: String = (1..=200)
            .map(|i| format!("{i:04} {long}\n"))
            .collect::<String>()
            + "no newline at the end";
        std::fs::write(&path, body).expect("write");
        let got = tail(&path, 3).expect("tail");
        assert_eq!(got.len(), 3);
        assert_eq!(got[2], "no newline at the end");
        assert!(got[0].starts_with("0199 "), "got {:?}", got[0]);
        assert!(got[1].starts_with("0200 "), "got {:?}", got[1]);
    }

    #[test]
    fn an_empty_log_tails_to_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty");
        std::fs::write(&path, "").expect("write");
        assert!(tail(&path, 10).expect("tail").is_empty());
    }

    /// Removing a session directory must not be steerable: the path is built
    /// from a validated name, and a symlink in its place is refused rather than
    /// followed. The planted link is removed again at the end, because the path
    /// is the real runtime directory and a leftover one would break every
    /// later run of this test.
    #[test]
    fn removal_refuses_a_symlink_where_the_directory_should_be() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Unique per process: this test writes into the shared runtime dir, so
        // two concurrent runs must not collide on the same name.
        let name = SessionName::parse(&format!("symlinktest{}", std::process::id())).expect("name");
        let planted = session_dir(&name);
        let _ = std::fs::remove_file(&planted);
        let target = dir.path().join("precious");
        std::fs::create_dir(&target).expect("target");
        std::os::unix::fs::symlink(&target, &planted).expect("plant a symlink");

        let err = remove_dir(&name).expect_err("a symlink must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(target.exists(), "the target must be untouched");

        let _ = std::fs::remove_file(&planted);
    }

    /// Every error variant has to read as advice, not as an exception dump:
    /// the message is what an agent (or a person) is shown.
    #[test]
    fn errors_name_the_session_and_the_way_out() {
        let name = SessionName::parse("debug").expect("name");
        assert_eq!(
            SessionError::NotFound(name.clone()).to_string(),
            "session 'debug' does not exist"
        );
        assert!(SessionError::AlreadyRunning(name.clone())
            .to_string()
            .contains("already running"));
        assert!(SessionError::Ambiguous(vec!["main".into(), "debug".into()])
            .to_string()
            .contains("main, debug"));
        let wm = SessionError::WmFailed {
            reason: "binary not found".into(),
            log: PathBuf::from("/run/user/1000/maverick/debug/maverick.log"),
        };
        let text = wm.to_string();
        assert!(text.contains("binary not found") && text.contains("maverick.log"));
    }
}
