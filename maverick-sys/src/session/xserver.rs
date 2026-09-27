//! The nested X server that backs a Maverick session.
//!
//! A session needs a real X server of its own: the window manager connects to
//! `DISPLAY`, the compositor needs GLX/Composite/Damage, and every application
//! the session runs expects a normal X client environment. Maverick does not
//! nest a server into itself — the session *manager* does, here.
//!
//! # Why Xephyr, and what "backend" means here
//!
//! The alternatives on Linux were compared against the criteria that actually
//! matter for this feature (a real server, a real GLX for the compositor, no
//! privileges, scriptable, present on a stock Arch install):
//!
//! * **Nested Xorg** (`Xorg -configure` with a generated `xorg.conf`) is the
//!   most "real" option and is what a distribution would ship, but it needs
//!   the Xorg driver modules (`/usr/lib/Xorg/modules/drivers`) to bind a
//!   seat/driver at all, it races the host Xorg for DRM master, and on a
//!   multi-seat system it wants `logind` session ownership. On this machine the
//!   module directory does not even exist, so it cannot be used without root
//!   and a distribution-specific driver package.
//! * **Xvfb** is a real X server and needs no privileges, but it is *headless*:
//!   nothing renders into the parent display, so a user cannot see the session
//!   they just created, and its GLX is software-rasterised (llvmpipe) or
//!   absent, which means the real Maverick compositor either fails to
//!   initialise or runs a path it never runs on real hardware. It stays as a
//!   backend for headless/CI use.
//! * **Xephyr** is a real X server whose framebuffer *is* a window on the
//!   parent display, with real GLX, Composite, Damage and RANDR. It is the
//!   only option that gives a developer what this feature is for: a second
//!   Maverick they can watch, at an independent resolution, without touching
//!   the primary session.
//!
//! So the session manager owns a [`Backend`] abstraction and Xephyr is its
//! default implementation. Nothing in [`crate::session`] is "an Xephyr
//! wrapper": display allocation, the Xauthority cookie, readiness, liveness,
//! teardown and cleanup live here and are backend-independent, and a future
//! `xorg` backend is a second `Backend` arm rather than a rewrite.
//!
//! # Ownership and lifecycle
//!
//! [`XServer`] owns the child process. Spawning returns a handle with the pid
//! *and* the pid's start time, because a pid alone is not an identity — the
//! kernel recycles pids, and every later signal is gated on that pair (see
//! [`crate::session::proc`]). Dropping the handle does **not** kill the server:
//! the session outlives the `maverickctl` invocation that created it, and the
//! teardown path is an explicit, recorded step ([`XServer::stop`]) so a crash
//! leaves something to reap rather than a signal nobody sent.
//!
//! # Security
//!
//! - The server is started with `-auth` pointing at a cookie file that lives
//!   inside the session's `0700` directory with `0600` permissions, so the
//!   session's display is unusable by anyone who cannot read that file. This
//!   is what stops a *second* user from bypassing `maverickctl` and talking to
//!   the display directly.
//! - TCP listening is disabled, so the display is reachable only through the
//!   abstract/unix socket, never the network.
//! - The cookie is never written to the session record, the log files, or any
//!   command output: it is a secret and only the `XAUTHORITY` file path is
//!   reported.

use std::io;
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::proc;
use super::ProcRef;

/// Which nested X server implementation to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// A real X server rendered into a window on the parent display. The
    /// default, and the only backend that gives a *visible* session with a
    /// hardware/GLX compositor path.
    #[default]
    Xephyr,
    /// Headless real X server. No window on the parent display and a software
    /// (or absent) GLX, so the compositor may fall back to the X11 path. Meant
    /// for CI and for `maverickctl exec`-style automation, not for watching.
    Xvfb,
}

impl Backend {
    /// Parse the backend name accepted on the command line.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "xephyr" | "nested" | "default" => Some(Self::Xephyr),
            "xvfb" | "headless" => Some(Self::Xvfb),
            _ => None,
        }
    }

    /// The backend's binary name.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Xephyr => "Xephyr",
            Self::Xvfb => "Xvfb",
        }
    }

    /// The name used in `session status` / `inspect` output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Xephyr => "xephyr",
            Self::Xvfb => "xvfb",
        }
    }
}

/// An X display number, rendered as the `DISPLAY` value it corresponds to.
///
/// Kept as its own type because the number and the `":N"` string travel
/// together everywhere: a session's display is meaningless as one without the
/// other, and formatting it by hand in each caller is how a `":1"` and a `1`
/// drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Display(pub u32);

impl std::fmt::Display for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, ":{}", self.0)
    }
}

impl Display {
    /// Parse a `DISPLAY` value. Only the local form `:N` is accepted: a
    /// remote display (`host:N`) is a different machine's session, which this
    /// manager neither creates nor owns.
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.trim().strip_prefix(':')?;
        // Tolerate a screen suffix (`:1.0`), which selects a screen on a
        // multi-screen server rather than a different display.
        let number = rest.split('.').next()?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        number.parse().ok().map(Self)
    }
}

/// Where the X server's unix socket lives for a display number.
///
/// This is the X11 ABI, not a Maverick choice: every X server on Linux binds
/// `<runtime>/X<n>` under `/tmp/.X11-unix`. It carries no secret — the
/// session's data and cookie live in the session's own `0700` directory — and
/// the display is protected by the Xauthority cookie, not by this path.
fn socket_path(display: Display) -> PathBuf {
    Path::new("/tmp/.X11-unix").join(format!("X{}", display.0))
}

/// The lock file an X server creates to claim a display number.
fn lock_path(display: Display) -> PathBuf {
    PathBuf::from(format!("/tmp/.X{}-lock", display.0))
}

/// True if the display number is free: neither the socket nor the lock file
/// exists.
///
/// Both are checked because they are created at different moments. A server
/// creates its lock before it starts listening, so a lock without a socket is
/// a server that is still coming up (or died without cleaning up), and a
/// socket without a lock is a server that has finished starting. Either means
/// "not ours to take".
pub fn display_is_free(display: Display) -> bool {
    !socket_path(display).exists() && !lock_path(display).exists()
}

/// The first free display number at or after `from`.
///
/// X display numbering is a flat namespace with no allocator: "free" means
/// "nothing has a socket or a lock for it". The scan starts at 1 because
/// display 0 is the console's by convention, and the bound keeps a pathological
/// machine from turning a typo into an unbounded scan.
/// The per-display claim file Maverick uses to serialise its own creators.
///
/// Deliberately *not* `/tmp/.X<n>-lock`. An X server claims that path itself
/// with `O_EXCL`, so pre-creating it makes every spawn fail; worse, a server
/// that finds a stale lock reclaims it by replacing the file, which would
/// destroy a lock taken on that path. This is a separate file, so neither
/// happens. A zero-byte file whose name says only "a Maverick creator is
/// working on this display" is all it holds: authority is the kernel's, not the
/// file's.
fn claim_path(display: Display) -> PathBuf {
    Path::new("/tmp").join(format!(".X{}-mav", display.0))
}

/// An exclusive claim on one display number, held across select → spawn →
/// confirm.
///
/// Display selection is a stat-then-act probe, so two creators could both see
/// a number free and both spawn onto it. The X server's own `O_EXCL` lock then
/// picks a winner, but a loser had already committed: its readiness check asks
/// whether *the display* is listening, which the winner's server is, so the
/// loser recorded a dead pid and then spent the full start timeout failing.
///
/// `flock` is what makes the selection a decision rather than a guess. It is
/// released by the kernel if this process dies — including `SIGKILL` — so a
/// crashed creator cannot strand a number, which an `O_CREAT|O_EXCL` marker
/// file could. The file is left behind empty and is harmless.
///
/// Non-blocking, so a caller that loses the race moves on to the next number
/// rather than waiting: another creator is already doing this work. The mode is
/// `0600` so another user cannot hold a claim on a display from this account,
/// and gets `EACCES` instead, which is also "in use".
pub struct DisplayClaim {
    file: std::fs::File,
}

impl DisplayClaim {
    /// Try to claim `display` for exclusive use. `Ok(None)` means another
    /// creator holds it; `Err` is a real filesystem error.
    pub fn try_acquire(display: Display) -> io::Result<Option<Self>> {
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(claim_path(display))
        {
            Ok(file) => file,
            // Held by another user. Their claim is as real as ours.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
            Err(e) => return Err(e),
        };
        // `flock` through a maintained wrapper: the `EWOULDBLOCK` case is the
        // interesting one, and reading it as "another creator holds the claim"
        // rather than as a generic error is the whole point of the call.
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Self { file })),
            // EWOULDBLOCK: another creator is inside the window. Not an error.
            Err(rustix::io::Errno::AGAIN) => Ok(None),
            Err(e) => Err(std::io::Error::from_raw_os_error(e.raw_os_error())),
        }
    }
}

impl Drop for DisplayClaim {
    fn drop(&mut self) {
        // The file is closed immediately after, which releases the lock anyway,
        // so a failure here changes nothing.
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
    }
}

/// True if the X server on `display` has written its own pid into the lock.
///
/// This is how a creator knows the number is now the server's rather than its
/// own reservation: the X server publishes its pid there, and comparing it with
/// the pid we spawned is the server telling us it won. It is a fact on disk
/// rather than a sleep — the wait measured 4.4 ms to 7.0 ms from spawn to the
/// lock naming us, and a poll is the only honest way to observe it.
pub fn lock_names(display: Display, pid: u32) -> bool {
    std::fs::read_to_string(lock_path(display))
        .map(|text| text.trim().parse::<u32>() == Ok(pid))
        .unwrap_or(false)
}

/// The first free display number at or after `from`.
///
/// X display numbering is a flat namespace with no allocator: "free" means
/// "nothing has a socket or a lock for it". The scan starts at 1 because
/// display 0 is the console's by convention, and the bound keeps a pathological
/// machine from turning a typo into an unbounded scan.
///
/// Callers that can spawn should prefer [`DisplayClaim`], which makes the
/// choice exclusive rather than advisory. This stays as the scan, and a caller
/// that holds a claim knows the number is not shared.
pub fn allocate_display(from: u32) -> io::Result<Display> {
    let start = from.max(1);
    for n in start..start.saturating_add(512) {
        let d = Display(n);
        if display_is_free(d) {
            return Ok(d);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no free X display in :{start}.."),
    ))
}

/// Claim a free display for this creator, exclusively.
///
/// The scan asks [`display_is_free`], and a candidate is only taken once
/// [`DisplayClaim`] succeeds — so two creators racing the same number cannot
/// both proceed to the spawn, which is what made the loser record a dead pid
/// and then wait out a full start timeout for a window manager whose display
/// server had never existed.
///
/// The claim is held by the caller until the X server has claimed the number
/// in turn; see [`lock_names`].
pub fn claim_display(from: Display) -> io::Result<(Display, DisplayClaim)> {
    let start = from.0.max(1);
    for n in start..start.saturating_add(512) {
        let display = Display(n);
        if !display_is_free(display) {
            continue;
        }
        if let Some(claim) = DisplayClaim::try_acquire(display)? {
            return Ok((display, claim));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no free X display in :{start}.."),
    ))
}

/// True if an X server is accepting connections on `display`.
///
/// A successful `connect` is the readiness signal: the X server binds its
/// listening socket only once it is far enough into startup to serve, and it
/// keeps the connection open waiting for the client's setup request, so a
/// refusal means "not up yet" while success means "up". This needs no X client
/// library and no privileges, which matters because the session manager must
/// not depend on X11 just to wait for a server.
pub fn is_listening(display: Display) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path(display)).is_ok()
}

/// Wait until `display` accepts connections, or `timeout` elapses.
///
/// `proc_ok` answers "is the server *we* started still running". It is
/// consulted *first*, before the socket, and that ordering is the point: a
/// listening socket is a statement about whoever owns the display, which during
/// a race is the session that won it. Checking it first meant a server that had
/// died on startup was reported ready — because the *other* session's X server
/// was listening on the number both of them picked — and the record went on to
/// name a dead pid.
///
/// That ordering only closes the hole with a predicate that can see a reap. The
/// spawned child is never reaped here, so it is a zombie for the whole wait and
/// still satisfies a pid-and-start-time identity check; see [`crate::session::proc::is_running`],
/// which is what the caller must pass.
///
/// The two failure modes stay distinct: a server that is gone reports
/// `ProcessDied` rather than waiting out the full timeout to say so, because the
/// difference between the two is the difference between "try again" and "the log
/// is at <path>".
pub fn wait_ready(
    display: Display,
    timeout: Duration,
    mut proc_ok: impl FnMut() -> bool,
) -> Result<(), WaitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if !proc_ok() {
            return Err(WaitError::ProcessDied);
        }
        if is_listening(display) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(WaitError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Why [`wait_ready`] gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// The X server process exited during startup; its log has the reason.
    ProcessDied,
    /// The server is still running but never started listening.
    Timeout,
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProcessDied => write!(f, "the X server exited during startup"),
            Self::Timeout => write!(f, "the X server did not start listening in time"),
        }
    }
}

/// A running nested X server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XServer {
    /// The display the server owns.
    pub display: Display,
    /// Which implementation is running.
    pub backend: Backend,
    /// The server process, paired with its start time so every later signal is
    /// gated on the right process (see [`proc::pid_is`]).
    pub proc: ProcRef,
    /// The cookie file handed to the server with `-auth`.
    pub xauth_path: PathBuf,
}

/// Everything needed to start one nested X server.
#[derive(Debug, Clone)]
pub struct XServerSpec {
    /// Which implementation to run.
    pub backend: Backend,
    /// Display to claim.
    pub display: Display,
    /// Screen geometry in pixels.
    pub resolution: super::Resolution,
    /// Requested refresh rate in Hz. Honoured by [`Backend::Xephyr`] (whose
    /// `-screen` takes a `xDEPTHxFREQ` suffix) and ignored by
    /// [`Backend::Xvfb`], which has no equivalent switch; the caller reports
    /// the value back as declared intent either way.
    pub refresh_rate: Option<u32>,
    /// Cookie file to create and pass as `-auth`.
    pub xauth_path: PathBuf,
    /// Window title on the parent display, so several sessions are tellable
    /// apart on screen.
    pub title: String,
    /// File the server's stderr/stdout are redirected to.
    pub log_path: PathBuf,
}

/// Generate a fresh MIT-MAGIC-COOKIE-1 value as 32 lowercase hex characters.
///
/// 16 bytes from `/dev/urandom`, hex-encoded because that is the only encoding
/// every X client library (and `xauth`) accepts.
pub fn generate_cookie() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    // No clock fallback here, unlike the session id: a cookie that is not
    // unpredictable is not a cookie, so a missing entropy source must fail
    // the start rather than produce a guessable secret.
    rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty()).map_err(|e| {
        io::Error::other(format!("could not read random bytes for the X cookie: {e}"))
    })?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Write a `.Xauthority` file granting `cookie` for `display`.
///
/// The format is the X11 authority file format, written directly rather than
/// by shelling out to `xauth(1)`: it is six big-endian length-prefixed fields
/// per entry, it has no failure modes beyond a short write, and the session
/// manager would otherwise need a second binary present on the machine before
/// it can create a session at all.
///
/// Two entries are written because the X server looks the cookie up by
/// different keys depending on how the client connects:
///
/// * `FamilyLocal` (256) with the display number and no address — the local
///   unix-socket case,
/// * `FamilyWild` (65535) with neither address nor number — the fallback that
///   matches regardless of how the address is presented.
///
/// The file is created `0600` before any cookie byte is written, so the secret
/// is never briefly world-readable.
pub fn write_xauth(path: &Path, display: Display, cookie: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // Truncate/create with 0600 in one step: creating with the default mode
    // and chmod-ing afterwards leaves a window where the cookie is readable.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;

    const FAMILY_LOCAL: u16 = 256;
    const FAMILY_WILD: u16 = 0xFFFF;
    const NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";
    let data = hex_to_bytes(cookie)?;

    let mut out: Vec<u8> = Vec::with_capacity(128);
    out.extend_from_slice(&FAMILY_LOCAL.to_be_bytes());
    push_field(&mut out, b"");
    push_field(&mut out, display.0.to_string().as_bytes());
    push_field(&mut out, NAME);
    push_field(&mut out, &data);
    out.extend_from_slice(&FAMILY_WILD.to_be_bytes());
    push_field(&mut out, b"");
    push_field(&mut out, b"");
    push_field(&mut out, NAME);
    push_field(&mut out, &data);

    file.write_all(&out)?;
    file.flush()
}

/// Open a log file for appending, owner-only.
///
/// The mode is set at creation, not applied afterwards: a log that exists for a
/// moment as `0644` is a session's stderr — window names, window titles, and
/// whatever a program launched into the session wrote — visible to every user
/// on the machine for as long as the `chmod` takes.
pub fn open_private_log(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // An existing file keeps whatever mode it had, so tighten it too: a log
    // created by an older build, or by a umask, must not stay readable.
    let current = file.metadata()?.permissions().mode() & 0o777;
    if current != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Append one length-prefixed field to an authority entry.
fn push_field(out: &mut Vec<u8>, field: &[u8]) {
    // A field longer than 65535 cannot be expressed; nothing Maverick writes
    // is (the longest is the 18-byte cookie name or a display number), and
    // truncating would silently change the meaning, so clamp to the maximum
    // representable length rather than wrapping the length itself.
    let len = u16::try_from(field.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&field[..field.len().min(len as usize)]);
}

/// Decode a hex cookie into bytes. A malformed cookie is an error, never a
/// silently truncated secret.
fn hex_to_bytes(hex: &str) -> io::Result<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cookie is not an even number of hex digits",
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "cookie is not hexadecimal")
            })
        })
        .collect()
}

/// The `-screen` argument for a backend.
///
/// Xephyr's grammar is `WIDTHxHEIGHT[xDEPTHxFREQ]`, so a requested refresh
/// rate is expressed there; Xvfb's is `WIDTHxHEIGHTxDEPTH` and has no refresh
/// component at all.
fn screen_arg(backend: Backend, spec: &XServerSpec) -> String {
    let r = spec.resolution;
    match (backend, spec.refresh_rate) {
        (Backend::Xephyr, Some(hz)) => format!("{}x{}x24x{hz}", r.width, r.height),
        _ => format!("{}x{}x24", r.width, r.height),
    }
}

/// Start the nested X server described by `spec`.
///
/// The child is put in its own process group so it is not reached by a
/// terminal's `SIGINT` and so it cannot be confused with the session manager's
/// own group, and both streams go to `spec.log_path` so a failed start leaves
/// the server's own diagnosis where `maverickctl logs` can read it.
///
/// # Errors
///
/// Returns an error if the display is already claimed (a race against another
/// session manager), if the backend binary is not installed, or if the log file
/// cannot be opened.
pub fn spawn(spec: &XServerSpec) -> io::Result<XServer> {
    if !display_is_free(spec.display) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("X display {} is already in use", spec.display),
        ));
    }
    let log = open_private_log(&spec.log_path)?;

    let screen = screen_arg(spec.backend, spec);
    let mut cmd = std::process::Command::new(spec.backend.binary());
    cmd.arg(spec.display.to_string());
    match spec.backend {
        Backend::Xephyr => {
            cmd.arg("-screen").arg(&screen);
            cmd.arg("-title").arg(&spec.title);
            // The extensions Maverick's compositor and borders rely on. Naming
            // them explicitly makes a session's capability set reproducible
            // instead of dependent on a server build's defaults.
            for ext in ["RANDR", "GLX", "Composite", "DAMAGE", "RENDER", "XFIXES"] {
                cmd.arg("+extension").arg(ext);
            }
        }
        Backend::Xvfb => {
            // Xvfb takes a numbered screen argument rather than `-screen`.
            cmd.arg("-screen").arg("0").arg(&screen);
        }
    }
    cmd.arg("-auth").arg(&spec.xauth_path);
    // Never expose a session display on the network: the cookie is the only
    // thing standing between a session and every host that can reach a port.
    cmd.arg("-nolisten").arg("tcp");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(log.try_clone()?));
    cmd.stderr(Stdio::from(log));
    // Own process group: see the doc comment above.
    cmd.process_group(0);

    let child = cmd.spawn()?;
    let pid = child.id();
    // The start time is read immediately, while the pid is certainly still
    // ours. A server that exits in the same instant cannot be distinguished
    // from one that started and vanished, and both are reported the same way —
    // as "not running" — which is what a zero start time encodes, so every
    // later signal is refused and the readiness wait reports the death.
    let start_time = proc::start_time(pid).unwrap_or(0);

    Ok(XServer {
        display: spec.display,
        backend: spec.backend,
        proc: ProcRef { pid, start_time },
        xauth_path: spec.xauth_path.clone(),
    })
}

impl XServer {
    /// True if this exact server is still running.
    pub fn is_running(&self) -> bool {
        proc::pid_is(self.proc.pid, self.proc.start_time)
    }

    /// Ask the server to exit, escalating to `SIGKILL` after `grace`.
    ///
    /// `SIGTERM` first: an X server that gets `SIGKILL` skips its own cleanup
    /// and can leave a lock file behind, which would make display `N` look
    /// claimed to the next session created. The lock is removed explicitly
    /// afterwards as well, so a server that died earlier cannot hold a display
    /// number hostage either.
    pub fn stop(&self, grace: Duration) {
        if !self.is_running() {
            self.cleanup_artifacts();
            return;
        }
        // The result is deliberately not inspected: whether the signal landed
        // or the server had already exited, the next loop asks `pid_is` and
        // escalates to `SIGKILL` if it is still there. A refusal is reported
        // by the escalation below, or by the server simply outliving us.
        let _ = proc::terminate(self.proc.pid, self.proc.start_time);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !proc::pid_is(self.proc.pid, self.proc.start_time) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if self.is_running() {
            let _ = proc::kill_hard(self.proc.pid, self.proc.start_time);
            // Give the kernel a moment to close the listening socket and the
            // process to be reaped, so a display freed here is really free.
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline && is_listening(self.display) {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        self.cleanup_artifacts();
    }

    /// Release this display: remove the claim files the X server left behind.
    ///
    /// Both artefacts are removed, not just the lock. A `SIGKILL`ed server
    /// skips its own cleanup and leaves `/tmp/.X<n>-lock` *and*
    /// `/tmp/.X11-unix/X<n>` behind, and `display_is_free` treats either as
    /// claimed — so reclaiming only the lock left the number permanently
    /// unusable, with nothing able to free it.
    ///
    /// Nothing ever unlinked the socket before: `socket_path` was read only by
    /// `is_listening` and `display_is_free`, so the sole cleanup authority for
    /// `/tmp/.X11-unix/X<n>` was the X server's own exit handler, which
    /// `SIGKILL` skips — including the escalation in `stop` above.
    ///
    /// A claim is only released when nobody is serving the display. A stale
    /// claim is a file; a live one is a running server, and removing its
    /// artefacts would disarm a session that is working perfectly well. That
    /// check is also what keeps a losing creator's teardown from deleting the
    /// winner's claim, which is exactly what happened when a create lost the
    /// display race and then ran its error path.
    ///
    /// Each path is removed only if it is of the expected type and was not
    /// following a symlink: `/tmp` is world-writable, so an unlink that
    /// followed a link would let a planted link decide what gets deleted.
    fn cleanup_artifacts(&self) {
        if is_listening(self.display) {
            return;
        }
        let lock = lock_path(self.display);
        if let Ok(meta) = std::fs::symlink_metadata(&lock) {
            if meta.is_file() {
                let _ = std::fs::remove_file(&lock);
            }
        }
        let sock = socket_path(self.display);
        if let Ok(meta) = std::fs::symlink_metadata(&sock) {
            // A unix socket is a socket, not a regular file, and the type check
            // is what distinguishes "a dead server's socket" from anything a
            // planted symlink or regular file would present as.
            if meta.file_type().is_socket() {
                let _ = std::fs::remove_file(&sock);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Resolution;

    #[test]
    fn display_round_trips_through_its_string_form() {
        for n in [0u32, 1, 7, 99, 512] {
            let d = Display(n);
            assert_eq!(Display::parse(&d.to_string()), Some(d));
        }
        assert_eq!(Display::parse(":1.0"), Some(Display(1)));
    }

    /// Only a local display is a display this manager could have created. A
    /// remote one (`host:1`) must be refused rather than silently reduced to
    /// the number, which would point at a completely different session.
    #[test]
    fn only_local_displays_are_accepted() {
        assert_eq!(Display::parse("host:1"), None);
        assert_eq!(Display::parse("unix:1"), None);
        assert_eq!(Display::parse("1"), None);
        assert_eq!(Display::parse(":"), None);
        assert_eq!(Display::parse(":x"), None);
        assert_eq!(Display::parse(""), None);
    }

    #[test]
    fn backends_parse_and_report_themselves() {
        assert_eq!(Backend::parse("Xephyr"), Some(Backend::Xephyr));
        assert_eq!(Backend::parse("xvfb"), Some(Backend::Xvfb));
        assert_eq!(Backend::parse("headless"), Some(Backend::Xvfb));
        assert_eq!(Backend::parse("wayland"), None);
        assert_eq!(Backend::default(), Backend::Xephyr);
        assert_eq!(Backend::Xephyr.binary(), "Xephyr");
    }

    /// The cookie is a secret: it must be 16 bytes of entropy, hex-encoded, and
    /// two sessions must never share one.
    #[test]
    fn cookies_are_sixteen_bytes_of_entropy() {
        let a = generate_cookie().expect("urandom");
        let b = generate_cookie().expect("urandom");
        assert_eq!(a.len(), 32, "16 bytes hex-encoded");
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    /// The authority file is what the X server actually reads, so its exact
    /// bytes are a contract. The expected layout is spelled out here rather
    /// than round-tripped, so a change in the writer cannot quietly change the
    /// file.
    #[test]
    fn the_authority_file_has_the_documented_byte_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Xauthority");
        let secret: [u8; 16] = [
            0xde, 0xad, 0xbe, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23,
            0x45, 0x67,
        ];
        write_xauth(&path, Display(99), "deadbeef0123456789abcdef01234567").expect("write");

        let entry = |family: u16, fields: &[&[u8]]| {
            let mut out = family.to_be_bytes().to_vec();
            for f in fields {
                out.extend_from_slice(&(f.len() as u16).to_be_bytes());
                out.extend_from_slice(f);
            }
            out
        };
        let mut want = entry(256, &[b"", b"99", b"MIT-MAGIC-COOKIE-1", &secret]);
        want.extend(entry(0xFFFF, &[b"", b"", b"MIT-MAGIC-COOKIE-1", &secret]));

        assert_eq!(std::fs::read(&path).expect("read back"), want);
    }

    /// The cookie must never be briefly world-readable: the file is created
    /// with its final mode, not chmod-ed into place.
    #[cfg(unix)]
    #[test]
    fn the_authority_file_is_private_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Xauthority");
        write_xauth(&path, Display(1), "00112233445566778899aabbccddeeff").expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the cookie file must be owner-only");
    }

    /// A cookie that is not hex must be refused, not written as text: an
    /// X server would reject it and the failure would look like a permissions
    /// problem much later.
    #[test]
    fn a_malformed_cookie_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Xauthority");
        assert!(write_xauth(&path, Display(1), "abc").is_err());
        assert!(write_xauth(&path, Display(1), "zzzz").is_err());
    }

    /// A requested refresh rate is Xephyr's `-screen` suffix, and Xvfb has no
    /// equivalent — which is a documented limitation, not something to fake.
    #[test]
    fn screen_args_carry_the_refresh_rate_only_where_it_is_real() {
        let base = XServerSpec {
            backend: Backend::Xephyr,
            display: Display(1),
            resolution: Resolution::new(1280, 720).expect("resolution"),
            refresh_rate: None,
            xauth_path: PathBuf::from("/tmp/x"),
            title: "t".into(),
            log_path: PathBuf::from("/tmp/l"),
        };
        assert_eq!(screen_arg(Backend::Xephyr, &base), "1280x720x24");
        let with_hz = XServerSpec {
            refresh_rate: Some(120),
            ..base.clone()
        };
        assert_eq!(screen_arg(Backend::Xephyr, &with_hz), "1280x720x24x120");
        let xvfb = XServerSpec {
            backend: Backend::Xvfb,
            ..with_hz
        };
        assert_eq!(
            screen_arg(Backend::Xvfb, &xvfb),
            "1280x720x24",
            "Xvfb has no refresh switch; silently claiming one would be a lie"
        );
    }

    /// Allocation must never hand out display 0, and must respect a starting
    /// point so a caller can retry a specific number.
    #[test]
    fn allocation_starts_at_one_and_honours_a_hint() {
        // On a machine with :0 taken (the normal case) the first allocation is
        // at or above 1; the exact value depends on the machine, so the
        // invariant is the one that must hold everywhere.
        if let Ok(d) = allocate_display(0) {
            assert!(d.0 >= 1, "display 0 belongs to the console");
        }
        if let Ok(d) = allocate_display(400) {
            assert!(d.0 >= 400, "the hint must be honoured");
        }
    }

    /// The readiness wait asks two questions — "is it listening yet" and "is it
    /// still alive" — and the second one decides *which failure* is reported.
    /// A predicate that answers the second question backwards turns a perfectly
    /// healthy server into an immediate "it exited", which is what a real
    /// session start reported before the predicate was pinned here.
    #[test]
    fn the_readiness_wait_reports_a_dead_server_and_keeps_waiting_for_a_live_one() {
        let free = allocate_display(300).expect("a free display");
        assert!(
            !is_listening(free),
            "the test needs a display nothing is on"
        );
        let start = std::time::Instant::now();
        // A server that is alive is waited for, and times out.
        let err = wait_ready(free, Duration::from_millis(150), || true)
            .expect_err("nothing will ever be listening");
        assert_eq!(err, WaitError::Timeout);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "it must have waited"
        );
        // A server that died is reported as dead, and immediately.
        let start = std::time::Instant::now();
        let err = wait_ready(free, Duration::from_secs(30), || false)
            .expect_err("a dead server never becomes ready");
        assert_eq!(err, WaitError::ProcessDied);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a dead server must be reported at once, not after the timeout"
        );
    }

    /// The whole readiness defect in one test: a display that *is* being served,
    /// and a server that is not running.
    ///
    /// The test above cannot catch this, because it uses a display nothing is
    /// listening on — where a listening socket, whoever's, adds nothing. During
    /// a create race the display *is* serving, by the session that won the
    /// number, and asking about it first is what let the loser record a dead
    /// pid. Nothing here needs an X server: a plain listener is a listener.
    #[test]
    fn a_dead_server_is_not_ready_just_because_its_display_is_serving() {
        use std::os::unix::net::UnixListener;
        let free = allocate_display(320).expect("a free display");
        let _listener = UnixListener::bind(socket_path(free)).expect("stand in for a winner");
        assert!(is_listening(free), "the display must be serving");
        let start = std::time::Instant::now();
        let err = wait_ready(free, Duration::from_secs(30), || false)
            .expect_err("our server is not running, so it is not ready");
        assert_eq!(
            err,
            WaitError::ProcessDied,
            "a serving display must not make our own dead server look ready"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "it must be reported at once, not after the timeout"
        );
        release_display(free);
    }

    /// A healthy server is still reported ready, and still immediately.
    ///
    /// The counterpart to the test above, so the reordering cannot be "fixed"
    /// by never reporting readiness at all.
    #[test]
    fn a_live_server_on_a_serving_display_is_ready_at_once() {
        use std::os::unix::net::UnixListener;
        let free = allocate_display(321).expect("a free display");
        let _listener = UnixListener::bind(socket_path(free)).expect("stand in for a winner");
        let start = std::time::Instant::now();
        wait_ready(free, Duration::from_secs(5), || true).expect("live and serving");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a live server must not pay the poll interval for a display that is up"
        );
        release_display(free);
    }

    /// Exactly one holder at a time, decided by the kernel rather than by a
    /// sleep. Sixteen contenders is well past the two that would show a
    /// check-then-act bug.
    #[test]
    fn only_one_creator_holds_a_display_claim() {
        let mine = DisplayClaim::try_acquire(Display(330))
            .expect("open the claim file")
            .expect("nothing else holds it");
        let display = Display(330);
        let taken = DisplayClaim::try_acquire(display).expect("a second open is not an error");
        assert!(
            taken.is_none(),
            "a display already claimed must not be claimable again"
        );
        drop(mine);
        assert!(
            DisplayClaim::try_acquire(display)
                .expect("reopen after release")
                .is_some(),
            "releasing the claim must make the display available again"
        );
    }

    /// Releasing a display must not disturb a server that is still serving it.
    ///
    /// The ownership check, not the liveness of the handle: a creator that
    /// lost a race holds a handle whose process is dead, and tearing that down
    /// used to delete the *winner's* claim, disarming a working session.
    #[test]
    fn a_dead_handle_does_not_release_a_live_servers_display() {
        use std::os::unix::net::UnixListener;
        let free = allocate_display(340).expect("a free display");
        let listener = UnixListener::bind(socket_path(free)).expect("a live server stands in");
        // A handle whose pid is not running at all — the losing creator.
        let dead = XServer {
            display: free,
            backend: super::Backend::Xvfb,
            proc: super::ProcRef {
                pid: u32::MAX - 1,
                start_time: 1,
            },
            xauth_path: std::path::PathBuf::from("/nonexistent"),
        };
        dead.stop(Duration::from_millis(50));
        assert!(
            is_listening(free),
            "another server owns this display and must be left alone"
        );
        release_display(free);
        drop(listener);
    }

    /// Remove the socket file a test listener left behind.
    ///
    /// Dropping a `UnixListener` closes the socket but does not unlink the file,
    /// and `display_is_free` treats the file as a claim — so without this a test
    /// that binds one fails on its second run with `AddrInUse`, or quietly makes
    /// the next allocation skip the number.
    fn release_display(display: Display) {
        let _ = std::fs::remove_file(socket_path(display));
        let _ = std::fs::remove_file(lock_path(display));
    }

    /// The handover that ends the display claim is the X server naming itself,
    /// not the passage of time.
    ///
    /// Written against a planted lock rather than a real server, so it asserts
    /// the comparison itself: the pid in the lock is the only thing that makes
    /// the number ours, and any other value must not.
    #[test]
    fn only_the_pid_in_the_lock_marks_the_display_as_ours() {
        let display = Display(350);
        let path = lock_path(display);
        // The X server writes its pid right-aligned and newline-terminated,
        // which is the format the comparison has to cope with.
        std::fs::write(&path, format!("{:>10}\n", 4242)).expect("plant a lock file");
        assert!(
            lock_names(display, 4242),
            "the server that wrote its own pid into the lock owns the number"
        );
        assert!(
            !lock_names(display, 4243),
            "any other pid must not read as ours, or the claim is released early"
        );
        std::fs::write(&path, "not a number\n").expect("plant a junk lock file");
        assert!(
            !lock_names(display, 4242),
            "an unparsable lock is not a handover"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A display that is already claimed must be refused, not hijacked: this is    /// the check that stops two sessions landing on one display.
    #[test]
    fn spawning_onto_a_claimed_display_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let xauth = dir.path().join("Xauthority");
        write_xauth(&xauth, Display(0), &generate_cookie().expect("cookie")).expect("write");
        let spec = XServerSpec {
            backend: Backend::Xephyr,
            display: Display(0), // the console's display, always taken
            resolution: Resolution::new(800, 600).expect("resolution"),
            refresh_rate: None,
            xauth_path: xauth,
            title: "t".into(),
            log_path: dir.path().join("x.log"),
        };
        let err = spawn(&spec).expect_err("display 0 must be refused");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
}
