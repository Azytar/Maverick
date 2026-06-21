// maverick/src/main.rs
// High-performance tiling window manager — config is compiled in (see config.rs).

mod config;
mod types;
pub mod core;
mod backend;
mod log;

use std::process;

fn main() {
    log::init();

    log::info!("maverick v{} starting...", env!("CARGO_PKG_VERSION"));

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
                println!("Start from .xinitrc with: exec maverick");
                process::exit(0);
            }
            unknown => {
                eprintln!("Unknown argument: {unknown}");
                process::exit(1);
            }
        }
    }

    setup_signals();
    detach_from_terminal();

    let cfg = config::load_config();
    log::info!(
        "Embedded config: {} tags, {} keybinds, {} rules",
        cfg.tag_names.len(),
        cfg.keybinds.len(),
        cfg.rules.len()
    );

    // Ejemplo de cómo tendrías que ejecutarlo en tu código principal
    for cmd in &cfg.autostart {
        if let Some((program, args)) = cmd.split_first() {
            if let Err(e) = std::process::Command::new(program)
                .args(args)
                .spawn()
            {
                log::error!("Failed to autostart '{}': {}", program, e);
            }
        }
    }

    match backend::x11::WindowManager::new(cfg) {
        Ok(mut manager) => match manager.run() {
            Ok(()) => {
                log::info!("maverick exiting cleanly");
                if let Err(e) = manager.cleanup() {
                    log::warn!("Cleanup error: {e}");
                }
            }
            Err(e) => {
                log::error!("Fatal error in event loop: {e}");
                process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("maverick: failed to initialize: {e}");
            process::exit(1);
        }
    }
}

/// Detach from the launching shell (dwm/i3 pattern: WM must not die with a terminal).
fn detach_from_terminal() {
    unsafe {
        libc::setsid();

        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }

        let devnull = std::ffi::CString::new("/dev/null").expect("valid path");
        let fd = libc::open(devnull.as_ptr(), libc::O_RDWR);
        if fd < 0 {
            return;
        }
        libc::dup2(fd, libc::STDIN_FILENO);
        libc::dup2(fd, libc::STDOUT_FILENO);
        // libc::dup2(fd, libc::STDERR_FILENO); // Disabled for debugging
        if fd > 2 {
            libc::close(fd);
        }
    }
}

fn setup_signals() {
    unsafe {
        // SIGCHLD: default disposition, but don't generate zombies and restart
        // interrupted syscalls (the old nix-based version did the same).
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        sa.sa_flags = libc::SA_NOCLDWAIT | libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());

        // SIGPIPE: ignore (broken pipes shouldn't kill the WM).
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGPIPE, &sa, std::ptr::null_mut());

        // SIGCONT: flag that grabs may need to be redone after a freeze/thaw.
        extern "C" fn sigcont_handler(_: libc::c_int) {
            crate::backend::x11::NEED_REGRAB.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigcont_handler as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCONT, &sa, std::ptr::null_mut());
    }
}
