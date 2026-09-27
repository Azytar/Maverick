//! Binary entry — process lifecycle, CLI, and handover to the X backend.
//!
//! Owns process-level resources only: argv, the `MAVERICK_INSTANCE` export,
//! terminal detachment, signal disposition, the per-session identity record
//! (`maverick-sys::identity`), and the control-socket hub. It then hands a
//! `Cfg` to `backend::x11::WindowManager`, which owns the X connection
//! (`Rc<XConn>`) and the event loop.
//!
//! # Lifecycle
//!
//! `log::init` → parse args → `--check-config`? (exits) → detach + signals →
//! write identity + spawn control socket → `config::load_config` →
//! `WindowManager::new` → autostart → `state.running = true` → `run()` →
//! `cleanup()` on a clean exit, or `cleanup_meta` + `exit(1)` if init failed.
//!
//! # Invariants
//!
//! `--check-config` never starts the backend: it loads the TOML, dumps
//! diagnostics, prints a summary, and exits 0/1. Signal handlers are installed
//! after `detach_from_terminal` and before the X connection is opened, so
//! `SIGTERM`/`SIGINT`/`SIGQUIT`/`SIGCONT`/`SIGPIPE` disposition is defined for
//! the whole session. `SIGCHLD` is set to auto-reaping at the same moment,
//! which is what keeps autostarted clients from becoming zombies — and which
//! also means nothing in this process may wait on a child, so the image
//! decoders' external fallback reads its converter's output instead of asking
//! for a status. If any of those installs is refused, `uninstalled_report` says
//! which guarantee was lost rather than leaving the process to imply it has
//! all of them.
//! The original argv is captured verbatim (minus `argv[0]`) because `restart`
//! re-execs with exactly those arguments, so a `--config` override can never be
//! silently downgraded to the XDG default.
// Opt into clippy's pedantic lint set for higher code quality, then allow the
// handful of categories that are inherent to an X11 window manager and would
// only add noise if "fixed":
//   * X11 protocol coordinates freely mix i16/u16/u32/i32 (window geometry,
//     event fields, CARDINAL props). Wrapping every conversion in From/try_into
//     or asserting ranges buys nothing here — the casts are protocol-correct.
//   * `module_name_repetitions` / `wildcard_imports`: the backend uses
//     `use super::*;` re-exports and x11rb's flat type names by design.
//   * `missing_errors_doc`: internal fns return boxed errors that are logged,
//     not part of a documented public API surface.
//   * `must_use_candidate`: most getters are used immediately; annotating all
//     is churn without safety value.
#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::module_name_repetitions,
    clippy::wildcard_imports,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::similar_names,
    // Hex colour literals (0x1a1b26) and X11 bit masks read better without
    // digit-group separators.
    clippy::unreadable_literal,
    // Stylistic pedantic lints where the current form is intentional and, in
    // this codebase, at least as clear as the suggested rewrite. Event handlers
    // uniformly return `Result<(), Box<dyn Error>>` for a consistent dispatch
    // signature (hence unit/Result "unnecessary" returns and unused-self on a
    // few); the early-`match`/`return` style is deliberate for readability.
    clippy::manual_let_else,
    clippy::semicolon_if_nothing_returned,
    clippy::items_after_statements,
    clippy::unused_self,
    clippy::unnecessary_wraps,
    clippy::struct_excessive_bools,
    clippy::many_single_char_names,
    clippy::trivially_copy_pass_by_ref,
    clippy::needless_pass_by_value
)]

mod backend;
mod bench_arrange;
mod compositor_policy;
mod config;
pub mod core;
mod log;
mod types;
mod userconfig;

use std::process;

fn main() {
    log::init();
    log::info!("maverick v{} starting", env!("CARGO_PKG_VERSION"));

    // Parse args in any order. Unknown arguments abort before any state is
    // created, so a typo cannot leave a half-initialised session behind.
    let mut instance_name = maverick_sys::DEFAULT_NAME.to_string();
    let mut session_id: Option<String> = None;
    // Logging flags, applied after `log::init` (which reads the environment)
    // and before anything is created, so a debug session's very first line is
    // already at the level that was asked for.
    let mut debug = false;
    let mut log_level: Option<String> = None;
    let mut replace = false;
    let mut show_help = false;
    let mut show_version = false;
    let mut config_path: Option<String> = None;
    // `--check-config` with an optional path argument. `Some(Some(p))` means the
    // flag was given with an explicit path; `Some(None)` means the flag was
    // given bare (use the configured/default path); `None` means the flag was
    // not passed at all.
    let mut check_config: Option<Option<String>> = None;
    let mut bad_arg: Option<String> = None;
    let mut bench_arrange = false;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bench-arrange" => bench_arrange = true,
            "-v" | "--version" => show_version = true,
            "-h" | "--help" => show_help = true,
            "--replace" | "-r" => replace = true,
            "--config" => {
                if let Some(p) = args.next() {
                    config_path = Some(p)
                } else {
                    bad_arg = Some("--config requires a value".into());
                    break;
                }
            }
            "--check-config" => {
                // Optional path: consume the next token only if it is not a
                // flag. Both `--flag` and `-f` count as flags, otherwise
                // `--check-config -v` would validate a file named `-v`.
                let path = match args.peek() {
                    Some(next) if !next.starts_with('-') => args.next(),
                    _ => None,
                };
                check_config = Some(path);
            }
            "--name" => {
                if let Some(n) = args.next() {
                    instance_name = n
                } else {
                    bad_arg = Some("--name requires a value".into());
                    break;
                }
            }
            "--session-id" => {
                // An explicit id makes this instance's runtime directory, its
                // control socket and its identity ficha live at a name the user
                // chose, which is how a session manager addresses a Maverick by
                // session name instead of by a random per-process id. Rejected
                // before any state is created: an invalid id would otherwise
                // produce a directory no tool knows how to find.
                if let Some(s) = args.next() {
                    session_id = Some(s)
                } else {
                    bad_arg = Some("--session-id requires a value".into());
                    break;
                }
            }
            "--debug" => debug = true,
            "--log-level" => {
                if let Some(l) = args.next() {
                    if log::is_known_level(&l) {
                        log_level = Some(l)
                    } else {
                        bad_arg = Some(format!(
                            "--log-level '{l}' is not a level (off|error|warn|info|debug|trace)"
                        ));
                        break;
                    }
                } else {
                    bad_arg = Some("--log-level requires a value".into());
                    break;
                }
            }
            unknown => {
                bad_arg = Some(format!("unknown argument: {unknown}"));
                break;
            }
        }
    }

    // Capture the original argv (minus argv[0]) so `restart` can re-exec with
    // the EXACT same arguments (--config/--name/--replace), never a silently
    // regenerated/defaulted config.
    let launch_args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(msg) = bad_arg {
        eprintln!("maverick: {msg}");
        process::exit(1);
    }

    // Applied here, after the environment was read and validated but before the
    // first log line: a session started with `--debug` must not lose its first
    // few lines to the level it was asked to raise. `--log-level` wins over
    // `--debug` so the two do not have to be ordered relative to each other.
    if debug {
        log::set_level("debug");
    }
    if let Some(level) = &log_level {
        log::set_level(level);
    }

    // Synthetic release benchmark: no X, no WM startup, exits after printing.
    if bench_arrange {
        process::exit(crate::bench_arrange::run());
    }

    // `--check-config` exits 0 when the file parses with no warnings and no
    // errors, 1 otherwise. Config is never fatal at runtime, but a CI or lint
    // gate still needs a non-zero status to act on it.
    if let Some(check) = check_config {
        let path: std::path::PathBuf = match check {
            Some(p) => std::path::PathBuf::from(p),
            None => config_path
                .clone()
                .map(std::path::PathBuf::from)
                .or_else(crate::userconfig::config_path)
                .unwrap_or_else(|| {
                    eprintln!("maverick: --check-config: no config file found");
                    process::exit(1);
                }),
        };
        let (cfg, diag) = crate::userconfig::load_from_path(&path);
        crate::userconfig::dump_diagnostics(&diag);
        println!(
            "maverick: config check {}: {} warning(s), {} error(s), {} keybind(s)",
            path.display(),
            diag.warnings.len(),
            diag.errors.len(),
            cfg.keybinds.len()
        );
        if diag.is_clean() {
            println!("maverick: config OK");
            process::exit(0);
        } else {
            process::exit(1);
        }
    }

    if show_version {
        println!("maverick {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }
    if show_help {
        println!("Usage: maverick [--name <id>] [--session-id <id>] [--replace] [--debug] [--log-level <level>] [--config <path>] [--check-config [path]] [-v] [-h]");
        println!("  --name <id>          Instance name for control/identification");
        println!("  --session-id <id>    Publish this instance under a fixed session id");
        println!("                        ([A-Za-z0-9_-]), so its runtime directory and");
        println!("                        control socket are named after it. Used by");
        println!("                        `maverickctl session`; the default is random.");
        println!("  --replace            Replace an already-running WM (adopts your windows)");
        println!("  --debug              Log at debug level (same as MAVERICK_LOG=debug)");
        println!("  --log-level <level>  off|error|warn|info|debug|trace (overrides --debug)");
        println!("  --config <path>      Load the config TOML from <path> instead of");
        println!("                        $XDG_CONFIG_HOME/maverick/config.toml");
        println!("  --check-config [path] Validate the config TOML and exit (0 = clean,");
        println!("                        1 = warnings/errors). Starts no WM.");
        println!("  -v, --version        Print version and exit");
        println!("  -h, --help           Show this help");
        println!();
        println!("Configuration: a config.toml is read from $XDG_CONFIG_HOME/maverick/");
        println!("(or the path given to --config). When no file exists, compiled-in");
        println!("defaults are used. Start from .xinitrc: exec maverick");
        process::exit(0);
    }

    log::info!("instance name: {}", instance_name);

    // Build this instance's identity. The `session_id` is the filesystem key
    // naming the per-session runtime dir, control socket and identity record;
    // the human `--name` is kept separately as a label. Computed before
    // detaching so the tty_nr/start_time metadata are captured reliably.
    //
    // A `--session-id` makes the key a name the user chose instead of a random
    // one, so the session manager can address this instance by session name.
    // A rejected id aborts here, before anything is created: an instance that
    // fell back to a random id would write its ficha and bind its socket
    // somewhere no `maverickctl` command would ever look.
    let info = match &session_id {
        Some(sid) => match maverick_sys::identity::self_info_with_sid(&instance_name, sid) {
            Ok(info) => info,
            Err(e) => {
                eprintln!("maverick: {e}");
                process::exit(1);
            }
        },
        None => maverick_sys::self_info(&instance_name),
    };
    let sid = info.session_id.clone();

    // Export the session id so child processes (notably `maverickctl`) target
    // *this* instance by default, even when several Mavericks run on different
    // TTYs/DISPLAYs.
    std::env::set_var("MAVERICK_INSTANCE", &sid);

    maverick_sys::detach_from_terminal();
    let uninstalled = maverick_sys::Signal::new()
        .ignore(libc::SIGPIPE)
        .on_sigterm(libc::SIGTERM)
        // SIGINT takes the same route as SIGTERM: `on_sigterm` records the
        // quit flag for whichever signal it is given, and the event loop turns
        // that flag into the same `begin_shutdown` a control-socket `quit` and
        // `Mod4+Shift+Q` use. Registering it is what makes the inherited
        // disposition irrelevant — a non-interactive shell sets SIGINT to
        // `SIG_IGN` in any job it starts with `&`, and without a handler of our
        // own a backgrounded Maverick could not be stopped with Ctrl-C at all,
        // and one that was stopped ran none of its cleanup.
        //
        // It also stops that `SIG_IGN` reaching the applications we start:
        // an ignored disposition survives `exec`, so an inherited one would
        // hand every autostarted client a SIGINT it can never act on.
        .on_sigterm(libc::SIGINT)
        // SIGQUIT is the same hole, and it is the one a user reaches for next
        // (Ctrl-\), so leaving it out made the handler above half a fix: a
        // shell sets *both* dispositions to `SIG_IGN` for a backgrounded job,
        // so a backgrounded window manager was unstoppable by SIGQUIT, ran
        // none of its cleanup, and passed that `SIG_IGN` to every client it
        // started. Registering it costs nothing and closes both halves.
        .on_sigterm(libc::SIGQUIT)
        .on_sigcont(libc::SIGCONT)
        .install();
    // A disposition that did not install is a window manager that is missing
    // one of the guarantees it is about to depend on, so say which one rather
    // than that "a handler" is missing: they are not interchangeable, and the
    // user who reads this line needs to know whether the window manager can be
    // stopped or whether its clients will be left as zombies. See
    // `uninstalled_report`.
    if let Some(report) = uninstalled_report(&uninstalled) {
        log::warn!("{report}");
    }

    // Advertise this instance so an external tool can discover or close it,
    // even when several Mavericks run on different TTYs/DISPLAYs. Neither of
    // these is fatal: a WM without a control socket still manages windows.
    if let Err(e) = maverick_sys::identity::write_meta(&info) {
        log::warn!("failed to write instance ficha: {e}");
    }
    let identity_json = maverick_sys::control::identity_json(&info);
    // The hub bridges the control-socket thread and the WM event loop: it
    // queues dispatched commands, caches the state snapshot, and fans out
    // events to `subscribe` clients.
    let hub = maverick_sys::ControlHub::new();
    let control = match maverick_sys::ControlServer::spawn(&sid, identity_json, hub.clone()) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("failed to start control socket: {e}");
            None
        }
    };

    let cfg = config::load_config(config_path.as_deref().map(std::path::Path::new));
    log::info!(
        "config: {} tags, {} keybinds, {} rules, {} autostart",
        cfg.tag_names.len(),
        cfg.keybinds.len(),
        cfg.rules.len(),
        cfg.autostart.len(),
    );

    match backend::x11::WindowManager::new(
        cfg,
        replace,
        config_path.clone().map(std::path::PathBuf::from),
        launch_args,
    ) {
        Ok(mut manager) => {
            // Hand over the control socket + session id so cleanup() can tear
            // them down and remove the identity record on exit.
            manager.set_session_id(sid.clone());
            manager.set_hub(hub);
            if let Some(server) = control {
                manager.set_control(server);
            }

            // Autostart runs only after the WM owns the X connection: a bar or
            // portal started earlier would race the EWMH setup it needs.
            // Compositor, bar, wallpaper and portals are all just entries here;
            // nothing is orchestrated specially. See the examples in
            // config.rs / config.toml.
            for cmd in &manager.engine.cfg.autostart {
                if let Some((bin, args)) = cmd.split_first() {
                    // Detach stdio fully: an autostart child inheriting our
                    // stdin keeps the terminal/session alive and can block a
                    // clean reset. (`actions::spawn` follows the same rule.)
                    if let Err(e) = std::process::Command::new(bin)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        log::error!("autostart '{}' failed: {}", bin, e);
                    }
                }
            }

            // `running` starts out `false` (State::new), so the loop would
            // otherwise exit before servicing a single X11 event.
            manager.engine.state.running = true;
            match manager.run() {
                Ok(()) => {
                    // A loop that returns with `running` still set means the X
                    // connection died; there is nothing to clean up against.
                    let disconnected = manager.engine.state.running;
                    if disconnected {
                        log::warn!("maverick: X server disconnected — exiting");
                    } else {
                        log::info!("maverick exiting cleanly");
                        if let Err(e) = manager.cleanup() {
                            log::warn!("cleanup error: {e}");
                        }
                    }
                }
                Err(e) => {
                    log::error!("fatal error in event loop: {e}");
                    let _ = manager.cleanup();
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("maverick: failed to initialise: {e}");
            // Remove the identity record written before init, so a failed
            // start does not linger in the instance list.
            maverick_sys::identity::cleanup_meta(&sid);
            process::exit(1);
        }
    }
}

// Detaching and signal setup live in `maverick-sys`, the only place in the
// project that touches libc FFI. See `detach_from_terminal` and `Signal` there.

/// Describe the signal dispositions that did not install, naming the guarantee
/// each one is holding up.
///
/// `Signal::install` reports the signals whose `sigaction` was refused and
/// nothing else — it cannot know what the caller was going to depend on. The
/// consequences are not interchangeable, and a report that lumps them together
/// is worse than no report: a window manager that ignored `SIGPIPE` did not
/// become unkillable, and a reader told otherwise learns to distrust the next
/// one. So the classification lives here, with the window manager's own
/// lifecycle, rather than in the FFI crate.
///
/// `None` means every disposition installed, which is the only state in which
/// this process may claim a signal-controlled lifecycle at all. Partial
/// installation is not rolled back — there is nothing to roll back to, since
/// the inherited dispositions were never read — so the claim is narrowed to
/// what did install, and the gap is reported rather than described away.
fn uninstalled_report(uninstalled: &[libc::c_int]) -> Option<String> {
    if uninstalled.is_empty() {
        return None;
    }

    // What each disposition is holding up, if it fails to install. A missed
    // stop signal is the one that strands the session: the window manager
    // cannot be asked to stop, so it never runs `cleanup()`, never removes its
    // identity record and never closes its control socket. A missed `SIGCHLD`
    // turns every autostarted client into a zombie for the life of the process,
    // because nothing in the window manager waits. A signal nobody configured —
    // refused as an invalid number, say — has no consequence this crate can
    // name, and must not be reported as if it had one.
    let consequence = |sig: libc::c_int| -> Option<&'static str> {
        Some(match sig {
            libc::SIGTERM | libc::SIGINT | libc::SIGQUIT => {
                "the window manager cannot be stopped by signal and will not run its cleanup"
            }
            libc::SIGCONT => "keyboard grabs are not restored after a suspend",
            libc::SIGCHLD => "autostarted children are not auto-reaped and stay as zombies",
            libc::SIGPIPE => "a client disconnecting mid-write can terminate the window manager",
            _ => return None,
        })
    };

    let mut names: Vec<&str> = Vec::with_capacity(uninstalled.len());
    let mut consequences: Vec<&str> = Vec::new();
    for &sig in uninstalled {
        names.push(signal_name(sig).unwrap_or("an unrecognised signal"));
        consequences.extend(consequence(sig));
    }
    // The three stop signals share one consequence, so a report naming all of
    // them must not repeat it three times.
    names.sort_unstable();
    names.dedup();
    consequences.sort_unstable();
    consequences.dedup();

    let mut report = format!("could not install a disposition for {}", names.join(", "));
    if consequences.is_empty() {
        report.push_str("; no window-manager guarantee is known to be lost");
    } else {
        report.push_str(": ");
        report.push_str(&consequences.join("; "));
    }
    Some(report)
}

/// `SIGTERM` and friends, or the number itself when it names nothing this
/// crate knows. Never panics and never allocates for the known cases, because
/// this runs on the startup path where a message that cannot be rendered is
/// worse than a message that is merely terse.
fn signal_name(sig: libc::c_int) -> Option<&'static str> {
    Some(match sig {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGWINCH => "SIGWINCH",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::uninstalled_report;

    /// The only state in which the process may claim a signal-controlled
    /// lifecycle at all.
    #[test]
    fn a_complete_install_reports_nothing() {
        assert_eq!(uninstalled_report(&[]), None);
    }

    /// SIGCHLD is the disposition that silently changes child reaping, and its
    /// consequence is not the one a missing stop signal has. Reporting them
    /// alike is what made the previous single message unreadable.
    #[test]
    fn a_missed_sigchld_names_reaping_and_not_stoppability() {
        let report = uninstalled_report(&[libc::SIGCHLD]).expect("a report");
        assert!(report.contains("SIGCHLD"), "{report}");
        assert!(report.contains("zombies"), "{report}");
        assert!(
            !report.contains("cannot be stopped"),
            "SIGCHLD has nothing to do with stoppability: {report}"
        );
    }

    #[test]
    fn a_missed_stop_signal_names_stoppability() {
        let report = uninstalled_report(&[libc::SIGTERM]).expect("a report");
        assert!(report.contains("SIGTERM"), "{report}");
        assert!(report.contains("cannot be stopped"), "{report}");
    }

    /// Every one of them at once: each consequence must appear, and the report
    /// must not stop at the first.
    #[test]
    fn a_partial_install_reports_every_lost_guarantee() {
        let report =
            uninstalled_report(&[libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGCHLD])
                .expect("a report");
        for expected in ["SIGINT", "SIGQUIT", "SIGTERM", "SIGCHLD"] {
            assert!(
                report.contains(expected),
                "{expected} missing from {report}"
            );
        }
        assert!(report.contains("cannot be stopped"), "{report}");
        assert!(report.contains("zombies"), "{report}");
    }

    /// A signal this crate does not configure has no consequence it can name.
    /// Guessing one would be a lie in a message whose whole purpose is to be
    /// trusted.
    #[test]
    fn an_unconfigured_signal_reports_no_consequence() {
        let report = uninstalled_report(&[-1]).expect("a report");
        assert!(report.contains("an unrecognised signal"), "{report}");
        assert!(
            report.contains("no window-manager guarantee is known to be lost"),
            "{report}"
        );
    }

    /// Raw signal numbers are not a message. The whole point of reporting is
    /// that someone can act on it.
    #[test]
    fn every_reported_signal_is_named() {
        let report =
            uninstalled_report(&[libc::SIGPIPE, libc::SIGCONT, libc::SIGUSR1]).expect("a report");
        for expected in ["SIGPIPE", "SIGCONT", "SIGUSR1"] {
            assert!(
                report.contains(expected),
                "{expected} missing from {report}"
            );
        }
    }
}
