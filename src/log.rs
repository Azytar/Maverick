//! Process-wide stderr logger — minimal replacement for `log` + `env_logger`.
//!
//! Level-filtered `eprintln!` macros (`error!`, `warn!`, `info!`, `debug!`)
//! gated by a single `AtomicU8` `LEVEL`. `init()` reads `MAVERICK_LOG` (falling
//! back to `RUST_LOG`) once at startup; `enabled(level)` is the inline gate each
//! macro checks. No file rotation, timestamps, ANSI color, or regex filtering.
//!
//! `LEVEL` is a process-wide `AtomicU8` written by `init()` and read by the
//! macros on the event-loop hot path. Relaxed ordering is enough: the level is
//! advisory — worst case a macro sees the previous level for one call — and no
//! other memory is ordered against it. Call `init()` before the first log call;
//! the value is fixed for the session, so a later `MAVERICK_LOG` change has no
//! effect.

use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const ERROR: u8 = 1;
pub(crate) const WARN: u8 = 2;
pub(crate) const INFO: u8 = 3;
pub(crate) const DEBUG: u8 = 4;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

/// Reads `MAVERICK_LOG` (falling back to `RUST_LOG` for muscle-memory compat).
/// Anything unrecognized — including an empty value — means `info`, so a typo
/// never silences the log.
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

/// Opt-in config-pipeline trace (`MAVERICK_CONFIG_TRACE=1`), resolved once.
/// It prints full config snapshots and file paths, so leave it off on a session
/// whose bindings or autostart entries are sensitive.
pub(crate) fn config_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("MAVERICK_CONFIG_TRACE").is_some_and(|v| v == "1"))
}

/// Emit one `CONFIG-TRACE` line: process-relative microseconds since the first
/// trace event plus a monotonic sequence number, so concurrent traces can be
/// ordered without a wall clock. Never used by WM policy.
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

/// FNV-1a over a deterministic, same-binary `Debug` rendering of the config
/// (`Cfg` contains no maps, pointers or addresses). It identifies "did the
/// config change between two trace points" and nothing else: not a security
/// hash, and not stable across builds or versions.
pub(crate) fn config_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// Trace one `Cfg` at a named pipeline stage, fingerprinted so two snapshots can
/// be compared without diffing the whole rendering. A no-op unless
/// `config_trace_enabled`.
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
