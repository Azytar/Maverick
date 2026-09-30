//! `maverickctl` — external Unix control tool for the Maverick window manager.
//!
//! This crate owns everything the window manager does not need to manage
//! windows: CLI parsing ([`ctl`]), the control-socket client ([`client`]),
//! instance discovery ([`discover`]) and session orchestration ([`session`]).
//! It drives a running Maverick over `control.sock`; the only Maverick crate
//! it links is [`maverick_sys`], for the shared protocol surface (socket
//! paths, peer-credential authorization, line framing, JSON helpers).

pub mod client;
pub mod ctl;
pub mod discover;
pub mod session;
#[cfg(test)]
pub(crate) mod test_support;
