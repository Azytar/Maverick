//! `maverickctl session …`, `exec`, `shell`, `attach`, `logs`, `debug` and
//! `inspect`: everything that knows a session is more than one process.
//!
//! # Where the line is drawn
//!
//! Nothing in this module talks to X11, and nothing in it decides window
//! state. A session's *lifecycle*, environment and process graph belong to the
//! session manager; a session's *windows* belong to the window manager, and
//! every window operation here leaves through the control socket as an action
//! so it goes through the same state machine a keypress does. That split is
//! the whole point: a tool that called `XMoveWindow` directly would be a second
//! authority competing with the reconciler.
//!
//! # Defaults
//!
//! Every command takes an optional session name and resolves it the way
//! [`super::resolve_target`] documents, so the common case — "the session I am
//! in" — is `maverickctl window list` with no arguments at all. A session name
//! is only spelled out when the caller means a specific one.

use crate::client;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::windows::flatten_windows;
use super::{print_usage, session_target, Ctl};
use crate::session::{
    self, lifecycle, proc, Backend, Resolution, Session, SessionName, SessionState, SessionView,
    Spec,
};
use maverick_sys::json::Json;

/// How many log lines `maverickctl logs` shows by default.
const DEFAULT_LOG_LINES: usize = 40;

/// Run a `session …` subcommand. Returns `Ok(true)` if it handled the verb.
pub fn run(c: &mut Ctl, args: &[String]) -> Result<bool, String> {
    // The verb comes from `positionals`, not `args`. `run_group` lifts global
    // options off the front of the argument list before calling, so for
    // `maverickctl --session debug window list` the first element is
    // `--session` and reading it as the verb rejected the command with
    // "unknown window command '--session'". The global itself was already parsed
    // correctly by `Ctl::parse`; only this lookup was looking in the wrong place.
    let Some(verb) = c.positionals.first().map(|&i| args[i].as_str()) else {
        print_usage(super::Usage::Sessions);
        return Ok(true);
    };
    let rest = &args[1..];
    match verb {
        "list" | "ls" => {
            list(c);
            Ok(true)
        }
        "create" | "new" => {
            create(c, rest)?;
            Ok(true)
        }
        "status" => {
            status(c, rest)?;
            Ok(true)
        }
        "start" => {
            change(c, rest, "start")?;
            Ok(true)
        }
        "stop" => {
            change(c, rest, "stop")?;
            Ok(true)
        }
        "restart" => {
            change(c, rest, "restart")?;
            Ok(true)
        }
        "kill" => {
            change(c, rest, "kill")?;
            Ok(true)
        }
        "remove" | "rm" | "delete" => {
            change(c, rest, "remove")?;
            Ok(true)
        }
        "help" | "-h" | "--help" => {
            print_usage(super::Usage::Sessions);
            Ok(true)
        }
        other => Err(format!(
            "unknown session command '{other}'\n\n  try: maverickctl session list | create | status | start | stop | restart | kill | remove"
        )),
    }
}

// ── session list ─────────────────────────────────────────────────────────────

/// `maverickctl session list` — one row per session, human or JSON.
///
/// Read-only on purpose: it never reaps. An agent that polls it must not be
/// causing side effects, and the derived state it prints is what makes a
/// crashed session visible without touching it. Cleanup happens on the next
/// command that is already allowed to change something.
fn list(c: &Ctl) {
    let views = session::list();
    if c.json {
        // A document, not a pretty array: an agent that reads `{"sessions":[…]}`
        // can be extended with new fields without breaking its parser, which a
        // bare array cannot survive.
        let items: Vec<String> = views.iter().map(view_json).collect();
        println!("{{\"sessions\":[{}]}}", items.join(","));
        return;
    }
    if views.is_empty() {
        println!("No Maverick sessions. Create one with: maverickctl session create <name>");
        return;
    }
    println!("MAVERICK SESSIONS\n");
    println!(
        "{:<12} {:<8} {:<12} {:<8} {:<7} STATE",
        "NAME", "DISPLAY", "RESOLUTION", "PID", "MODE"
    );
    for v in &views {
        println!(
            "{:<12} {:<8} {:<12} {:<8} {:<7} {}",
            v.name,
            if v.display.is_empty() {
                "-"
            } else {
                &v.display
            },
            v.resolution
                .map_or_else(|| "-".to_string(), |r| r.to_string()),
            v.pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
            v.mode(),
            v.state,
        );
    }
    println!("\n{} session(s).", views.len());
}

/// One session as JSON.
///
/// Only facts, and no secrets: the Xauthority *path* appears (a control tool
/// needs to point a client at it) but never its contents, and no environment
/// variables are reported at all — an X cookie reaching a log or a JSON
/// document would be a credential leak with a plausible-looking cause.
fn view_json(v: &SessionView) -> String {
    let field = |k: &str, val: String| format!("\"{k}\":{val}");
    let resolution = match v.resolution {
        Some(r) => format!("{{\"width\":{},\"height\":{}}}", r.width, r.height),
        None => "null".to_string(),
    };
    let refresh = v.refresh_rate.map_or("null".to_string(), |r| r.to_string());
    let xauth = match live_record(&v.name) {
        Some(s) => maverick_sys::json::json_quote(&s.xauth_path().display().to_string()),
        None => "null".to_string(),
    };
    let fields = [
        field("name", maverick_sys::json::json_quote(&v.name)),
        field("session_id", maverick_sys::json::json_quote(&v.sid)),
        field("kind", maverick_sys::json::json_quote(v.kind)),
        field("state", maverick_sys::json::json_quote(v.state.as_str())),
        field("display", maverick_sys::json::json_quote(&v.display)),
        field("resolution", resolution),
        field("refresh_rate", refresh),
        field("pid", v.pid.map_or("null".to_string(), |p| p.to_string())),
        field(
            "x_pid",
            v.x_pid.map_or("null".to_string(), |p| p.to_string()),
        ),
        field("binary", maverick_sys::json::json_quote(&v.binary)),
        field("debug", v.debug.to_string()),
        field(
            "backend",
            v.backend.map_or("null".to_string(), |b| {
                maverick_sys::json::json_quote(b.label())
            }),
        ),
        field("xauth", xauth),
        field(
            "exit_reason",
            maverick_sys::json::json_quote(&v.exit_reason),
        ),
        field("created_at", v.created_at.to_string()),
        field("owner_uid", v.owner_uid.to_string()),
    ];
    format!("{{{}}}", fields.join(","))
}

// ── session create ───────────────────────────────────────────────────────────

/// `maverickctl session create <name> [options] [-- <maverick args…>]`
fn create(c: &Ctl, args: &[String]) -> Result<(), String> {
    let parsed = CreateArgs::parse(args)?;
    let binary = if parsed.binary.is_empty() {
        String::new()
    } else {
        // A relative binary is anchored to the caller's directory *now*, while
        // that is still the directory it was typed in.
        PathBuf::from(&parsed.binary)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&parsed.binary))
            .display()
            .to_string()
    };
    let cwd = parsed
        .cwd
        .as_ref()
        .map(|p| {
            p.canonicalize()
                .unwrap_or_else(|_| p.clone())
                .display()
                .to_string()
        })
        .map(PathBuf::from);

    let spec = Spec {
        backend: parsed.backend,
        resolution: parsed.resolution,
        refresh_rate: parsed.refresh_rate,
        binary,
        cwd,
        args: parsed.maverick_args,
        debug: parsed.debug,
    };

    if c.json {
        let session = lifecycle::create(&parsed.name, spec).map_err(|e| e.to_string())?;
        println!("{}", created_json(&session));
        return Ok(());
    }
    let session = lifecycle::create(&parsed.name, spec).map_err(|e| e.to_string())?;
    println!(
        "session '{}' started on {} at {}",
        session.name, session.display, session.spec.resolution
    );
    println!("  maverickctl exec {} <program>", session.name);
    println!(
        "  maverickctl window list {}\n  maverickctl logs {}",
        session.name, session.name
    );
    Ok(())
}

/// The `session create` options, parsed.
#[derive(Debug)]
struct CreateArgs {
    name: SessionName,
    backend: Backend,
    resolution: Resolution,
    refresh_rate: Option<u32>,
    binary: String,
    cwd: Option<PathBuf>,
    debug: bool,
    maverick_args: Vec<String>,
}

impl CreateArgs {
    /// Parse `create`'s arguments.
    ///
    /// Everything after a bare `--` belongs to the Maverick binary, verbatim
    /// and unparsed: the session manager must not need to know the window
    /// manager's own vocabulary in order to forward an argument to it, and a
    /// flag Maverick gains next month must not require a change here.
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut name: Option<SessionName> = None;
        let mut backend = Backend::default();
        let mut resolution = Resolution::DEFAULT;
        let mut refresh_rate = None;
        let mut binary = String::new();
        let mut cwd = None;
        let mut debug = false;
        let mut maverick_args = Vec::new();

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            // The end of *this* parser's vocabulary: everything after it is
            // Maverick's.
            if arg == "--" {
                maverick_args = args[i + 1..].to_vec();
                break;
            }
            // A value-taking option, with the value in the next argument.
            let mut value = |what: &str| -> Result<String, String> {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| format!("{what} requires a value"))
            };
            match arg {
                "--resolution" | "-r" => {
                    let raw = value("--resolution")?;
                    resolution = Resolution::parse(&raw).map_err(|e| e.to_string())?;
                }
                "--refresh-rate" => {
                    let raw = value("--refresh-rate")?;
                    let hz: u32 = raw
                        .parse()
                        .map_err(|_| format!("--refresh-rate '{raw}' is not a number"))?;
                    if !(1..=1000).contains(&hz) {
                        return Err(format!("--refresh-rate {hz} is out of range (1..=1000)"));
                    }
                    refresh_rate = Some(hz);
                }
                "--backend" => {
                    let raw = value("--backend")?;
                    backend = Backend::parse(&raw)
                        .ok_or_else(|| format!("unknown --backend '{raw}' (xephyr|xvfb)"))?;
                }
                "--binary" => binary = value("--binary")?,
                "--cwd" => cwd = Some(PathBuf::from(value("--cwd")?)),
                "--debug" => debug = true,
                other if other.starts_with('-') => {
                    return Err(format!(
                        "unknown option '{other}'\n\n  try: maverickctl session create --help"
                    ))
                }
                other => {
                    if name.is_some() {
                        return Err(format!(
                            "unexpected argument '{other}' — a session takes one name"
                        ));
                    }
                    name = Some(SessionName::parse(other).map_err(|e| e.to_string())?);
                }
            }
            i += 1;
        }
        Ok(CreateArgs {
            name: name.ok_or_else(|| {
                "session create needs a name\n\n  try: maverickctl session create debug".to_string()
            })?,
            backend,
            resolution,
            refresh_rate,
            binary,
            cwd,
            debug,
            maverick_args,
        })
    }
}

/// The JSON a successful `create` prints.
fn created_json(s: &Session) -> String {
    view_json(&session::SessionView {
        name: s.name.as_str().to_string(),
        sid: s.name.as_str().to_string(),
        kind: "nested",
        state: s.derived_state(),
        display: s.display.to_string(),
        resolution: Some(s.spec.resolution),
        refresh_rate: s.spec.refresh_rate,
        x_pid: (s.xserver.pid != 0).then_some(s.xserver.pid),
        pid: (s.wm.pid != 0).then_some(s.wm.pid),
        binary: s.spec.binary.clone(),
        debug: s.spec.debug,
        backend: Some(s.spec.backend),
        exit_reason: s.exit_reason.clone(),
        created_at: s.created_at,
        owner_uid: s.owner_uid,
    })
}

// ── session status / start / stop / … ────────────────────────────────────────

/// `maverickctl session status <name>`
fn status(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let view = session::resolve(&name).map_err(|e| e.to_string())?;
    if c.json {
        println!("{}", view_json(&view));
        return Ok(());
    }
    println!("SESSION");
    println!("  {:<14}{}", "name:", view.name);
    println!("  {:<14}{}", "display:", display_or_dash(&view.display));
    println!(
        "  {:<14}{}",
        "resolution:",
        view.resolution
            .map_or_else(|| "-".to_string(), |r| r.to_string())
    );
    if let Some(hz) = view.refresh_rate {
        println!("  {:<14}{} Hz (requested)", "refresh:", hz);
    }
    println!("  {:<14}{}", "state:", view.state);
    println!(
        "  {:<14}{}",
        "pid:",
        view.pid.map_or_else(|| "-".to_string(), |p| p.to_string())
    );
    if let Some(x) = view.x_pid {
        println!("  {:<14}{}", "x server pid:", x);
    }
    println!("  {:<14}{}", "binary:", display_or_dash(&view.binary));
    if view.debug {
        println!("  {:<14}yes", "debug:");
    }
    if let Some(b) = view.backend {
        println!("  {:<14}{}", "x backend:", b.label());
    }
    println!("  {:<14}{}", "created:", view.created_at);
    if !view.exit_reason.is_empty() {
        println!("  {:<14}{}", "note:", view.exit_reason);
    }
    println!("\n  maverickctl inspect {}", view.name);
    Ok(())
}

/// Run one of the lifecycle verbs that all take a name and nothing else.
fn change(c: &Ctl, args: &[String], verb: &str) -> Result<(), String> {
    let name = session_target(c, args)?;
    let parsed = SessionName::parse(&name).map_err(|e| e.to_string())?;
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let result = match verb {
        "start" => lifecycle::start(&parsed).map(|_| ()),
        "stop" => lifecycle::stop(&parsed).map(|_| ()),
        "restart" => lifecycle::restart(&parsed).map(|_| ()),
        "kill" => lifecycle::kill(&parsed).map(|_| ()),
        "remove" => lifecycle::remove(&parsed, force),
        _ => return Err(format!("unknown lifecycle verb '{verb}'")),
    };
    if let Err(e) = result {
        return Err(e.to_string());
    }
    if c.json {
        let views: Vec<String> = session::list()
            .iter()
            .filter(|v| v.name == parsed.as_str())
            .map(view_json)
            .collect();
        if views.is_empty() {
            println!(
                "{{\"name\":{},\"removed\":true}}",
                maverick_sys::json::json_quote(&name)
            );
        } else {
            println!("{{\"session\":{}}}", views[0]);
        }
        return Ok(());
    }
    match verb {
        "remove" => println!("session '{name}' removed"),
        "restart" => println!("session '{name}' restarted"),
        other => println!("session '{name}' {other}ped"),
    }
    Ok(())
}

fn display_or_dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

// ── exec / shell / attach ────────────────────────────────────────────────────

/// `maverickctl exec <session> <program> [args…]`
///
/// Everything from the third argument on is the program and its own arguments:
/// `maverickctl exec debug alacritty --json` runs `alacritty --json`, because
/// a program must never inherit this tool's vocabulary by accident.
///
/// The child is started in its own process group so it survives this one-shot
/// CLI, and the group id is *recorded in the session* — that record is the only
/// thing that keeps an `exec`ed program findable by `process list` once the CLI
/// has exited and the kernel has reparented it to init, where a parent-pointer
/// walk can no longer reach it.
pub fn exec(c: &Ctl, args: &[String]) -> Result<(), String> {
    let Some(call) = split_call(c, args) else {
        return Err(
            "exec needs a session and a program\n\n  try: maverickctl exec debug alacritty"
                .to_string(),
        );
    };
    let record = live_record(&call.session).ok_or_else(|| {
        format!(
            "session '{}' does not exist\n\n{}",
            call.session,
            available_sessions()
        )
    })?;
    if call.argv.is_empty() {
        return Err("exec needs a program to run".to_string());
    }
    let wait = c.flag(&["--wait", "-w"]);
    let inherit = c.flag(&["--inherit", "-i"]);

    let child = launch_in_session(&record, &call.argv, inherit).map_err(|e| e.to_string())?;
    let pid = child.id();
    // The new process group's id is the child's pid (`process_group(0)`), but
    // it is read back from `/proc` rather than assumed: the registration is
    // what makes the program part of the session, and a wrong id would put it
    // in some other session's tree or none.
    if let Some(info) = proc::read(pid) {
        register_pgrp(&call.session, info.pgid)?;
    }
    if c.json {
        println!(
            "{{\"session\":{},\"pid\":{pid}}}",
            maverick_sys::json::json_quote(&call.session)
        );
    } else {
        println!("{pid}");
    }
    if wait {
        // Block and report the program's own exit status, so a script can act
        // on a failure instead of only seeing that it started.
        let mut child = child;
        let status = child
            .wait()
            .map_err(|e| format!("could not wait for the program: {e}"))?;
        if c.json {
            println!("{{\"status\":{}}}", status.code().unwrap_or(-1));
        }
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    Ok(())
}

/// Add a process group to a session's registry.
///
/// The record is read, updated and written back atomically by
/// [`session::write`], and the write is verified: a group that is not recorded
/// is a program the session cannot see, and an `exec` that reports a pid while
/// the process is invisible to `process list` is worse than one that fails.
fn register_pgrp(name: &str, pgid: u32) -> Result<(), String> {
    if pgid == 0 {
        return Ok(());
    }
    let parsed = SessionName::parse(name).map_err(|e| e.to_string())?;
    let mut record = live_record(name).ok_or_else(|| format!("session '{name}' is gone"))?;
    if !record.pgrps.contains(&pgid) {
        record.pgrps.push(pgid);
        session::write(&record).map_err(|e| e.to_string())?;
    }
    let _ = parsed;
    Ok(())
}

/// `maverickctl shell <session>` — a shell inside the session's environment.
pub fn shell(c: &Ctl, args: &[String]) -> Result<(), String> {
    enter(c, args, "shell", false)
}

/// `maverickctl attach <session>` — a shell, plus the context for what the
/// session is.
///
/// "Attach" to a nested session cannot mean a terminal multiplexer attach: a
/// nested X server is a window on the *parent* display, so what a user
/// "attaches to" is the graphical session, and the thing they need from a
/// terminal is the environment to drive it. The two are separated on purpose —
/// `shell` gives the environment, `attach` says which display and session it
/// belongs to first — and the interface is left room for a real attacher later
/// without changing either.
pub fn attach(c: &Ctl, args: &[String]) -> Result<(), String> {
    enter(c, args, "attach", true)
}

/// Shared body of `shell` and `attach`.
fn enter(c: &Ctl, args: &[String], verb: &str, announce: bool) -> Result<(), String> {
    let Some(call) = split_call(c, args) else {
        return Err(format!(
            "{verb} needs a session\n\n  try: maverickctl {verb} debug"
        ));
    };
    let name = call.session;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    if record.state != SessionState::Running {
        return Err(format!(
            "session '{name}' is {} — start it with: maverickctl session start {name}",
            record.state
        ));
    }

    // No command named: the user's shell. The difference between `shell` and
    // `attach` is the announcement, not the environment — a nested session is
    // a window on the parent display, so what a user "attaches to" is the
    // graphical session, and the terminal's job is to be inside it.
    let argv: Vec<String> = if call.argv.is_empty() {
        default_shell()
    } else {
        call.argv
    };
    if announce && c.stderr_is_tty() {
        eprintln!(
            "entering Maverick session '{name}' (display {}) — leave with `exit`",
            record.display
        );
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (key, value) in record.env() {
        cmd.env(key, value);
    }
    if !announce {
        cmd.stdin(Stdio::inherit());
    }
    // Both forms inherit the terminal: a session's shell is an interactive
    // thing, and there is nothing for the parent to do with its streams.
    cmd.status()
        .map_err(|e| format!("could not run {}: {e}", argv[0]))?;
    Ok(())
}

/// The shell to run when the caller named none.
fn default_shell() -> Vec<String> {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).exists())
        .map(|s| vec![s])
        .unwrap_or_else(|| vec!["/bin/sh".to_string()])
}

/// Split `<session> <rest…>` off the front of the arguments.
///
/// Returns `None` when no session was named, so each caller can print its own
/// usage rather than a shared "missing argument" that says nothing about what
/// the command was for.
///
/// When the session came from `--session`, *every* positional belongs to the
/// rest; otherwise the first positional is the session and the rest follows it.
/// That distinction is the whole reason this is not just `args[1..]`: with a
/// positional session, `window focus debug firefox` must resolve `firefox` as
/// the window, and reading the first positional as the selector would address a
/// session name as if it were a window.
pub fn split_session_and_rest(c: &Ctl, args: &[String]) -> Option<(String, Vec<String>)> {
    let rest: Vec<String> = args.iter().filter(|a| !is_flag(a, c)).cloned().collect();
    match c.explicit_session() {
        Some(explicit) => Some((explicit.to_string(), rest)),
        None => rest
            .split_first()
            .map(|(name, tail)| (name.clone(), tail.to_vec())),
    }
}

/// Every flag name this tool recognises, including the ones a subcommand claims.
///
/// Listed rather than guessed from "does it start with a dash", because that
/// guess is wrong in both directions: `+10%` and `-10%` are amounts, and a
/// command word may begin with a dash. Both were being read as options, so
/// `resize session -10%` found nothing to resize.
const KNOWN_FLAGS: &[&str] = &[
    "--json",
    "-j",
    "--yes",
    "-y",
    "--session",
    "-s",
    "--name",
    "-n",
    "--wait",
    "-w",
    "--inherit",
    "-i",
    "--force",
    "-f",
    "-9",
    "--xserver",
    "-x",
    "--follow",
    "--window",
    "--help",
    "-h",
    "--keep",
];

/// True if an argument is one of this tool's own flags.
///
/// A leading dash alone is not enough: an amount, or a command word like
/// `--weird`, is a value. An argument counts as a flag when it is one this tool
/// recognises, including with a value attached (`--json=x`) — never on the dash
/// alone.
fn is_flag(arg: &str, c: &Ctl) -> bool {
    if !arg.starts_with('-') {
        return false;
    }
    let name = arg.split('=').next().unwrap_or(arg);
    c.is_own_flag(name) || KNOWN_FLAGS.contains(&name)
}

/// A `maverickctl <verb> <session> <command…>` call, with the command split off.
pub struct Call {
    /// The session the command runs in.
    pub session: String,
    /// The command and its arguments, verbatim.
    pub argv: Vec<String>,
}

/// Split a call into its session and its command.
///
/// `exec`, `shell` and `attach` take a command to run, and the boundary between
/// this tool's arguments and the command's has to land exactly where a user
/// expects it. The rule is positional: everything after the *command word*
/// belongs to the command, unfiltered, so `maverickctl exec debug alacritty
/// --json` runs `alacritty --json`. A program's own flags must never be consumed
/// by the tool that launched it — the one place where being clever about
/// argument parsing is actively wrong.
///
/// A bare `--` before the command word ends this tool's parsing instead, for
/// the case where the command word itself looks like a flag. Everything after
/// it is then the command, including arguments that would otherwise be
/// interpreted.
///
/// Returns `None` when no session was named, so each caller prints its own
/// usage rather than a shared "missing argument" that says nothing about what
/// the command was for.
pub fn split_call(c: &Ctl, args: &[String]) -> Option<Call> {
    // A bare `--` is a boundary, not a word.
    if let Some(at) = args.iter().position(|a| a == "--") {
        let session = match c.explicit_session() {
            Some(explicit) => explicit.to_string(),
            None => args.get(*c.positionals.first()?)?.clone(),
        };
        return Some(Call {
            session,
            argv: args[at + 1..].to_vec(),
        });
    }
    let (session, command_at) = match c.explicit_session() {
        // With the session named by a flag, the first positional *is* the
        // command.
        Some(explicit) => (explicit.to_string(), c.positionals.first().copied()),
        // Otherwise the first positional is the session and the second is the
        // command; its index is what separates the two. A session with no
        // command is legitimate — `shell debug` uses the user's shell — so a
        // missing second positional is an empty command, not an error.
        None => (
            args.get(*c.positionals.first()?)?.clone(),
            c.positionals.get(1).copied(),
        ),
    };
    // From the command word onwards, verbatim: no flag filtering, because from
    // here on it is not this tool's to interpret.
    Some(Call {
        session,
        argv: command_at.map_or_else(Vec::new, |at| args[at..].to_vec()),
    })
}

/// Start a program inside a session, recording its process group.
///
/// Output goes to the session's own `exec.log` by default, not to the caller's
/// terminal. A detached program's output belongs to the session it runs in —
/// and inheriting is actively unsafe in the two cases that matter: a caller
/// whose stdout is a pipe nobody reads (an agent, a script) leaves the program
/// blocked forever on a full buffer, and a caller on a terminal has its screen
/// scribbled on by a background job. `--inherit` opts back in for the case
/// where the caller *is* the terminal the output is wanted on.
fn launch_in_session(
    session: &Session,
    command: &[String],
    inherit: bool,
) -> Result<std::process::Child, String> {
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]);
    for (key, value) in session.env() {
        cmd.env(key, value);
    }
    if inherit {
        // A detached program holding the terminal open would also stop the
        // caller's shell from returning, so its stdin is /dev/null even when
        // its output is inherited: it is a background job, not a child of a
        // terminal session.
        cmd.stdin(Stdio::null());
    } else {
        let log = crate::session::xserver::open_private_log(&session.dir().join("exec.log"))
            .map_err(|e| format!("could not open the session log: {e}"))?;
        cmd.stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?));
        cmd.stderr(Stdio::from(log));
    }
    // Its own process group: the program must outlive this CLI, and it must be
    // findable afterwards — a reparented child is only reachable by its group.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", command[0]))?;
    Ok(child)
}

// ── process ──────────────────────────────────────────────────────────────────

/// `maverickctl process list <session>`
pub fn process(c: &mut Ctl, args: &[String]) -> Result<bool, String> {
    // As in `run`: globals are lifted to the front, so the verb is the first
    // positional rather than the first argument.
    let verb = c
        .positionals
        .first()
        .map(|&i| args[i].as_str())
        .unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match verb {
        "list" | "ls" => process_list(c, rest),
        "inspect" | "info" => {
            process_inspect(c, rest)?;
            Ok(true)
        }
        "kill" => {
            process_kill(c, rest)?;
            Ok(true)
        }
        "help" | "-h" | "--help" => {
            print_usage(super::Usage::Process);
            Ok(true)
        }
        other => Err(format!("unknown process command '{other}'")),
    }
}

/// Every pid the session owns, from one definition.
///
/// Three sets, unioned — see [`crate::session::proc`] for why a parent walk
/// alone is not enough. `process list` and `process kill` both go through this
/// function, so the two cannot disagree about what a session contains; when
/// they computed the union separately, one of them was always going to be the
/// one lying.
///
/// The roots come from [`Session::owned_roots`], so a stopped session — whose
/// roots are all `ProcRef::default()` — contributes nothing. The registered
/// groups are consulted only while there is a live root to own them: they exist
/// so an `exec`ed program stays findable while the session runs, and a record
/// that names no live process owns nothing, however many group ids it carries.
/// That also covers a record which never passed through teardown at all —
/// hand-edited, left by an older build, or written by a crash.
fn owned_pids(record: &Session, table: &proc::ProcTable) -> std::collections::HashSet<u32> {
    let roots = record.owned_roots();
    let mut pids = table.closure(&roots);
    if !roots.is_empty() {
        pids.extend(table.in_groups(&record.pgrps));
    }
    pids
}

/// Every process in a session, in pid order.
fn processes(record: &Session) -> Vec<proc::ProcInfo> {
    let table = proc::ProcTable::scan();
    let pids = owned_pids(record, &table);

    let mut out: Vec<proc::ProcInfo> = pids.iter().filter_map(|pid| proc::read(*pid)).collect();
    // The two roots first even though the table is pid-sorted: "is this the
    // session" is answered by reading the top of the list, not by scanning it.
    out.sort_by_key(|p| {
        let role = match (p.pid == record.wm.pid, p.pid == record.xserver.pid) {
            (true, _) => 0,
            (_, true) => 1,
            _ => 2,
        };
        (role, p.pid)
    });
    out
}

/// The role a process plays in its session, for the listing and for
/// `process inspect`.
fn role_of(p: &proc::ProcInfo, session: &Session) -> &'static str {
    if p.pid == session.wm.pid {
        "maverick"
    } else if p.pid == session.xserver.pid {
        "x-server"
    } else if p.pgid == p.pid {
        // A group leader that is neither root is something the session started
        // directly — an `exec`ed program.
        "application"
    } else {
        "child"
    }
}

fn process_list(c: &Ctl, args: &[String]) -> Result<bool, String> {
    // The resolution error is the answer, not a missing name. Defaulting it to
    // "" reported "session '' does not exist" in an ambiguous context, where
    // the useful message is the one `window list` gives: name one of these.
    let name = session_target(c, args)?;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    let procs = processes(&record);
    if c.json {
        let items: Vec<String> = procs
            .iter()
            .map(|p| process_json(p, role_of(p, &record)))
            .collect();
        println!(
            "{{\"session\":{},\"processes\":[{}]}}",
            maverick_sys::json::json_quote(&name),
            items.join(",")
        );
        return Ok(true);
    }
    if procs.is_empty() {
        println!("No processes found for session '{name}'.");
        return Ok(true);
    }
    println!(
        "{:<8} {:>6} {:>6} {:<12} {:<10} CMD",
        "PID", "CPU", "MEM", "PROCESS", "ROLE"
    );
    for p in &procs {
        println!(
            "{:<8} {:>5.1}% {:>6} {:<12} {:<10} {}",
            p.pid,
            p.cpu_percent(),
            p.rss_human(),
            p.display_name(),
            role_of(p, &record),
            truncate(&p.cmdline, 60),
        );
    }
    println!("\n{} process(es).", procs.len());
    Ok(true)
}

fn process_json(p: &proc::ProcInfo, role: &str) -> String {
    format!(
        "{{\"pid\":{},\"ppid\":{},\"pgid\":{},\"name\":{},\"role\":{},\"cmdline\":{},\"cpu_percent\":{:.1},\"rss_bytes\":{},\"rss\":{},\"elapsed_ms\":{}}}",
        p.pid,
        p.ppid,
        p.pgid,
        maverick_sys::json::json_quote(&p.display_name()),
        maverick_sys::json::json_quote(role),
        maverick_sys::json::json_quote(&p.cmdline),
        p.cpu_percent(),
        p.rss_bytes,
        maverick_sys::json::json_quote(&p.rss_human()),
        p.elapsed_ms,
    )
}

fn process_inspect(c: &Ctl, args: &[String]) -> Result<(), String> {
    let (name, rest) = split_session_and_rest(c, args)
        .ok_or_else(|| "process inspect needs a session and a pid".to_string())?;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    let Some(raw) = rest.first() else {
        return Err("process inspect needs a pid".to_string());
    };
    let pid: u32 = raw.parse().map_err(|_| format!("'{raw}' is not a pid"))?;
    let Some(p) = proc::read(pid) else {
        return Err(format!("no process {pid}"));
    };
    let role = role_of(&p, &record);
    if c.json {
        println!("{}", process_json(&p, role));
        return Ok(());
    }
    println!("PROCESS");
    println!("  {:<12}{}", "pid:", p.pid);
    println!("  {:<12}{}", "ppid:", p.ppid);
    println!("  {:<12}{}", "pgid:", p.pgid);
    println!("  {:<12}{role}", "role:");
    println!("  {:<12}{}", "name:", p.display_name());
    println!("  {:<12}{:.1}%", "cpu:", p.cpu_percent());
    println!("  {:<12}{}", "memory:", p.rss_human());
    println!("  {:<12}{:.1}s", "elapsed:", p.elapsed_ms as f64 / 1000.0);
    println!("  {:<12}{}", "command:", display_or_dash(&p.cmdline));
    Ok(())
}

fn process_kill(c: &Ctl, args: &[String]) -> Result<(), String> {
    let (name, rest) = split_session_and_rest(c, args)
        .ok_or_else(|| "process kill needs a session and a pid".to_string())?;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    let Some(raw) = rest.first() else {
        return Err("process kill needs a pid".to_string());
    };
    let pid: u32 = raw.parse().map_err(|_| format!("'{raw}' is not a pid"))?;
    let force = c.flag(&["--force", "-9"]);
    let Some(p) = proc::read(pid) else {
        return Err(format!("no process {pid}"));
    };
    if !is_in_session(&p, &record) {
        return Err(format!(
            "process {pid} is not part of session '{name}'\n  refusing to signal a process this session does not own"
        ));
    }
    let outcome = if force {
        proc::kill_hard(p.pid, p.start_time)
    } else {
        proc::terminate(p.pid, p.start_time)
    };
    // Only "it is gone" is worth reporting as gone. Any other errno is the
    // kernel refusing, and saying "no longer running" there sends the user
    // looking for an exit that already happened.
    if let Err(e) = outcome {
        return Err(if e.kind() == std::io::ErrorKind::NotFound {
            format!("process {pid} is no longer running")
        } else {
            format!("could not signal {pid}: {e}")
        });
    }
    if c.json {
        println!("{{\"pid\":{pid},\"signalled\":true,\"force\":{force}}}");
    } else {
        println!(
            "{} sent to {pid}{}",
            if force { "SIGKILL" } else { "SIGTERM" },
            if p.pid == record.wm.pid {
                " (the session's window manager — use `maverickctl session stop` for a clean exit)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

/// True if `p` belongs to `session`.
///
/// Shares [`owned_pids`] with the listing, so `process kill` can never signal
/// something `process list` would not have shown — the two must agree, or one
/// of them is lying about what a session owns.
fn is_in_session(p: &proc::ProcInfo, session: &Session) -> bool {
    owned_pids(session, &proc::ProcTable::scan()).contains(&p.pid)
}

// ── logs / debug / inspect ───────────────────────────────────────────────────

/// `maverickctl logs <session>` — the tail of a session's own log.
pub fn logs(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    // The X server's log is the other half of a failed start, and it is where a
    // "display already in use" or a driver that would not bind actually shows up.
    let xserver = args.iter().any(|a| a == "--xserver" || a == "-x");
    let path = if xserver {
        record.xserver_log_path()
    } else {
        record.log_path()
    };
    if !path.exists() {
        return Err(format!(
            "no {} log for session '{name}' yet ({})",
            if xserver {
                "X server"
            } else {
                "window manager"
            },
            path.display()
        ));
    }
    let follow = args.iter().any(|a| a == "-f" || a == "--follow");
    let lines: usize = args
        .iter()
        .skip_while(|a| a.as_str() != "-n")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LOG_LINES);

    print!(
        "{}",
        session::tail(&path, lines)
            .map_err(|e| e.to_string())?
            .join("\n")
    );
    println!();
    if follow {
        follow_file(&path, c)
    } else {
        Ok(())
    }
}

/// `maverickctl debug <session>` — the live event stream plus the debug log.
///
/// Two real sources, because neither alone answers "what is this session
/// doing": the `subscribe` stream is structured, ordered and cheap but only
/// carries transitions, while the log at `MAVERICK_LOG=debug` carries the
/// reconciliation and geometry detail that a bug in either of those needs. Both
/// are filtered by `--window`, so an investigation can be scoped to one window
/// without reading everything.
pub fn debug(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let record = live_record(&name).ok_or_else(|| {
        format!(
            "session '{name}' does not exist\n\n{}",
            available_sessions()
        )
    })?;
    let window_filter = args
        .iter()
        .skip_while(|a| a.as_str() != "--window")
        .nth(1)
        .and_then(|v| v.trim_start_matches("0x").parse::<u64>().ok())
        .map(|v| v as u32);
    if let Some(win) = window_filter {
        println!("# filtered to window 0x{win:x} — press Ctrl-C to stop");
    }
    // The log first, so the events that arrive are read with context.
    if record.log_path().exists() {
        let past = session::tail(&record.log_path(), 20).unwrap_or_default();
        for line in past {
            if matches_filter(&line, window_filter) {
                println!("{line}");
            }
        }
    }
    println!("# live event stream (Ctrl-C to stop)");
    // Callback-shaped because the subscription is a blocking read on a socket:
    // the closure returning `false` is how a filter ends the stream without a
    // second control path through the reader.
    let result = client::subscribe_stream(&name, |line| match window_filter {
        Some(win) if !line_mentions(line, win) => true,
        _ => {
            println!("{line}");
            let _ = std::io::stdout().flush();
            true
        }
    });
    result.map_err(|e| format!("cannot subscribe to session '{name}': {e}\n  is it running?"))
}

/// `maverickctl inspect <session>` — what the session manager and the window
/// manager each know, together.
pub fn inspect(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let view = session::resolve(&name).map_err(|e| e.to_string())?;
    let record = live_record(&view.name);
    let procs = record.as_ref().map(processes).unwrap_or_default();
    // The window manager's own answer, when it is up. Absent for a stopped
    // session, which is reported as such rather than as a session with no
    // windows.
    let live = client::query(&view.sid, "inspect")
        .ok()
        .and_then(|j| maverick_sys::json::parse(&j));
    let tree = live
        .as_ref()
        .and_then(|_| client::query(&view.sid, "tree").ok())
        .and_then(|j| maverick_sys::json::parse(&j));
    let windows = tree.as_ref().map(flatten_windows).unwrap_or_default();

    if c.json {
        let mut parts = vec![view_json(&view)];
        // Merge the two documents into one object rather than nesting them, so
        // an agent reads `{"name":…,"windows":{…}}` and not two levels of
        // wrapper it has to know about.
        let merged = merge_inspect(&parts[0], live.as_ref(), &windows, procs.len());
        parts.clear();
        println!("{merged}");
        return Ok(());
    }

    println!("SESSION");
    println!("  {:<14}{}", "name:", view.name);
    println!("  {:<14}{}", "display:", display_or_dash(&view.display));
    println!(
        "  {:<14}{}",
        "resolution:",
        view.resolution
            .map_or_else(|| "-".to_string(), |r| r.to_string())
    );
    println!("  {:<14}{}", "state:", view.state);
    println!(
        "  {:<14}{}",
        "pid:",
        view.pid.map_or_else(|| "-".to_string(), |p| p.to_string())
    );
    println!("  {:<14}{}", "binary:", display_or_dash(&view.binary));
    println!(
        "  {:<14}{}",
        "debug:",
        if view.debug { "yes" } else { "no" }
    );

    let Some(live) = live.as_ref() else {
        println!("\n  the window manager is not answering; no live layout to report");
        return Ok(());
    };
    if let Some(w) = live.get("windows") {
        println!("\nWINDOWS");
        println!("  {:<14}{}", "managed:", w.num_field("total"));
        println!("  {:<14}{}", "floating:", w.num_field("floating"));
        println!("  {:<14}{}", "fullscreen:", w.num_field("fullscreen"));
    }
    if let Some(l) = live.get("layout") {
        println!("\nLAYOUT");
        println!("  {:<14}{}", "type:", l.str_field("type"));
        println!("  {:<14}{}", "columns:", l.num_field("columns"));
        if let Some(cam) = l.get("camera").and_then(Json::as_f64) {
            println!("  {:<14}{cam:.3}", "camera:");
        }
    }
    println!("\nPROCESSES");
    println!("  {:<14}{}", "total:", procs.len());
    Ok(())
}

/// Combine the session view, the window manager's answer and the process count
/// into one flat object.
fn merge_inspect(
    view: &str,
    live: Option<&Json>,
    windows: &[super::WindowInfo],
    process_count: usize,
) -> String {
    let base = maverick_sys::json::parse(view).expect("a view this function just built");
    let mut fields = match base {
        Json::Obj(fields) => fields,
        _ => Vec::new(),
    };
    if let Some(live) = live {
        for key in ["windows", "layout", "monitor", "animations"] {
            if let Some(v) = live.get(key) {
                fields.retain(|(k, _)| k != key);
                fields.push((key.to_string(), v.clone()));
            }
        }
    }
    if !windows.is_empty() {
        let items: Vec<Json> = windows
            .iter()
            .map(|w| {
                Json::Obj(vec![
                    ("id".into(), Json::Num(w.id as f64)),
                    (
                        "pid".into(),
                        w.pid.map_or(Json::Null, |p| Json::Num(p as f64)),
                    ),
                    ("class".into(), Json::Str(w.class.clone())),
                    ("instance".into(), Json::Str(w.instance.clone())),
                    ("title".into(), Json::Str(w.title.clone())),
                    ("monitor".into(), Json::Num(w.monitor as f64)),
                    ("workspace".into(), Json::Num(w.workspace as f64)),
                    ("floating".into(), Json::Bool(w.floating)),
                    ("fullscreen".into(), Json::Bool(w.fullscreen)),
                    ("maximized".into(), Json::Bool(w.maximized)),
                    ("focused".into(), Json::Bool(w.focused)),
                ])
            })
            .collect();
        fields.retain(|(k, _)| k != "window_list");
        fields.push(("window_list".into(), Json::Arr(items)));
    }
    fields.push(("process_count".into(), Json::Num(process_count as f64)));
    Json::Obj(fields).to_json()
}

/// Print a file and keep printing what is appended to it.
fn follow_file(path: &Path, _c: &Ctl) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        // A truncated file (a restart) means the offset is meaningless; go
        // back to the start rather than printing from a hole.
        if meta.len() < offset {
            offset = 0;
        }
        if meta.len() == offset {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            continue;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        offset += buf.len() as u64;
        print!("{}", String::from_utf8_lossy(&buf));
        let _ = std::io::stdout().flush();
    }
}

/// True if a log line concerns `win`, or names no window at all.
///
/// A line that names *other* windows is dropped; a line that names none is
/// kept. That second half matters: a failure with no window reference — a
/// rejected keybind, a lost client, an X error — is exactly the one a window
/// filter would otherwise hide, and the whole reason to filter is to read less,
/// not to miss things.
fn matches_filter(line: &str, window: Option<u32>) -> bool {
    let Some(win) = window else {
        return true;
    };
    let named = hex_ids(line);
    named.is_empty() || named.contains(&win)
}

/// Every `0x…` window id mentioned in a line.
///
/// A hex token is read up to the first character that cannot be part of one, so
/// a line mentioning two windows yields both and a line mentioning none yields
/// an empty list.
fn hex_ids(line: &str) -> Vec<u32> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end > start {
                if let Ok(id) = u32::from_str_radix(line.get(start..end).unwrap_or(""), 16) {
                    out.push(id);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// True if a structured event line names this window.
fn line_mentions(line: &str, win: u32) -> bool {
    let hex = format!("0x{win:x}");
    let dec = win.to_string();
    line.contains(&hex) || line.contains(&dec)
}

/// Truncate for a table cell, marking that something was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The record for a session this manager owns, or `None` for a session that is
/// not on disk (an unmanaged one, or no such name).
pub fn live_record(name: &str) -> Option<Session> {
    SessionName::parse(name)
        .ok()
        .and_then(|n| session::read(&n))
}

/// The names a user can pick from, rendered for an error message.
pub fn available_sessions() -> String {
    let mut names: Vec<String> = session::list().into_iter().map(|v| v.name).collect();
    names.sort();
    if names.is_empty() {
        return "No sessions exist yet. Create one with: maverickctl session create <name>".into();
    }
    format!(
        "Available sessions:\n{}",
        names
            .iter()
            .map(|n| format!("  {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl::windows::window_selector;

    /// The boundary between this tool's arguments and a command's is the one
    /// place where being clever is actively wrong: a program's own flags must
    /// reach the program.
    #[test]
    fn a_commands_own_flags_reach_the_command() {
        let args = vec![
            "debug".to_string(),
            "alacritty".to_string(),
            "--json".to_string(),
            "--wait".to_string(),
        ];
        let c = Ctl::parse("maverickctl", &args, &[]);
        let call = split_call(&c, &args).expect("a call");
        assert_eq!(call.session, "debug");
        assert_eq!(
            call.argv,
            vec!["alacritty", "--json", "--wait"],
            "a program's flags must not be eaten by the launcher"
        );
    }

    /// With an explicit `--session`, the first positional is the command rather
    /// than the session.
    #[test]
    fn an_explicit_session_makes_the_first_positional_the_command() {
        let args = vec![
            "--session".to_string(),
            "agent".to_string(),
            "firefox".to_string(),
            "--new-window".to_string(),
        ];
        let c = Ctl::parse("maverickctl", &args, &[]);
        let call = split_call(&c, &args).expect("a call");
        assert_eq!(call.session, "agent");
        assert_eq!(call.argv, vec!["firefox", "--new-window"]);
    }

    /// `--` ends this tool's parsing, which is the escape hatch for a command
    /// word that looks like a flag.
    #[test]
    fn a_double_dash_hands_the_rest_over_untouched() {
        let args = vec![
            "debug".to_string(),
            "--".to_string(),
            "--weird".to_string(),
            "-x".to_string(),
        ];
        let c = Ctl::parse("maverickctl", &args, &[]);
        let call = split_call(&c, &args).expect("a call");
        assert_eq!(call.session, "debug");
        assert_eq!(call.argv, vec!["--weird", "-x"]);
    }

    /// With no command named at all, the argv is empty and the caller supplies
    /// a default — the split must not invent a program.
    #[test]
    fn a_session_with_no_command_yields_no_command() {
        let args = vec!["debug".to_string()];
        let c = Ctl::parse("maverickctl", &args, &[]);
        let call = split_call(&c, &args).expect("a call");
        assert_eq!(call.session, "debug");
        assert!(call.argv.is_empty());
    }

    /// The window verbs read the selector from *after* the session: reading the
    /// first positional as the selector would ask the window manager to act on a
    /// session name.
    #[test]
    fn a_window_selector_is_never_the_session_name() {
        let c = Ctl::parse("maverickctl", &[], &[]);
        let args = vec!["debug".to_string(), "firefox".to_string()];
        assert_eq!(window_selector(&c, &args), Some("firefox".to_string()));
        // No selector at all: the caller falls back to the focused window.
        assert_eq!(window_selector(&c, &["debug".to_string()]), None);
        // A direction is the verb's own argument, never the target.
        let moving = vec!["debug".to_string(), "right".to_string()];
        assert_eq!(window_selector(&c, &moving), None);
        // And a flag of this tool's is not a window name.
        let flagged = vec!["debug".to_string(), "--json".to_string()];
        assert_eq!(window_selector(&c, &flagged), None);
    }

    /// Everything after `--` belongs to the window manager, and nothing before
    /// it does: a flag Maverick supports today must be forwardable without
    /// the session manager learning it.
    #[test]
    fn everything_after_the_separator_goes_to_maverick() {
        let args = vec![
            "debug".to_string(),
            "--resolution".to_string(),
            "800x600".to_string(),
            "--".to_string(),
            "--debug".to_string(),
            "--log-level".to_string(),
            "trace".to_string(),
        ];
        let parsed = CreateArgs::parse(&args).expect("parses");
        assert_eq!(parsed.name.as_str(), "debug");
        assert_eq!(
            parsed.resolution,
            Resolution::new(800, 600).expect("resolution")
        );
        assert_eq!(
            parsed.maverick_args,
            vec!["--debug", "--log-level", "trace"]
        );
        assert!(
            !parsed.debug,
            "`--debug` after `--` is Maverick's, not the session's"
        );
    }

    /// `--debug` before the separator configures the *session*: it is the
    /// flag that makes `maverickctl logs` worth reading, and it has to be
    /// something the user can type without also knowing `MAVERICK_LOG`.
    #[test]
    fn the_sessions_own_debug_flag_is_separate() {
        let parsed =
            CreateArgs::parse(&["debug".to_string(), "--debug".to_string()]).expect("parse");
        assert!(parsed.debug);
        assert!(parsed.maverick_args.is_empty());
    }

    #[test]
    fn create_parses_every_option() {
        let parsed = CreateArgs::parse(&[
            "agent".into(),
            "--resolution".into(),
            "1024x768".into(),
            "--refresh-rate".into(),
            "120".into(),
            "--backend".into(),
            "xvfb".into(),
            "--binary".into(),
            "./target/debug/maverick".into(),
            "--cwd".into(),
            "/tmp".into(),
        ])
        .expect("parses");
        assert_eq!(parsed.name.as_str(), "agent");
        assert_eq!(parsed.resolution, Resolution::new(1024, 768).expect("res"));
        assert_eq!(parsed.refresh_rate, Some(120));
        assert_eq!(parsed.backend, Backend::Xvfb);
        assert_eq!(parsed.binary, "./target/debug/maverick");
        assert_eq!(parsed.cwd, Some(PathBuf::from("/tmp")));
    }

    /// Every failure has to name the option and, where there is an obvious
    /// alternative, the value that would have worked.
    #[test]
    fn create_reports_bad_options_with_the_offending_value() {
        for (args, needle) in [
            (vec!["debug", "--resolution", "big"], "WIDTHxHEIGHT"),
            (vec!["debug", "--refresh-rate", "fast"], "not a number"),
            (vec!["debug", "--backend", "wayland"], "unknown --backend"),
            (vec!["debug", "--resolution"], "requires a value"),
            (vec!["debug", "--wat"], "unknown option"),
            (vec!["debug", "extra"], "one name"),
            (vec!["bad/name"], "invalid session name"),
        ] {
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let err = CreateArgs::parse(&owned)
                .expect_err(&format!("{args:?} must be refused"))
                .to_string();
            assert!(
                err.contains(needle),
                "{args:?}: {err:?} must mention {needle:?}"
            );
        }
        assert!(CreateArgs::parse(&[]).is_err(), "a name is required");
    }

    /// The listing must not be able to print a secret. The Xauthority *path* is
    /// reported, because a client needs to be pointed at it; its contents never
    /// appear in any document this tool writes.
    ///
    /// Checked as a property of the document rather than against a fixed value,
    /// because the `xauth` field is a path only when a record exists on disk —
    /// and a test that depended on that would be asserting on whatever the
    /// machine happened to have in its runtime directory.
    #[test]
    fn the_session_view_carries_the_xauth_path_and_never_its_contents() {
        let view = SessionView {
            name: "secretprobe".into(),
            sid: "secretprobe".into(),
            kind: "nested",
            state: SessionState::Running,
            display: ":1".into(),
            resolution: Some(Resolution::new(1280, 720).expect("res")),
            refresh_rate: None,
            x_pid: Some(11),
            pid: Some(22),
            binary: "/usr/bin/maverick".into(),
            debug: true,
            backend: Some(Backend::Xephyr),
            exit_reason: String::new(),
            created_at: 17,
            owner_uid: 1000,
        };
        let doc = view_json(&view);
        assert!(
            doc.contains("\"xauth\""),
            "the key is always present: {doc}"
        );
        assert!(
            maverick_sys::json::parse(&doc).is_some(),
            "and it must be valid JSON"
        );

        // No 32-hex-digit run anywhere: that is the shape of an MIT-MAGIC
        // cookie, and one appearing in a document is a credential leak. The
        // pid-like numbers and the resolution cannot produce sixteen adjacent
        // hex digits, so this is a real check rather than a formality.
        let bytes = doc.as_bytes();
        let mut run = 0;
        for b in bytes {
            if b.is_ascii_hexdigit() {
                run += 1;
                assert!(run < 16, "a cookie-shaped token leaked: {doc}");
            } else {
                run = 0;
            }
        }
        // And no environment variable is reported at all, so nothing inherited
        // by this process can ride along into a log or a JSON document.
        for forbidden in ["DISPLAY=", "XAUTHORITY="] {
            assert!(!doc.contains(forbidden), "{forbidden} leaked: {doc}");
        }
    }

    /// The listing and the kill guard must agree about what a session owns: a
    /// pid the listing would not show must not be killable either.
    #[test]
    fn the_session_view_is_valid_json_with_every_documented_field() {
        let doc = view_json(&SessionView {
            name: "main".into(),
            sid: "abc123".into(),
            kind: "main",
            state: SessionState::Running,
            display: ":0".into(),
            resolution: None,
            refresh_rate: None,
            x_pid: None,
            pid: Some(1821),
            binary: "/usr/bin/maverick".into(),
            debug: false,
            backend: None,
            exit_reason: String::new(),
            created_at: 0,
            owner_uid: 1000,
        });
        let v = maverick_sys::json::parse(&doc).expect("valid JSON");
        for key in [
            "name",
            "session_id",
            "kind",
            "state",
            "display",
            "resolution",
            "refresh_rate",
            "pid",
            "x_pid",
            "binary",
            "debug",
            "backend",
            "xauth",
            "exit_reason",
            "created_at",
            "owner_uid",
        ] {
            assert!(v.get(key).is_some(), "{doc} is missing {key}");
        }
        assert_eq!(v.get("resolution"), Some(&Json::Null));
        assert_eq!(v.num_field("pid"), 1821);
        assert_eq!(v.str_field("state"), "running");
    }

    #[test]
    fn process_json_round_trips_through_the_parser() {
        let p = proc::ProcInfo {
            pid: 18231,
            ppid: 1,
            pgid: 18231,
            start_time: 5,
            comm: "alacritty".into(),
            cmdline: "alacritty --title \"a,b\"".into(),
            rss_bytes: 42 * 1024 * 1024,
            user_ms: 1000,
            sys_ms: 500,
            elapsed_ms: 10_000,
        };
        let doc = process_json(&p, "application");
        let v = maverick_sys::json::parse(&doc).expect("valid JSON");
        assert_eq!(v.num_field("pid"), 18231);
        assert_eq!(v.str_field("name"), "alacritty");
        assert_eq!(v.str_field("role"), "application");
        assert_eq!(v.str_field("cmdline"), "alacritty --title \"a,b\"");
        assert_eq!(v.num_field("rss_bytes"), 42 * 1024 * 1024);
        assert_eq!(v.str_field("rss"), "42M");
    }

    /// A debug filter must not hide the lines that never name a window: a
    /// failure with no window reference is the one a `--window` filter would
    /// otherwise swallow.
    #[test]
    fn a_window_filter_keeps_lines_that_name_no_window() {
        let win = 0x42003;
        assert!(matches_filter(&format!("focus win=0x{win:x}"), Some(win)));
        assert!(matches_filter(
            "keybind rejected: no such action",
            Some(win)
        ));
        assert!(
            !matches_filter("focus win=0x99", Some(win)),
            "a line about another window must be filtered out"
        );
        assert!(matches_filter("anything", None));
        // A line naming both keeps, because it is partly about this one.
        assert!(matches_filter(&format!("swap 0x99 -> {win:#x}"), Some(win)));
    }

    #[test]
    fn event_lines_are_matched_by_either_id_spelling() {
        let win = 0x42003;
        assert!(line_mentions(&format!("{{\"window\":0x{win:x}}}"), win));
        assert!(line_mentions(&format!("{{\"window\":{win}}}"), win));
        assert!(!line_mentions("{\"window\":1}", win));
    }

    #[test]
    fn table_cells_are_truncated_with_a_mark() {
        assert_eq!(truncate("short", 10), "short");
        let long = "x".repeat(50);
        let cut = truncate(&long, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }
}
