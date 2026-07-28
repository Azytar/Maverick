// maverick/src/main.rs

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
mod config;
pub mod core;
mod log;
mod types;

use std::process;

fn main() {
    log::init();
    log::info!("maverick v{} starting", env!("CARGO_PKG_VERSION"));

    // Parse args (any order). Only --name is added; -v/-h stay.
    let mut instance_name = maverick_sys::DEFAULT_NAME.to_string();
    let mut show_help = false;
    let mut show_version = false;
    let mut bad_arg: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-v" | "--version" => show_version = true,
            "-h" | "--help" => show_help = true,
            "--name" => {
                if let Some(n) = args.next() {
                    instance_name = n
                } else {
                    bad_arg = Some("--name requires a value".into());
                    break;
                }
            }
            unknown => {
                bad_arg = Some(format!("unknown argument: {unknown}"));
                break;
            }
        }
    }

    if let Some(msg) = bad_arg {
        eprintln!("maverick: {msg}");
        process::exit(1);
    }
    if show_version {
        println!("maverick {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }
    if show_help {
        println!("Usage: maverick [--name <id>] [-v] [-h]");
        println!("  --name <id>      Instance name for control/identification");
        println!("  -v, --version    Print version and exit");
        println!("  -h, --help       Show this help");
        println!();
        println!("Configuration is compiled into the binary (src/config.rs).");
        println!("Start from .xinitrc: exec maverick");
        process::exit(0);
    }

    log::info!("instance name: {}", instance_name);

    // Export the instance name so child processes (notably `maverickctl`, e.g.
    // the Mod+Shift+Q quit-confirm keybind) target *this* instance by default,
    // even when several Mavericks run on different TTYs/DISPLAYs.
    std::env::set_var("MAVERICK_INSTANCE", &instance_name);

    maverick_sys::detach_from_terminal();
    maverick_sys::Signal::new()
        .ignore(libc::SIGPIPE)
        .on_sigterm(libc::SIGTERM)
        .on_sigcont(libc::SIGCONT)
        .install();

    // ── Identity + control socket ───────────────────────────────────────────
    // Advertise this instance so an external tool can discover/close it,
    // even when several Mavericks run on different TTYs/DISPLAYs.
    let info = maverick_sys::self_info(&instance_name);
    if let Err(e) = maverick_sys::identity::write_meta(&info) {
        log::warn!("failed to write instance ficha: {e}");
    }
    let identity_json = maverick_sys::control::identity_json(&info);
    // The hub bridges the control-socket thread and the WM event loop: it
    // queues dispatched commands, caches the state snapshot, and fans out
    // events to `subscribe` clients.
    let hub = maverick_sys::ControlHub::new();
    let control =
        match maverick_sys::ControlServer::spawn(&instance_name, identity_json, hub.clone()) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("failed to start control socket: {e}");
                None
            }
        };

    // Write PID file so external tools can find us (legacy compat).
    if let Err(e) = std::fs::write("/tmp/maverick.pid", format!("{}\n", std::process::id())) {
        log::warn!("failed to write PID file: {e}");
    }

    let cfg = config::load_config();
    log::info!(
        "config: {} tags, {} keybinds, {} rules, {} autostart",
        cfg.tag_names.len(),
        cfg.keybinds.len(),
        cfg.rules.len(),
        cfg.autostart.len(),
    );

    // ── Phase 1: compositor ───────────────────────────────────────────────────
    // Picom starts BEFORE WindowManager::new() so every window receives
    // compositing from its very first frame — no flash of uncomposited content.
    let compositor_cmd = cfg.compositor.clone();
    let compositor_delay = cfg.compositor_delay_ms;

    if let Some((bin, args)) = compositor_cmd.split_first() {
        match std::process::Command::new(bin)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => log::info!("compositor '{}' started", bin),
            Err(e) => log::warn!("compositor '{}' failed: {}", bin, e),
        }
        if compositor_delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(compositor_delay));
        }
    }

    // ── Phase 2: WM init ──────────────────────────────────────────────────────
    match backend::x11::WindowManager::new(cfg) {
        Ok(mut manager) => {
            // Hand over the control socket + instance name so cleanup() can
            // tear them down and remove the identity ficha on exit.
            manager.set_instance_name(instance_name.clone());
            manager.set_hub(hub);
            if let Some(server) = control {
                manager.set_control(server);
            }
            // ── Phase 3: startup sound ────────────────────────────────────────
            // Compositor is up, WM is ready — ideal moment for the startup chime.
            let sound = manager.engine.cfg.startup_sound.clone();
            let sound_default = "/usr/share/sounds/freedesktop/stereo/service-login.oga";
            let sound_path = sound.as_deref().unwrap_or(sound_default);
            if std::path::Path::new(sound_path).exists() {
                play_sound(sound_path);
            }

            // ── Phase 4: autostart apps ───────────────────────────────────────
            // All apps start after compositor + WM are ready, so they get
            // compositing from frame 0 and the WM manages them from the start.
            for cmd in &manager.engine.cfg.autostart.clone() {
                if let Some((bin, args)) = cmd.split_first() {
                    if let Err(e) = std::process::Command::new(bin)
                        .args(args)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        log::error!("autostart '{}' failed: {}", bin, e);
                    }
                }
            }

            // ── Phase 5: event loop ───────────────────────────────────────────
            match manager.run() {
                Ok(()) => {
                    let disconnected = manager.engine.state.running;
                    if disconnected {
                        log::warn!("maverick: X server disconnected — exiting");
                    } else {
                        log::info!("maverick exiting cleanly");
                        if let Err(e) = manager.cleanup() {
                            log::warn!("cleanup error: {e}");
                        }
                    }
                    let _ = std::fs::remove_file("/tmp/maverick.pid");
                }
                Err(e) => {
                    log::error!("fatal error in event loop: {e}");
                    let _ = manager.cleanup();
                    let _ = std::fs::remove_file("/tmp/maverick.pid");
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("maverick: failed to initialise: {e}");
            // Clean up the identity ficha written earlier so it doesn't
            // linger and confuse tools that list instances.
            maverick_sys::identity::cleanup_meta(&instance_name);
            process::exit(1);
        }
    }
}

/// Play a sound file asynchronously.
/// Tries pw-play → paplay → canberra-gtk-play → mpv → aplay in order.
fn play_sound(path: &str) {
    let candidates: &[(&str, &[&str])] = &[
        ("pw-play", &[path] as &[&str]),
        ("paplay", &[path]),
        ("canberra-gtk-play", &["-i", "service-login"]),
        ("mpv", &["--no-video", path]),
        ("aplay", &[path]),
    ];
    for (bin, args) in candidates {
        let ok = std::process::Command::new(bin)
            .args(*args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok();
        if ok {
            log::info!("startup sound: playing via {}", bin);
            return;
        }
    }
    log::warn!("startup sound: no audio player found");
}

// Detach + signal setup now live in the `maverick-sys` crate, which is the
// only place in the project that touches libc FFI. See `detach_from_terminal`
// and `Signal` there.
