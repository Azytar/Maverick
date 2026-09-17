//! Process-wide stderr logger — minimal replacement for `log` + `env_logger`.
//!
//! Role: level-filtered `eprintln!` macros (`error!`, `warn!`, `info!`, `debug!`)
//! gated by a single `AtomicU8` `LEVEL`. `init()` reads `MAVERICK_LOG`
//! (fallback `RUST_LOG`) once at startup; `enabled(level)` is the inline gate
//! used by each macro.
//!
//! Boundary: owns only the global `LEVEL` and the formatting in the macros.
//! No file rotation, no timestamps, no ANSI colour, no regex filtering, and no
//! dependency on external logging crates. Does not own config, X, or compositor
//! state.
//!
//! # Ownership
//!
//! `LEVEL` is a process-wide `AtomicU8` (relaxed ordering) initialized to
//! `INFO` and overwritten exactly once by `init()`. Macros capture
//! `format!` output and write to stderr; no handle is retained.
//!
//! # Lifecycle
//!
//! Call `init()` once before any `log::info!` etc. (the binary does this first
//! in `main`). Subsequent level changes are not supported — the value is
//! fixed for the session.

use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const ERROR: u8 = 1;
pub(crate) const WARN: u8 = 2;
pub(crate) const INFO: u8 = 3;
pub(crate) const DEBUG: u8 = 4;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

/// Reads `MAVERICK_LOG` (falls back to `RUST_LOG` for muscle-memory compat).
/// Anything unrecognized defaults to `info`, matching the old `env_logger` setup.
pub(crate) fn init() {
    let raw = std::env::var("MAVERICK_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_default();
    let level = match raw.to_ascii_lowercase().as_str() {
        "off" => 0,
        "error" => ERROR,
        "warn" => WARN,
        "debug" | "trace" => DEBUG,
        _ => INFO,
    };
    LEVEL.store(level, Ordering::Relaxed);
}

#[inline]
pub(crate) fn enabled(level: u8) -> bool {
    level <= LEVEL.load(Ordering::Relaxed)
}

macro_rules! info {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::INFO) {
            eprintln!("[INFO]  {}", format!($($arg)*));
        }
    };
}

macro_rules! warn_ {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::WARN) {
            eprintln!("[WARN]  {}", format!($($arg)*));
        }
    };
}

macro_rules! error {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::ERROR) {
            eprintln!("[ERROR] {}", format!($($arg)*));
        }
    };
}

macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::DEBUG) {
            eprintln!("[DEBUG] {}", format!($($arg)*));
        }
    };
}

pub(crate) use {debug, error, info, warn_ as warn};

/// Opt-in diagnostics only. May contain config paths and command arguments;
/// do not enable on a session containing sensitive bindings/autostart entries.
pub(crate) fn config_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("MAVERICK_CONFIG_TRACE").is_some_and(|v| v == "1"))
}

/// Process-relative monotonic timestamp plus sequence; never used by WM policy.
pub(crate) fn config_trace(event: &str, details: std::fmt::Arguments<'_>) {
    if !config_trace_enabled() {
        return;
    }
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let us = START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    eprintln!("CONFIG-TRACE seq={seq} t_us={us} event={event} {details}");
}

/// FNV-1a over a deterministic same-binary Debug representation (no maps or
/// pointers in Cfg). Not a security hash or a cross-version serialization API.
pub(crate) fn config_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

pub(crate) fn config_snapshot(stage: &str, cfg: &crate::config::Cfg) {
    if config_trace_enabled() {
        let snapshot = format!("{cfg:?}");
        config_trace(
            stage,
            format_args!(
                "fingerprint={:016x} config={snapshot}",
                config_fingerprint(snapshot.as_bytes())
            ),
        );
    }
}
