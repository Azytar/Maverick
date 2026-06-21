// maverick/src/log.rs
// Tiny level-filtered stderr logger, purpose-built to replace `log` + `env_logger`.
// Maverick only has ~15 call sites across 4 levels, so the regex/ansi-colour/datetime
// machinery those crates pull in (anstream, regex, jiff, ...) buys nothing here.

use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const ERROR: u8 = 1;
pub(crate) const WARN: u8 = 2;
pub(crate) const INFO: u8 = 3;
pub(crate) const DEBUG: u8 = 4;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

/// Reads `MAVERICK_LOG` (falls back to `RUST_LOG` for muscle-memory compat).
/// Anything unrecognized defaults to `info`, matching the old env_logger setup.
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
