// maverick/src/main.rs

mod backend;
mod config;
pub mod core;
mod log;
mod types;

use std::process;

fn main() {
    log::init();
    log::info!("maverick v{} starting", env!("CARGO_PKG_VERSION"));

    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-v" | "--version" => {
                println!("maverick {}", env!("CARGO_PKG_VERSION"));
                process::exit(0);
            }
            "-h" | "--help" => {
                println!("Usage: maverick [-v] [-h]");
                println!("  -v, --version    Print version and exit");
                println!("  -h, --help       Show this help");
                println!();
                println!("Configuration is compiled into the binary (src/config.rs).");
                println!("Start from .xinitrc: exec maverick");
                process::exit(0);
            }
            unknown => {
                eprintln!("maverick: unknown argument: {unknown}");
                process::exit(1);
            }
        }
    }

    setup_signals();
    detach_from_terminal();

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

/// Detach from the launching terminal so the WM outlives the shell.
fn detach_from_terminal() {
    unsafe {
        libc::setsid();
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }
        let devnull = std::ffi::CString::new("/dev/null").unwrap();
        let fd = libc::open(devnull.as_ptr(), libc::O_RDWR);
        if fd < 0 {
            return;
        }
        libc::dup2(fd, libc::STDIN_FILENO);
        libc::dup2(fd, libc::STDOUT_FILENO);
        // stderr left open so log messages reach journald / the terminal.
        if fd > 2 {
            libc::close(fd);
        }
    }
}

fn setup_signals() {
    unsafe {
        // SIGCHLD: reap children without zombies.
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        sa.sa_flags = libc::SA_NOCLDWAIT | libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());

        // SIGPIPE: ignore — broken pipes must not kill the WM.
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGPIPE, &sa, std::ptr::null_mut());

        // SIGCONT: flag that pointer/keyboard grabs need to be redone
        // after a system suspend/resume cycle.
        extern "C" fn sigcont_handler(_: libc::c_int) {
            crate::backend::x11::NEED_REGRAB.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigcont_handler as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCONT, &sa, std::ptr::null_mut());
    }
}
