//! Window manager core — niri-style columnar layout, clean coords.
//!
//! This is the main X11 backend. It owns the X connection, the event
//! loop, the window lifecycle, the compositor, and the translation
//! from `Effect` (the core's semantic vocabulary) into X11 protocol
//! calls.
//!
//! # Architecture
//!
//! ```text
//! X11 events → dispatch → Command::execute → State mutation → Effect
//!     → Backend::execute → DesiredState → Reconciler → AppliedState → X11
//! ```
//!
//! - **`WindowManager`** owns the X connection (`Rc<XConn>` shared with
//!   the compositor), the event loop, the keymap, the pointer grab
//!   state, and the compositor handle.
//! - **`dispatch`** handles every X11 event and translates it into a
//!   `Command` that the `Engine` executes.
//! - **`manage`/`unmanage`** handle the client lifecycle (scan, map,
//!   focus, unmap, destroy).
//! - **`render`** is the arrange+stack+reconcile pipeline that turns
//!   `State` into `DesiredState` and applies it to X11.
//! - **`pointer`** handles pointer grabs, drag/resize, and
//!   focus-follows-mouse.
//! - **`input`** sets up the root window, XKB, and key/button grabs.
//! - **`ewmh`** publishes EWMH properties on the root window.
//! - **`struts`** translates dock strut properties into workarea
//!   reservations.
//! - **`actions`** bridges `Effect` into X11 calls (the future
//!   Wayland backend replaces only this module).
//! - **`framesched`** decides when to render based on animation,
//!   damage, and geometry changes.
//! - **`reconciler`** diffs Desired vs Applied geometry and emits only
//!   the `ConfigureWindow` calls that actually changed.
//! - **`compositor`/`compositor_gl`** handle the OpenGL compositor
//!   lifecycle and the per-frame render pipeline.
//! - **`events`** handles X11 event callbacks dispatched from `mod.rs`.
//! - **`hubevents`** bridges domain events to the control-hub wire
//!   protocol.
//!
//! # X11 connection sharing
//!
//! The `conn: Rc<XConn>` is shared with the compositor so both see the
//! same sequence-number space and event queue. `XDisplay` is `Copy`
//! not `Drop` because the `XCBConnection` borrows its
//! `xcb_connection_t*` with `should_drop = false` — see
//! `maverick_x11` for the safety invariants.
//!
//! # Safety
//!
//! The only `unsafe` in this area is the `FD_CLOEXEC` `fcntl` in
//! `actions::restart`, documented there. The X11 FFI invariants — the
//! `Display*`/`xcb_connection_t` pairing in particular — live in
//! `maverick_x11` and `maverick_gl`.
//!
//! # Invariants
//!
//! - Every X11 request goes through the same `XCBConnection` — never
//!   two sockets.
//! - `AppliedState` is the sole gate for `ConfigureWindow` calls.
//! - `classify_configure` decides whether a reported geometry is our own
//!   echo or stale traffic; the model is never taken from a
//!   `ConfigureNotify`.
//! - Float geometry is clamped to the workarea before emission.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use x11rb::connection::Connection;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xkb::{self, KeySymMap, KeyType, MapPart};
use x11rb::protocol::{xproto::*, Event};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use maverick_x11::{XConn, XDisplay};

use crate::backend::atoms::Atoms;
use crate::config::Cfg;
use crate::core::layout::{
    arrange, fixed_size_hints, ideal_scroll, parse_wm_normal_hints, snap_float_to_hints,
    Placements, RibbonScratch,
};
use crate::core::{parse_action, state_json, Effect, Engine};
use crate::log;
use crate::types::*;

mod actions;
mod events;
mod ewmh;
mod hubevents;
mod input;
mod manage;
mod pointer;
pub(crate) mod reconciler;
mod render;
mod struts;
mod teardown;
pub use teardown::ShutdownReason;
use teardown::{runs_x_half, LiveX};
mod trace;
use trace::trace;
use trace::TraceEnd;
#[cfg(test)]
mod tests;
use pointer::DragState;

/// How long a keyboard-change notification waits for its siblings before the
/// keymap is re-read and every grab rebuilt. A single `setxkbmap` produces a
/// core `MappingNotify` *and* an XKB `MapNotify` (plus a `NewKeyboardNotify` on
/// hotplug); 50 ms is far below human perception and comfortably wider than the
/// gap between them.
const KBD_REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// How long the event loop may block on its next turn.
///
/// Every bound the loop owns is folded in here and enforced nowhere else. A
/// settled window manager has no work and so asks for **no** timeout at all,
/// blocking until the X connection or the control socket has something; that is
/// what keeps an idle window manager off the CPU, but it also means a bound
/// checked only *between* turns is not a bound — the wait never returns, so the
/// check that owns it never runs. Folding the deadlines into the wait is what
/// makes them real.
///
/// The shutdown budget is the one that bites: `begin_shutdown` arms 3 s and
/// then keeps pumping the loop so cooperative clients can close, but a client
/// that advertises `WM_DELETE_WINDOW` and ignores it leaves the loop settled,
/// the wait unbounded, and the budget unreachable until some unrelated X event
/// happens to arrive.
///
/// `frame` is the frame scheduler's own answer — 0 ms when a frame is pending —
/// and `keyboard` and `shutdown` are absolute deadlines. `None` throughout
/// means "no work and no deadline", i.e. block until something happens.
fn wait_timeout(
    frame: Option<std::time::Duration>,
    keyboard: Option<Instant>,
    shutdown: Option<Instant>,
) -> Option<std::time::Duration> {
    let mut timeout = frame;
    for deadline in [keyboard, shutdown].into_iter().flatten() {
        let left = deadline.saturating_duration_since(Instant::now());
        timeout = Some(timeout.map_or(left, |current| current.min(left)));
    }
    timeout
}

/// The X11 backend — owns the single `Rc<XConn>` + `XDisplay`, the `State`/
/// `Engine`, EWMH, grabs, and the compositor handle.
///
/// # Ownership
///
/// `Rc<XConn>` is shared with `Compositor` so both issue requests over the same
/// `xcb_connection_t` (sequence-number coherent, one socket). `XDisplay` is
/// `Copy` / non-`Drop` with `should_drop=false` (invariant proved by
/// `maverick_x11::open_x`): the `Display*` stays live as long as either `Rc`
/// holder lives.
///
/// # Lifecycle
///
/// Created by `WindowManager::new` (opens X, claims `SUBSTRUCTURE_REDIRECT`,
/// scans windows, arranges), driven by `run` → `run_once` (flush → drain →
/// animate → present → wait → keyboard → control), torn down by `cleanup`.
///
/// # Protocol why
///
/// `SubstructureRedirect` is the ICCCM WM election; `_NET_SUPPORTING_WM_CHECK` +
/// `_NET_SUPPORTED` advertise EWMH; `RandR` monitors drive `workarea`; XKB +
/// core `MappingNotify` drive keymap refresh.
pub struct WindowManager {
    /// The one X connection. It is an `XCBConnection` (not `RustConnection`)
    /// because it is the *same* `xcb_connection_t` the Xlib `Display` below
    /// owns: GLX needs a `Display*`, the WM needs XCB, and sharing one socket
    /// is the only way both can agree on sequence numbers and see the same
    /// event queue. See `maverick_gl::open_x`.
    conn: Rc<XConn>,
    /// The Xlib display backing `conn`, kept only so GLX has something to talk
    /// to. Never used for X *events* — XCB owns the queue. The compositor holds
    /// its own `Copy` of it; this field just pins the `Display*` open for the
    /// whole process (it is not `Drop`, so the connection survives either way).
    #[allow(dead_code)]
    dpy: XDisplay,
    screen_num: usize,
    root: Window,
    atoms: Atoms,
    pub engine: Engine,
    layout_registry: crate::core::layout::LayoutRegistry,
    check_win: Window,
    numlock: u16,
    /// Modifier-map column that carries Scroll Lock (0 when unmapped). Treated
    /// as an ignored lock like Caps/Num: grabs get `| scroll` variants and
    /// `clean_mask` strips it, so binds fire with the lock on or off.
    scroll: u16,
    keymap: BTreeMap<(u16, u32), crate::types::Action>,
    raw_keymap: Vec<u32>,
    raw_kpk: usize,
    raw_min: u8,
    /// Unified XKB keymap used by both passive-grab planning and
    /// `KeyPress` dispatch. `None` keeps the legacy core-mapping fallback
    /// for servers where the XKB extension is unavailable.
    xkb: Option<XkbKeyboardMap>,
    /// Effective XKB group captured with `xkb`. Planner uses this
    /// snapshot; dispatch prefers the event group and falls back to this
    /// value when `XGrabKey` delivery omits group bits.
    xkb_group: u8,
    /// Warnings the last `grab_keys` produced (unbindable keysyms, rejected
    /// grabs). Grabs are rebuilt on every keyboard change — and tools driving
    /// XTEST make the server report a few of those per run — so an unchanged
    /// complaint is logged once instead of on every rebuild.
    last_grab_warnings: Vec<String>,
    /// Deadline for a pending keyboard refresh. Core `MappingNotify`, XKB
    /// `MapNotify` and XKB `NewKeyboardNotify` all describe the *same* change
    /// and arrive together; each one only arms this deadline, so a burst
    /// collapses into a single `ungrab_key(ANY)` + regrab instead of several
    /// (losing a grab mid-burst is exactly when it hurts).
    kbd_refresh_due: Option<Instant>,
    drag: Option<DragState>,
    /// Deferred `_NET_CLIENT_LIST` update: set on manage/unmanage, flushed once
    /// per event-loop turn so a burst of window changes costs one property
    /// write.
    client_list_dirty: bool,
    /// Deferred restack: only re-stack when the float/fullscreen set changes.
    stack_dirty: bool,
    /// The `Reconciler`'s record of what geometry has actually been written to
    /// X11. Every desired placement is diffed against this so
    /// `configure_window` fires only on real changes.
    applied: crate::backend::x11::reconciler::AppliedState,
    /// No-compositor rounded-corner path (`round_corners`): the last
    /// (`outer_w`, `outer_h`, radius, `bw`) a Shape `BOUNDING` mask was
    /// actually set for, per window. The mask is a pure function of size,
    /// never of position, so this cache is what suppresses the re-upload
    /// during the pure-move configures `emit_geometry` issues for every
    /// visible window on every animation frame. `bw` is part of the key
    /// because the mask origin (`-bw, -bw`) re-anchors with the border.
    shape_mask_cache: std::collections::HashMap<Window, (u32, u32, i32, u32)>,
    /// Last `_NET_FRAME_EXTENTS` border width published per window.
    /// `emit_geometry` is the single writer: it publishes `[bw × 4]` only when
    /// the applied border changed, so fullscreen/maximize/border-rule
    /// transitions stay in sync without a property write per configure.
    frame_extents: std::collections::HashMap<Window, u32>,
    /// Reusable buffers for `hide_offscreen` — avoids reallocation per arrange.
    hide_ws_set: std::collections::HashSet<Window>,
    hide_mon_vec: Vec<Window>,
    /// The single desired representation fed to the `Reconciler`: `layout::arrange`
    /// fills it with the base `(win, geom, border_w)` for every window, then
    /// `present_into` rewrites it in place with the fullscreen/maximized overlay.
    /// The `Reconciler` diffs this `Desired` against `AppliedState` to decide what
    /// to write to X11. Reusable buffer — avoids allocation per `arrange()`.
    desired: Placements,
    /// Per-monitor "is a spring still moving" flag, produced by
    /// `tick_animations_multi`. Lets the frame loop recompute the live layout for
    /// only the monitors that are actually animating. Parallel to `state.monitors`.
    anim_per_mon: Vec<bool>,
    /// Reusable raise-list scratch for `live_placements` → `present_into`.
    /// The WM discards the raise list, so a fresh `Vec` here would allocate once
    /// per animating monitor per frame.
    present_scratch: Vec<WindowId>,
    /// Reusable scratch for the per-frame column projection (`ribbon_geom`).
    /// Without it every `arrange` (once per animating monitor per frame) would
    /// allocate the per-column table. Owned by the WM and threaded through
    /// `arrange` → `Layout::arrange`.
    ribbon_scratch: RibbonScratch,
    /// Rate-limit tracker for key repeat suppression (mods, keysym → last dispatch).
    last_key_times: std::collections::BTreeMap<(u16, u32), std::time::Instant>,
    /// Control socket server (identity + remote quit). None if it failed to start.
    control: Option<maverick_sys::ControlServer>,
    /// Session id (random, per-session) used as the control-socket/ficha key.
    session_id: String,
    /// Config file path that was loaded at boot (the --config override when
    /// given, otherwise the resolved XDG path, or `None` when the compiled
    /// defaults were used). `reload_config` re-reads this exact file: the
    /// override must survive a reload, not be silently replaced by the
    /// XDG default.
    config_path: Option<PathBuf>,
    /// Original command-line arguments (excluding argv[0]) captured at startup.
    /// `restart` re-execs with these EXACTLY, so the new instance reuses the
    /// same --config/--name/--replace instead of silently falling back to
    /// XDG/defaults.
    launch_args: Vec<String>,
    /// When `Some`, a graceful shutdown is in progress: clients were asked to
    /// close cooperatively and Maverick will terminate once either all clients
    /// are gone OR this deadline elapses. The deadline is a HARD upper bound —
    /// shutdown never depends on client cooperation to finish.
    shutdown_deadline: Option<std::time::Instant>,
    /// Bridge to the control-socket thread: drains dispatched commands, publishes
    /// state snapshots, and emits events for `subscribe` clients.
    hub: Option<maverick_sys::ControlHub>,
    /// Last state snapshot published to the hub — avoids re-publishing identical
    /// JSON on every loop iteration.
    last_state_json: String,
    /// External dock windows we currently reserve space for, mapped to the
    /// monitor index whose `reserved_regions` hold their reservation. Used to
    /// remove the reservation exactly when the dock is destroyed/unmapped.
    docks: std::collections::HashMap<Window, usize>,
    /// When set, `EnterNotify`-driven focus (focus-follows-mouse) is ignored.
    /// Armed right after keyboard navigation and other programmatic focus
    /// changes so the pointer — parked over a tile edge — can't instantly undo
    /// the key-driven switch. Cleared by the first real `MotionNotify`.
    pointer_guard_until: Option<std::time::Instant>,
    /// Server time of the most recent input event (key/button/enter). Used to
    /// stamp ICCCM `WM_TAKE_FOCUS` messages with a real timestamp instead of
    /// `CurrentTime`, which a few strict toolkits (some Java/Emacs builds)
    /// refuse to act on.
    last_event_time: u32,
    /// Always false. The loop is idle, so nothing consumes it; kept so the
    /// "is anything moving" question has one answer at every call site.
    animating: bool,
    /// Per-monitor cached stacking order (top-to-bottom) so `stack_overlay`
    /// only re-issues `raise()` when the order actually changed, instead of
    /// re-raising every float/popup on every animation frame.
    last_stack_order: std::collections::HashMap<usize, Vec<WindowId>>,
    /// Per-monitor record of which fullscreen window was "covering" (raised
    /// above the dock) on the previous frame, so the dock is only re-raised on
    /// the covering→not-covering transition. Re-raising it every frame would
    /// push floats below the bar.
    fs_covering: std::collections::HashMap<usize, Option<WindowId>>,
    /// Reconcile work the events drained this turn owe, collapsed into at most
    /// one pass per monitor.
    pending: PendingReconcile,
    /// `WM_PROTOCOLS` of each managed client, read once and kept fresh by
    /// `PropertyNotify`. `focus()` and `kill()` ask "does it speak
    /// `WM_TAKE_FOCUS` / `WM_DELETE_WINDOW`?" on the input path, and answering
    /// with a `GetProperty` round trip there stalled the single event-loop
    /// thread once per focus change. A `RefCell` because `has_protocol` is `&self`.
    protocols: std::cell::RefCell<std::collections::HashMap<Window, Vec<u32>>>,
    /// A `FocusOut` arrived this turn and the real X focus has not been
    /// compared with the logical one yet. The comparison is a blocking
    /// `GetInputFocus`; deferring it to the end of the turn makes a burst of
    /// focus events cost one probe, taken against the final state.
    focus_probe_due: bool,
    /// `Mod4+wheel` notches received this turn and not applied yet, in order.
    /// Applied together by `flush_pending`: N notches in one turn move the
    /// focus N columns but pay for one focus change and one arrange, not N.
    /// The direction list (not a net count) keeps edge clamping exact.
    wheel_steps: Vec<Dir>,
}

/// The layout, stacking and pointer work owed by input, drained once per turn.
///
/// A focus change has to arrange a monitor, restack its overlays and put the
/// pointer on the window that ended up focused. Doing that inline makes the
/// cost of handling one event proportional to the window count — and it is the
/// requests themselves, not the CPU, that make it worse: each arrange moves
/// every window, which makes the server answer with a `ConfigureNotify` per
/// window, each of which this loop then has to dispatch. A burst of N focus
/// changes over one monitor therefore costs N full passes and N×windows worth
/// of new events to consume, and the loop falls further behind for as long as
/// input keeps arriving.
///
/// Nothing here loses state. The passes are pure functions of logical state and
/// diff against `AppliedState`, so collapsing N of them into the last one
/// produces the same geometry and the same stack with a fraction of the
/// requests — and correspondingly fewer events to come back. What has to stay
/// bounded is the *bookkeeping*, and one entry per monitor is bounded by the
/// monitor count no matter how many events arrived.
///
/// Focus is recorded per monitor rather than per event because only the last
/// one survives into the flushed pass: it is the one whose post-arrange geometry
/// the pointer warp should target, and the one whose stacking the restack reads.
#[derive(Debug, Default, PartialEq, Eq)]
struct PendingReconcile {
    /// monitor -> the window whose focus this turn's pass should finish.
    monitors: std::collections::BTreeMap<usize, Option<Window>>,
}

impl PendingReconcile {
    /// Record that `mon` owes a reconcile pass, to finish `focus` if one is
    /// named. A later focus for the same monitor replaces an earlier one; a
    /// later `None` does not erase a focus that is still owed.
    fn mark(&mut self, mon: usize, focus: Option<Window>) {
        self.monitors
            .entry(mon)
            .and_modify(|slot| {
                if focus.is_some() {
                    *slot = focus;
                }
            })
            .or_insert(focus);
    }

    fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }

    /// Hand over everything owed, leaving the set empty for the next turn.
    fn take(&mut self) -> std::collections::BTreeMap<usize, Option<Window>> {
        std::mem::take(&mut self.monitors)
    }
}

impl WindowManager {
    fn dispatch(&mut self, ev: x11rb::protocol::Event) -> Result<(), Box<dyn std::error::Error>> {
        if trace::enabled() {
            trace::input(&ev);
        }
        let _dispatch_trace = trace::Span::new("event_dispatch");
        match ev {
            Event::ButtonPress(e) => self.on_button_press(e)?,
            Event::ButtonRelease(e) => self.on_button_release(e)?,
            Event::ClientMessage(e) => self.on_client_message(e)?,
            Event::ConfigureNotify(e) => self.on_configure_notify(e)?,
            Event::ConfigureRequest(e) => self.on_configure_request(e)?,
            Event::DestroyNotify(e) => self.on_destroy(e)?,
            Event::EnterNotify(e) => self.on_enter(e)?,
            Event::FocusIn(e) => self.on_focus_in(e)?,
            Event::FocusOut(e) => self.on_focus_out(e)?,
            Event::KeyPress(e) => {
                if crate::log::config_trace_enabled() {
                    static FIRST_KEYPRESS: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !FIRST_KEYPRESS.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        crate::log::config_trace("first_keypress", format_args!("event={e:?}"));
                    }
                    crate::log::config_trace(
                        "key_raw",
                        format_args!(
                            "keycode={} state={:#06x} group={} event={e:?}",
                            e.detail,
                            u16::from(e.state),
                            (u16::from(e.state) >> 13) & 3
                        ),
                    );
                }
                let result = self.on_key(e);
                crate::log::config_trace(
                    "key_handler_end",
                    format_args!("keycode={} time={} result={result:?}", e.detail, e.time),
                );
                result?;
            }
            Event::MappingNotify(e) => self.on_mapping(&e),
            Event::MapRequest(e) => self.on_map_request(e)?,
            Event::MotionNotify(e) => self.on_motion(e)?,
            Event::PropertyNotify(e) => self.on_property(e)?,
            Event::UnmapNotify(e) => self.on_unmap(e)?,
            // RandR change events (config/grab selected in `setup_root`): both
            // the 1.5 `NotifyEvent` (crtc/output changes) and the classic
            // `ScreenChangeNotifyEvent` funnel into the same re-detect handler as
            // a root ConfigureNotify would.
            Event::RandrNotify(_) | Event::RandrScreenChangeNotify(_) => {
                self.handle_monitor_change()?
            }
            // XKB keyboard changes. `MapNotify` covers remaps that never raise a
            // core `MappingNotify` (a pure XKB `setxkbmap`), `NewKeyboardNotify`
            // covers hotplug. A `StateNotify` with `GROUP_STATE` set is a layout
            // toggle: the active group moved, so grabs and the projected
            // group keysyms must be rebuilt. All share the debounced refresh, so
            // the usual burst regrabs only once.
            Event::XkbMapNotify(_) | Event::XkbNewKeyboardNotify(_) => {
                self.schedule_keyboard_refresh();
            }
            Event::XkbStateNotify(s) => {
                if s.changed.contains(xkb::StatePart::GROUP_STATE) {
                    self.kbd_refresh_due = None;
                    self.refresh_keyboard();
                }
            }
            // Errors from the many fire-and-forget requests the WM issues
            // (`let _ = …`). Debug, not warn: `BadWindow` from a client that
            // died between our request and the server processing it is routine
            // — `maverick-gl` installs a silent Xlib error handler for the same
            // reason. Without this arm a `BadAccess` from a rejected grab was
            // simply invisible.
            Event::Error(e) => log::debug!("X error: {e:?}"),
            _ => {}
        }
        Ok(())
    }
    /// Arm the debounced keyboard refresh. All three change notifications
    /// (core `MappingNotify`, XKB `MapNotify`, XKB `NewKeyboardNotify`) funnel
    /// here, and a burst of them collapses into one refresh.
    ///
    /// This is a fixed coalescing *window*, not a sliding debounce: the first
    /// notification sets the deadline and later ones do not push it back, so a
    /// continuous stream of events can never starve the refresh.
    pub(super) fn schedule_keyboard_refresh(&mut self) {
        if self.kbd_refresh_due.is_none() {
            self.kbd_refresh_due = Some(Instant::now() + KBD_REFRESH_DELAY);
        }
    }

    /// Re-read the keymap and rebuild every grab. Deliberately infallible:
    /// this runs from the event loop, and a transient failure to read the
    /// keyboard must never take the WM down with it — the previous keymap
    /// stays in place and the next notification retries.
    pub(super) fn refresh_keyboard(&mut self) {
        match fetch_keyboard_state(&self.conn) {
            Ok(ks) => {
                self.raw_keymap = ks.keysyms;
                self.raw_kpk = ks.kpk;
                self.raw_min = ks.min;
                self.numlock = ks.numlock;
                self.scroll = ks.scroll;
                self.xkb = ks.xkb;
                self.xkb_group = ks.xkb_group;
                self.last_key_times.clear();
            }
            Err(e) => {
                log::warn!(
                    "keyboard refresh: could not read the keymap ({e}) — keeping the previous one"
                );
                return;
            }
        }
        if let Err(e) = self.grab_keys() {
            log::warn!("keyboard refresh: regrabbing keys failed ({e})");
        }
        log::debug!(
            "keyboard: keymap refreshed ({} keysyms/keycode, XKB={}, group={})",
            self.raw_kpk,
            if self.xkb.is_some() {
                "enabled"
            } else {
                "core-fallback"
            },
            self.xkb_group
        );
        // Re-grab buttons on every managed window too: the modifier mapping may
        // have moved NumLock, and a stale `grab_button` mask silently breaks
        // Mod4+click.
        let wins: Vec<Window> = self.engine.state.clients.keys().copied().collect();
        for win in wins {
            if let Err(e) = self.grab_buttons(win) {
                log::debug!("keyboard refresh: regrabbing buttons on {win} failed ({e})");
            }
        }
    }
    /// Shut the window manager down, running the X half only when there is
    /// still something to talk to and the local half always.
    ///
    /// This is the only exit from the backend. `why` is a statement about the
    /// *session* — why it is ending — and it reaches the trace header verbatim;
    /// whether the X half runs is decided separately, by whether the connection
    /// can still carry a request ([`teardown::runs_x_half`]).
    ///
    /// The X half's error is captured rather than propagated. Every request in
    /// it is best-effort and a failure there is not this process's problem to
    /// solve, while the local half is the only thing standing between a dead
    /// window manager and a record of itself that outlives it. So the X half is
    /// reported and the local half runs regardless, and only the local half's
    /// own outcome — which cannot fail — is returned.
    pub fn shutdown(&mut self, why: ShutdownReason) -> Result<(), Box<dyn std::error::Error>> {
        // Cloned rather than borrowed so the connection borrow below does not
        // alias `self`: `teardown_x` needs `&mut self` for the client list, and
        // a borrow of `self.conn` would make those two borrows conflict even
        // though they name the same connection.
        let conn = self.conn.clone();
        let live = LiveX::acquire(&conn);
        let ran_x = runs_x_half(why, live.is_some());
        let x_error = if ran_x {
            self.teardown_x(live.as_ref().expect("a live connection was just taken"))
                .err()
        } else {
            None
        };

        if ran_x {
            if let Some(error) = &x_error {
                log::warn!("maverick: X teardown reported an error ({error}); the session record is still being written");
            }
        } else {
            log::warn!("maverick: X teardown skipped — the X server is gone, so there is nothing left to release");
        }
        // The trace reports the X half as it actually went, not as the reason
        // asked for: a clean exit that found no connection is still a lost
        // connection, and `x_teardown=full` must never be written for a teardown
        // that released nothing.
        let local = teardown::run_local(
            &self.session_id,
            &mut self.control,
            if ran_x {
                TraceEnd::CleanExit
            } else {
                TraceEnd::XConnectionLost
            },
        );
        if let Some(error) = &local.trace.error {
            log::warn!(
                "compositor trace dump {}: {error}",
                local.trace.path.display()
            );
        } else if local.trace.written {
            log::info!(
                "compositor trace: {} records written to {}",
                local.trace.records,
                local.trace.path.display()
            );
        }
        log::debug!(
            "shutdown: identity record removed={}, control socket released={}",
            local.ficha_removed,
            local.control_dropped
        );
        Ok(())
    }

    /// Release the WM-owned X resources: the compositor, the grabs, the root
    /// event mask, the EWMH properties, the check window and the root pixmap.
    ///
    /// Takes the [`LiveX`] borrow, so this cannot be *called* with a connection
    /// that has already failed — the whole point of the split. Every request
    /// goes out over `x`, never over `self.conn`, so the request and the proof
    /// that issuing it was safe are the same object.
    ///
    /// The `flush` at the end is the one request here whose failure is reported
    /// rather than ignored: it is the request that tells the server everything
    /// above it, so an error from it means none of the rest can be relied on
    /// either. It is also the reason this half and the local half cannot share a
    /// function — on a dead connection it returns in 0 ms, and a `?` here would
    /// skip everything after it, which is the entire local half.
    fn teardown_x(&mut self, x: &LiveX<'_>) -> Result<(), Box<dyn std::error::Error>> {
        let conn = x.conn();
        let _ = conn.ungrab_key(0u8, self.root, ModMask::ANY);

        // A drag in flight holds an active pointer grab: release it so
        // `restart`/`quit` never depends on the server disconnect to free it.
        if self.drag.is_some() {
            let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
            self.drag = None;
        }

        // Restore root event mask: remove SUBSTRUCTURE_REDIRECT so that
        // the next WM doesn't fail on startup.
        let _ = conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );

        for win in self.engine.state.clients.keys() {
            let _ = conn.ungrab_button(ButtonIndex::ANY, *win, ModMask::ANY);
        }

        let _ = conn.delete_property(self.root, self.atoms.net_supporting_wm_check);
        let _ = conn.delete_property(self.root, self.atoms.net_active_window);
        let _ = conn.delete_property(self.root, self.atoms.net_client_list);
        let _ = conn.destroy_window(self.check_win);

        conn.flush()?;
        Ok(())
    }

    /// Tear down WM-owned X resources (grabs, root event mask, EWMH props,
    /// check window) and remove the control socket / identity ficha. Safe to
    /// call before `exec` in `restart`.
    ///
    /// The clean-exit spelling of [`Self::shutdown`], kept for `restart` and the
    /// fatal-event-loop arm. It is safe on a dead connection too: the X half is
    /// skipped rather than attempted, so calling it from an error path that
    /// happens to be a connection loss costs the X half and nothing else.
    pub fn cleanup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.shutdown(ShutdownReason::Clean)
    }
    fn run_once(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::io::AsRawFd;

        trace::begin_turn();
        let _turn_trace = trace::Span::new("turn");
        // SIGCONT (resume from stop) requests a key regrab; SIGTERM requests quit.
        // Both are set by the maverick-sys signal handlers (the only unsafe code).
        if maverick_sys::need_regrab() {
            maverick_sys::clear_regrab();
            self.refresh_keyboard();
        }
        if maverick_sys::quit_requested() {
            maverick_sys::clear_quit();
            self.begin_shutdown();
            return Ok(());
        }

        // Drain the deferred _NET_CLIENT_LIST update (if any manage/unmanage
        // marked it dirty) before blocking on the next event, so all X11
        // output from the previous event batch is flushed in one shot.
        let flush_trace = trace::Span::new("x_flush");
        self.flush_client_list()?;
        self.conn.flush()?;
        drop(flush_trace);
        trace!("geometry_flush_returned", "actual_visible=false");

        // Drain every X11 + control-socket event that is already queued before
        // the loop settles, so a burst is handled in one pass rather than one
        // pass per wakeup.
        while let Some(ev) = self.conn.poll_for_event()? {
            self.dispatch(ev)?;
        }

        // Settle before blocking, for the same reason the events above were
        // dispatched with geometry from the previous turn already current: a
        // hit-test on an event that arrives while we are asleep reads
        // `client.geom`, and the focus that moved the camera does not rewrite
        // it until a pass runs.
        self.flush_pending()?;

        // Snap every spring straight to its target: dwm-style, zero animation.
        // Every state change has already landed on its final geometry through
        // the single `Effect::ArrangeMonitor` -> `arrange` (Phase::Settled)
        // path, so there is nothing to interpolate and nothing to reconfigure
        // per frame. Leaving the logical state settled is what makes the
        // integer `ConfigureWindow` rect the final rect.
        self.engine.state.snap_animations();
        self.anim_per_mon.clear();
        self.animating = false;

        // Block on X11 plus the control self-pipe. With nothing animating the
        // loop is idle, so the poll has no frame deadline: no heartbeat, no
        // timer. Every other bound the loop owns lives in `wait_timeout` — never
        // sleep past a pending keyboard refresh or the shutdown deadline.
        let fd = self.conn.as_raw_fd();
        let timeout = wait_timeout(None, self.kbd_refresh_due, self.shutdown_deadline);

        if timeout != Some(std::time::Duration::ZERO) {
            let wait_trace = trace::Span::new("wait");
            let mut fds = vec![fd];
            if let Some(hub) = &self.hub {
                fds.push(hub.wake_fd());
            }
            maverick_sys::wait_readable_fds(&fds, timeout);
            drop(wait_trace);
            // Drain for anything that arrived while we were blocked.
            while let Some(ev) = self.conn.poll_for_event()? {
                self.dispatch(ev)?;
            }
        }

        // One regrab per burst of keyboard-change notifications (see
        // `schedule_keyboard_refresh`).
        if self
            .kbd_refresh_due
            .is_some_and(|due| Instant::now() >= due)
        {
            self.kbd_refresh_due = None;
            self.refresh_keyboard();
        }

        // Execute any commands from the control socket, then publish state.
        let control_trace = trace::Span::new("control");
        self.drain_control()?;
        self.publish_state();
        drop(control_trace);

        // Whatever the post-wait drain owed. The earlier call already settled
        // everything that came before the wait; this one exists so that work is
        // never carried into the next turn, where it would queue up alongside
        // that turn's own events.
        let reconcile_trace = trace::Span::new("reconcile");
        self.flush_pending()?;
        drop(reconcile_trace);

        // Loop back → flush_client_list() rewrites _NET_CLIENT_LIST at most once per batch.
        Ok(())
    }

    /// Run the layout, stacking and pointer work [`PendingReconcile`] is holding.
    ///
    /// `arrange` restacks the monitor it laid out on its way out, so a monitor
    /// owes no separate stacking pass here. The pointer warp does have to wait:
    /// it targets the geometry `arrange` just produced, not where the window was
    /// before it moved.
    fn flush_pending(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.apply_wheel_steps()?;
        self.flush_layout()?;
        // After the layout work, so the probe compares the real X focus with
        // the logical focus this turn ended on, not one it passed through.
        if std::mem::take(&mut self.focus_probe_due) {
            self.reconcile_focus()?;
        }
        Ok(())
    }

    fn flush_layout(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.pending.is_empty() {
            return Ok(());
        }
        for (mon, focus) in self.pending.take() {
            self.arrange(mon)?;
            let Some(w) = focus else { continue };
            if !self.engine.cfg.warp_cursor {
                continue;
            }
            // `arrange` above rewrote `client.geom` to the settled position, so
            // this warps onto the window we actually focused rather than
            // wherever it slid from. Clamped to i16: a >32k half-size would
            // wrap negative.
            let g = self
                .engine
                .state
                .clients
                .get(&w)
                .map_or(Rect::new(0, 0, 1, 1), |c| c.geom);
            let dx = (g.w / 2).min(i16::MAX as u32) as i16;
            let dy = (g.h / 2).min(i16::MAX as u32) as i16;
            let _ = self.conn.warp_pointer(x11rb::NONE, w, 0, 0, 0, 0, dx, dy);
        }
        Ok(())
    }
    /// Drive the WM until `state.running` is false or the X connection is lost.
    /// One iteration is `run_once`; graceful `quit` waits up to `SHUTDOWN_BUDGET`
    /// for clients, then `force_kill_remaining`.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        crate::log::config_trace(
            "event_loop_start",
            format_args!(
                "presentation=x11_settled animations_enabled={} trace={}",
                crate::config::animations_enabled(&self.engine.cfg),
                trace::enabled(),
            ),
        );
        while self.engine.state.running {
            if let Err(e) = self.run_once() {
                return if is_x11_connection_loss(&*e) {
                    log::info!("maverick: X11 connection lost (X server disconnected)");
                    Ok(())
                } else {
                    Err(e)
                };
            }
            // Graceful shutdown: once a quit was requested, keep pumping the
            // event loop (so cooperative clients can close and be unmanaged)
            // until either every client is gone OR the global budget elapses.
            // The budget is a hard upper bound — Maverick ALWAYS terminates,
            // never waiting on client cooperation.
            if let Some(deadline) = self.shutdown_deadline {
                if self.engine.state.clients.is_empty() || std::time::Instant::now() >= deadline {
                    self.force_kill_remaining();
                    self.engine.state.running = false;
                    break;
                }
            }
        }
        Ok(())
    }
    /// Open X, claim the screen (or `--replace`), detect `RandR` monitors, build
    /// `Engine`, optionally initialise the GL compositor, scan existing windows
    /// and arrange. `config_path` is the exact file to re-read on `reload`;
    /// `launch_args` are replayed verbatim on `restart`.
    pub fn new(
        cfg: Cfg,
        replace: bool,
        config_path: Option<PathBuf>,
        launch_args: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        trace::init();
        let (dpy, conn, screen_num) = maverick_x11::open_x()?;
        // `conn` is shared (via `Rc`) with the compositor so both the WM and the
        // GLX layer issue requests over the *same* `XCBConnection` — that is what
        // keeps x11rb's sequence-number/reply tracking coherent. `XDisplay` is
        // `Copy`, so `dpy` is simply handed to both.
        let conn = Rc::new(conn);
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let visual = screen.root_visual;

        log::info!(
            "maverick: X11 connected root={} {}x{}",
            root,
            screen.width_in_pixels,
            screen.height_in_pixels
        );

        let atoms = Atoms::new(&conn)?;
        if replace {
            if !claim_screen_replacing(&conn, root, &atoms)? {
                return Err(
                    "another WM is running and did not yield the screen (use --replace only when one is present)".into(),
                );
            }
            log::info!("maverick: replaced the previous WM (--replace)");
        } else {
            check_no_other_wm(&conn, root)?;
        }

        let monitors = detect_monitors(&conn, screen, &cfg)?;
        let mut engine = Engine::new(cfg);
        engine.state.monitors = monitors;
        // Apply the configured scroll-camera spring constants (compositor
        // stiffness/damping) to every workspace camera, since Monitor::new /
        // reconcile_workspaces build cameras with hard-coded defaults.
        engine.apply_camera_cfg();
        crate::log::config_snapshot("engine_config", &engine.cfg);
        if crate::log::config_trace_enabled() {
            for (monitor, mon) in engine.state.monitors.iter().enumerate() {
                for (workspace, ws) in mon.workspaces.iter().enumerate() {
                    crate::log::config_trace(
                        "camera_config_applied",
                        format_args!(
                            "monitor={monitor} workspace={workspace} camera={:?}",
                            ws.camera
                        ),
                    );
                }
            }
        }

        let check_win = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            check_win,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )?
        .check()?;

        crate::log::config_trace(
            "xkb_init_start",
            format_args!("phase=fetch_before_compositor"),
        );
        let ks = fetch_keyboard_state(&conn).inspect_err(|e| {
            crate::log::config_trace(
                "xkb_init_end",
                format_args!("phase=fetch_before_compositor status=failed error={e}"),
            );
        })?;
        crate::log::config_trace(
            "xkb_init_end",
            format_args!("phase=fetch_before_compositor status=ok"),
        );
        crate::log::config_trace(
            "keyboard_snapshot_before_compositor",
            format_args!(
                "min={} kpk={} raw={:?} xkb={} xkb_group={} numlock={:#x} scroll={:#x}",
                ks.min,
                ks.kpk,
                ks.keysyms,
                ks.xkb.is_some(),
                ks.xkb_group,
                ks.numlock,
                ks.scroll
            ),
        );
        let (raw_keymap, raw_kpk, raw_min, numlock) = (ks.keysyms, ks.kpk, ks.min, ks.numlock);
        let keymap = build_keymap(&engine.cfg);

        let mut wm = WindowManager {
            conn,
            dpy,
            screen_num,
            root,
            atoms,
            engine,
            layout_registry: crate::core::layout::LayoutRegistry::new(),
            check_win,
            numlock,
            scroll: ks.scroll,
            keymap,
            raw_keymap,
            raw_kpk,
            raw_min,
            xkb: ks.xkb,
            xkb_group: ks.xkb_group,
            last_grab_warnings: Vec::new(),
            kbd_refresh_due: None,
            drag: None,
            client_list_dirty: false,
            stack_dirty: false,
            applied: crate::backend::x11::reconciler::AppliedState::default(),
            shape_mask_cache: std::collections::HashMap::new(),
            frame_extents: std::collections::HashMap::new(),
            hide_ws_set: std::collections::HashSet::with_capacity(32),
            hide_mon_vec: Vec::with_capacity(64),
            desired: Placements::with_capacity(32),
            anim_per_mon: Vec::new(),
            present_scratch: Vec::with_capacity(32),
            ribbon_scratch: RibbonScratch::default(),
            last_key_times: std::collections::BTreeMap::new(),
            control: None,
            session_id: String::new(),
            config_path,
            launch_args,
            shutdown_deadline: None,
            hub: None,
            last_state_json: String::new(),
            docks: std::collections::HashMap::new(),
            pointer_guard_until: None,
            last_event_time: 0,
            animating: false,
            last_stack_order: std::collections::HashMap::new(),
            fs_covering: std::collections::HashMap::new(),
            pending: PendingReconcile::default(),
            protocols: std::cell::RefCell::new(std::collections::HashMap::new()),
            focus_probe_due: false,
            wheel_steps: Vec::new(),
        };

        let _ = (depth, visual);

        wm.setup_root()?;
        wm.scan_windows()?;

        for i in 0..wm.engine.state.monitors.len() {
            wm.arrange(i)?;
        }

        // Publish the initial `_NET_WORKAREA` / `_NET_DESKTOP_GEOMETRY` now
        // that monitors, workareas (including pre-existing dock struts adopted
        // by `scan_windows`) and the first arrangement are settled. Without
        // this they only appear after the first strut/RandR event, leaving
        // EWMH clients with no workarea straight after startup.
        wm.update_workarea()?;

        wm.conn.flush()?;
        log::info!("maverick ready");
        Ok(wm)
    }
}

/// Interpret a strut vector as every non-zero (edge, thickness). Both
/// `_NET_WM_STRUT` (4 values) and `_NET_WM_STRUT_PARTIAL` (12 values) start
/// with `[left, right, top, bottom]`; a single dock may reserve several edges
/// at once, so all non-zero ones are returned.
fn strut_edge(v: &[u32]) -> Option<Vec<(Edge, u32)>> {
    let (left, right, top, bottom) = (v[0], v[1], v[2], v[3]);
    let mut out: Vec<(Edge, u32)> = Vec::new();
    if top > 0 {
        out.push((Edge::Top, top));
    }
    if bottom > 0 {
        out.push((Edge::Bottom, bottom));
    }
    if left > 0 {
        out.push((Edge::Left, left));
    }
    if right > 0 {
        out.push((Edge::Right, right));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn is_x11_connection_loss(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<ConnectionError>(),
        Some(ConnectionError::IoError(_))
    )
}

fn check_no_other_wm(conn: &XConn, root: Window) -> Result<(), Box<dyn std::error::Error>> {
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
    )?
    .check()
    .map_err(|_| "another WM is already running")?;
    conn.flush()?;
    Ok(())
}

fn grab_substructure(conn: &XConn, root: Window) -> bool {
    match conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
    ) {
        Ok(cookie) => cookie.check().is_ok(),
        Err(_) => false,
    }
}

/// `--replace` handover dance (dwm-style): try to grab
/// `SUBSTRUCTURE_REDIRECT` directly; if another WM holds it, find its
/// `_NET_SUPPORTING_WM_CHECK` window (EWMH 1.4 §WM Attributes) and politely
/// send it `WM_DELETE_WINDOW`, then retry the grab until it succeeds or the
/// timeout expires. The previous WM is never `SIGKILL`ed — it takes whatever
/// path its own `WM_DELETE` handler chooses, which is always a clean exit for
/// real WMs.
fn claim_screen_replacing(
    conn: &XConn,
    root: Window,
    atoms: &Atoms,
) -> Result<bool, Box<dyn std::error::Error>> {
    use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent};

    if grab_substructure(conn, root) {
        return Ok(true);
    }
    log::info!("another WM owns the screen; asking it to leave");
    const ATTEMPTS: usize = 20;
    const SLEEP_MS: u64 = 150;
    for _ in 0..ATTEMPTS {
        if let Ok(cookie) = conn.get_property(
            false,
            root,
            atoms.net_supporting_wm_check,
            AtomEnum::WINDOW,
            0,
            1,
        ) {
            if let Ok(reply) = cookie.reply() {
                if let Some(win) = reply.value32().and_then(|mut v| v.next()) {
                    if win != 0 && win != x11rb::NONE {
                        let ev = ClientMessageEvent {
                            response_type: CLIENT_MESSAGE_EVENT,
                            format: 32,
                            sequence: 0,
                            window: win,
                            type_: atoms.wm_protocols,
                            data: ClientMessageData::from([
                                atoms.wm_delete_window,
                                x11rb::CURRENT_TIME,
                                0,
                                0,
                                0,
                            ]),
                        };
                        let _ = conn.send_event(false, win, EventMask::NO_EVENT, ev);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
        if grab_substructure(conn, root) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn detect_monitors(
    conn: &XConn,
    screen: &Screen,
    cfg: &Cfg,
) -> Result<Vec<Monitor>, Box<dyn std::error::Error>> {
    let nt = cfg.n_tags;

    if let Ok(reply) = conn.randr_get_monitors(screen.root, true)?.reply() {
        if !reply.monitors.is_empty() {
            return Ok(reply
                .monitors
                .iter()
                .map(|m| {
                    let r = Rect::new(m.x as i32, m.y as i32, m.width as u32, m.height as u32);
                    Monitor::new(r, nt)
                })
                .collect());
        }
    }
    let r = Rect::new(
        0,
        0,
        screen.width_in_pixels as u32,
        screen.height_in_pixels as u32,
    );
    Ok(vec![Monitor::new(r, nt)])
}

fn build_keymap(cfg: &Cfg) -> BTreeMap<(u16, u32), Action> {
    let mut map = BTreeMap::new();
    for (m, k, a) in &cfg.keybinds {
        // Index by the *normalised* keysym: `on_key` normalises what it reads
        // from the keymap, so a bind written as the raw escape `0x41` (`A`) has
        // to be stored under `0x61` (`a`) or it could never be matched. The grab
        // side still searches for the raw keysym — `0x41` genuinely lives in
        // column 1 of the `a` keycode — so both halves agree.
        //
        // First wins: a later duplicate `(mods, keysym)` does not overwrite the
        // earlier one. Mirrors the conflict policy in `parse_keybindings`.
        map.entry((*m, normalize_ksym(*k)))
            .or_insert_with(|| a.clone());
    }
    map
}

/// Result of a pipelined keyboard+modifier state fetch.
struct KeyboardState {
    keysyms: Vec<u32>,
    kpk: usize,
    min: u8,
    numlock: u16,
    /// Modifier-map column that carries Scroll Lock (0 when unmapped). Treated
    /// as an ignored lock like Caps/Num: grabs get `| scroll` variants and
    /// `clean_mask` strips it, so binds fire with the lock on or off.
    scroll: u16,
    /// Complete XKB key types and symbols used by the unified resolver.
    xkb: Option<XkbKeyboardMap>,
    /// Effective XKB group captured with `xkb`.
    xkb_group: u8,
}

/// Apply the XKB out-of-range group policy for one key row.
fn effective_group(row: &KeySymMap, requested: u8) -> u8 {
    let groups = row.group_info & 0x0f;
    if requested < groups {
        return requested;
    }
    match row.group_info & 0xc0 {
        0x40 => groups - 1,
        0x80 => {
            let target = (row.group_info >> 4) & 0x3;
            if target < groups {
                target
            } else {
                0
            }
        }
        _ => requested % groups,
    }
}

/// Pipelined keyboard+modifier state: fire both requests, then collect both
/// replies, so the two round trips collapse into one.
fn fetch_keyboard_state(conn: &XConn) -> Result<KeyboardState, Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    // `count` is a `u8` (CARD8): `max - min + 1` can only exceed 255 with a
    // hostile/broken Setup (`min=0, max=255` → 256 → truncates to 0 and the
    // mapping comes back empty, silently dropping all binds). Reject an
    // inverted range and clamp the count instead of wrapping.
    if max < min {
        return Err("keyboard: inverted keycode range in Setup".into());
    }
    let count = (max as u16 - min as u16 + 1).min(u8::MAX as u16) as u8;
    if count == 0 {
        return Err("keyboard: empty keycode range in Setup".into());
    }

    let c_kb = conn.get_keyboard_mapping(min, count)?;
    let c_mod = conn.get_modifier_mapping()?;

    let map = c_kb.reply()?;
    let kpk = map.keysyms_per_keycode as usize;
    let keysyms = map.keysyms.clone();

    let (numlock, scroll) = if let Ok(modmap) = c_mod.reply() {
        let kpm = modmap.keycodes_per_modifier() as usize;
        (
            compute_numlock(&modmap.keycodes, kpm, &keysyms, kpk, min, max),
            compute_scroll(&modmap.keycodes, kpm, &keysyms, kpk, min, max),
        )
    } else {
        (0, 0)
    };

    use x11rb::protocol::xkb::{ConnectionExt as _, ID};
    let xkb_supported = match conn.xkb_use_extension(1, 0) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply.supported,
            Err(ReplyError::ConnectionError(ConnectionError::UnsupportedExtension)) => false,
            Err(error) => return Err(error.into()),
        },
        Err(ConnectionError::UnsupportedExtension) => false,
        Err(error) => return Err(error.into()),
    };
    let (xkb, xkb_group) = if xkb_supported {
        let map = conn
            .xkb_get_map(
                ID::USE_CORE_KBD.into(),
                MapPart::KEY_TYPES | MapPart::KEY_SYMS,
                0u16.into(),
                0,
                0,
                min,
                count,
                0,
                0,
                0,
                0,
                0u16.into(),
                0,
                0,
                0,
                0,
                0,
                0,
            )?
            .reply()?;
        let xkb = XkbKeyboardMap::from_reply(&map)
            .ok_or("keyboard: XKB reply omitted key types or key symbols")?;
        let group = conn
            .xkb_get_state(ID::USE_CORE_KBD.into())?
            .reply()?
            .group
            .into();
        (Some(xkb), group)
    } else {
        (None, 0)
    };
    Ok(KeyboardState {
        keysyms,
        kpk,
        min,
        numlock,
        scroll,
        xkb,
        xkb_group,
    })
}

/// Search for `NumLock` keysym in the modifier mapping.
fn compute_numlock(
    keycodes: &[u8],
    kpm: usize,
    keysyms: &[u32],
    kpk: usize,
    min: u8,
    max: u8,
) -> u16 {
    if kpk == 0 || kpm == 0 {
        return 0;
    }
    const XK_NUM_LOCK: u32 = 0xff7f;
    modifier_column(keycodes, kpm, keysyms, kpk, min, max, XK_NUM_LOCK)
}

/// Search for `Scroll Lock` keysym in the modifier mapping (0 when unmapped).
fn compute_scroll(
    keycodes: &[u8],
    kpm: usize,
    keysyms: &[u32],
    kpk: usize,
    min: u8,
    max: u8,
) -> u16 {
    if kpk == 0 || kpm == 0 {
        return 0;
    }
    const XK_SCROLL_LOCK: u32 = 0xff14;
    modifier_column(keycodes, kpm, keysyms, kpk, min, max, XK_SCROLL_LOCK)
}

/// Modifier-map column whose keycodes carry `keysym` (0 when unmapped). Shared
/// by the `NumLock` and `Scroll Lock` detectors: the column, not the keysym, is
/// what a lock mask looks like in event state.
fn modifier_column(
    keycodes: &[u8],
    kpm: usize,
    keysyms: &[u32],
    kpk: usize,
    min: u8,
    max: u8,
    keysym_wanted: u32,
) -> u16 {
    if kpk == 0 || kpm == 0 {
        return 0;
    }
    for (i, codes) in keycodes.chunks(kpm).enumerate() {
        for &code in codes {
            if code == 0 || code < min || code > max {
                continue;
            }
            let idx = (code - min) as usize * kpk;
            if (0..kpk).any(|j| keysyms[idx + j] == keysym_wanted) {
                return 1 << i;
            }
        }
    }
    0
}

/// Keycode of the `i`-th keymap row, or `None` when it falls outside the
/// protocol's 8-bit keycode space (which `Setup.min_keycode/max_keycode`
/// guarantees it won't, but the arithmetic must not be able to wrap).
#[inline]
fn row_keycode(min: u8, i: usize) -> Option<u8> {
    u8::try_from(usize::from(min) + i).ok()
}

/// Core modifier state used for XKB lookup. Group bits are kept separate
/// because `XGrabKey` cannot express a group in its modifier mask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct KeyLookupState {
    core: u16,
    group: u8,
}

impl KeyLookupState {
    /// `XGrabKey` delivery can omit group bits even though XKB's effective group
    /// is non-zero. `cached_group` comes from the existing `GROUP_STATE`
    /// notification/reread path and is the same policy used before unification.
    fn from_x11_with_group(state: u16, cached_group: u8) -> Self {
        let event_group = ((state >> 13) & 0x3) as u8;
        Self {
            core: state & 0xff,
            group: if state & 0x6000 == 0 {
                cached_group
            } else {
                event_group
            },
        }
    }
}

/// One authoritative XKB key translation. Planner and dispatch both
/// construct this value through the same resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedKey {
    keycode: u8,
    group: u8,
    level: u8,
    keysym: u32,
    /// Exact core modifier state inside the active key type that selected
    /// the level. This is also the selector portion required by a grab.
    required_core: u16,
    /// True when the selected core state matched an active XKB level rule.
    /// A no-rule fallback resolves level 0 for dispatch safety, but the
    /// planner only accepts that implicit default when no selector is active.
    rule_matched: bool,
}

impl ResolvedKey {
    fn planner_valid(self) -> bool {
        self.rule_matched || self.required_core == 0
    }
}

/// Immutable XKB map assembled from `XkbGetMap(KEY_TYPES | KEY_SYMS)`.
/// The core mapping remains a separate fallback only when XKB is absent.
#[derive(Debug, Clone, Default)]
struct XkbKeyboardMap {
    first_type: u8,
    first_key_sym: u8,
    types: Vec<KeyType>,
    keys: Vec<KeySymMap>,
}

impl XkbKeyboardMap {
    fn from_reply(reply: &xkb::GetMapReply) -> Option<Self> {
        Some(Self {
            first_type: reply.first_type,
            first_key_sym: reply.first_key_sym,
            types: reply.map.types_rtrn.as_ref()?.clone(),
            keys: reply.map.syms_rtrn.as_ref()?.clone(),
        })
    }

    fn row(&self, keycode: u8) -> Option<&KeySymMap> {
        if keycode < self.first_key_sym {
            return None;
        }
        self.keys.get(usize::from(keycode - self.first_key_sym))
    }

    fn parts(&self, keycode: u8, requested_group: u8) -> Option<(&KeySymMap, &KeyType, u8)> {
        let row = self.row(keycode)?;
        let groups = row.group_info & 0x0f;
        if groups == 0 || row.width == 0 {
            return None;
        }

        let group = effective_group(row, requested_group);
        let group_index = usize::from(group);
        let type_index = usize::from(
            row.kt_index
                .get(group_index)?
                .checked_sub(self.first_type)?,
        );
        let key_type = self.types.get(type_index)?;
        Some((row, key_type, group))
    }

    fn resolve(&self, keycode: u8, state: KeyLookupState) -> Option<ResolvedKey> {
        let (row, key_type, group) = self.parts(keycode, state.group)?;
        let group_index = usize::from(group);
        let selected = state.core & u16::from(key_type.mods_mask);
        let rule = key_type
            .map
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.active && u16::from(entry.mods_mask) == selected);
        let level = rule.map_or(0, |(_, entry)| usize::from(entry.level));
        if level >= usize::from(key_type.num_levels) || level >= usize::from(row.width) {
            return None;
        }
        let symbol_index = group_index
            .checked_mul(usize::from(row.width))?
            .checked_add(level)?;
        let keysym = row.syms.get(symbol_index).copied().unwrap_or(0);

        Some(ResolvedKey {
            keycode,
            group,
            level: level as u8,
            keysym,
            required_core: selected,
            rule_matched: rule.is_some(),
        })
    }
}

/// What `grab_keys` should ask the server for, computed without touching X so
/// it can be unit-tested against synthetic keymaps.
#[derive(Debug, Default)]
struct KeyGrabPlan {
    /// `(raw XGrabKey modifier mask, config keysym, keycode)` triples,
    /// deduplicated by `(mask, keycode)`.
    grabs: Vec<(u16, u32, u8)>,
    /// Binds whose keysym cannot be produced by the active XKB group.
    missing: Vec<(u16, u32)>,
}

/// Borrowed view of the authoritative keyboard map plus lock columns.
#[derive(Debug, Default, Clone, Copy)]
struct ActiveLayout<'a> {
    keysyms: &'a [u32],
    min: u8,
    kpk: usize,
    xkb: Option<&'a XkbKeyboardMap>,
    group: u8,
    numlock: u16,
    scroll: u16,
}

impl ActiveLayout<'_> {
    /// The single translation path shared by planning and dispatch.
    fn resolve_key(&self, keycode: u8, state: KeyLookupState) -> Option<ResolvedKey> {
        if let Some(xkb) = self.xkb {
            return xkb.resolve(keycode, state);
        }

        let level = dispatch_col(
            state.core & u16::from(ModMask::SHIFT) != 0,
            state.core & u16::from(ModMask::LOCK) != 0,
            self.kpk,
        );
        let keysym = input::keysym_at_col(self.keysyms, self.min, self.kpk, keycode, level);
        (keysym != 0).then_some(ResolvedKey {
            keycode,
            group: 0,
            level: level as u8,
            keysym,
            required_core: if level == 1 {
                u16::from(ModMask::SHIFT)
            } else {
                0
            },
            rule_matched: true,
        })
    }

    fn xkb_parts(&self, keycode: u8) -> Option<(&KeySymMap, &KeyType, u8)> {
        self.xkb?.parts(keycode, self.group)
    }

    /// Resolve a configured keysym to the least-constrained XKB/core level
    /// that actually contains it. Planner and dispatch both call this before
    /// `resolve_key`; it never contains a second level-selection algorithm.
    fn resolve_configured_key(&self, keycode: u8, keysym: u32) -> Option<ResolvedKey> {
        let wanted = normalize_ksym(keysym);
        if wanted == 0 {
            return None;
        }

        if let Some((_, key_type, group)) = self.xkb_parts(keycode) {
            let type_mask = u16::from(key_type.mods_mask);
            let mut normalized_fallback = None;
            for selected in mask_submasks(type_mask) {
                let candidate = self.resolve_key(
                    keycode,
                    KeyLookupState {
                        core: selected,
                        group,
                    },
                );
                if candidate.is_none_or(|resolved| {
                    !resolved.planner_valid() || normalize_ksym(resolved.keysym) != wanted
                }) {
                    continue;
                }
                if candidate.is_some_and(|resolved| resolved.keysym == keysym) {
                    return candidate;
                }
                normalized_fallback.get_or_insert(candidate);
            }
            return normalized_fallback.flatten();
        }

        if self.kpk == 0 {
            return None;
        }
        let level0 = input::keysym_at_col(self.keysyms, self.min, self.kpk, keycode, 0);
        let level1 = input::keysym_at_col(self.keysyms, self.min, self.kpk, keycode, 1);
        let level = if level0 == keysym {
            0
        } else if level1 == keysym {
            1
        } else if normalize_ksym(level0) == wanted {
            0
        } else if normalize_ksym(level1) == wanted {
            1
        } else {
            return None;
        };
        self.resolve_key(
            keycode,
            KeyLookupState {
                core: if level == 1 {
                    u16::from(ModMask::SHIFT)
                } else {
                    0
                },
                group: 0,
            },
        )
    }

    /// Return the exact raw grab masks that can represent this binding.
    /// Every candidate is validated through `resolve_key`; no second
    /// level-selection rule lives in the planner.
    fn grab_masks_for_binding(&self, keycode: u8, bind_mask: u16, keysym: u32) -> Vec<u16> {
        let Some(candidate) = self.resolve_configured_key(keycode, keysym) else {
            return Vec::new();
        };
        let wanted = normalize_ksym(keysym);
        let group = candidate.group;
        let base = bind_mask | candidate.required_core;
        let mut masks = Vec::new();
        for variant in mod_variants(self.numlock, self.scroll) {
            let raw_mask = base | variant;
            let Some(resolved) = self.resolve_key(
                keycode,
                KeyLookupState {
                    core: raw_mask & 0xff,
                    group,
                },
            ) else {
                continue;
            };
            let effective_match =
                resolved.planner_valid() && normalize_ksym(resolved.keysym) == wanted;
            let ignored_locks = u16::from(ModMask::LOCK) | self.scroll;
            let explicit_extras =
                resolved.required_core & !candidate.required_core & !ignored_locks;
            let binding_only_match =
                candidate.required_core == 0 && explicit_extras & !bind_mask == 0;
            if (effective_match || binding_only_match) && !masks.contains(&raw_mask) {
                masks.push(raw_mask);
            }
        }
        masks
    }
}

fn mask_submasks(mask: u16) -> Vec<u16> {
    let mut submasks = Vec::with_capacity(1usize << mask.count_ones().min(8));
    let mut submask = mask;
    loop {
        submasks.push(submask);
        if submask == 0 {
            break;
        }
        submask = (submask - 1) & mask;
    }
    submasks.reverse();
    submasks
}

/// Resolve configured bindings into raw passive-grab masks for the active
/// XKB group. XKB and core fallback both call `ActiveLayout::resolve_key`.
fn plan_key_grabs(binds: &[(u16, u32)], layout: ActiveLayout<'_>) -> KeyGrabPlan {
    let mut plan = KeyGrabPlan::default();
    let mut seen = BTreeSet::new();

    for &(bind_mask, keysym) in binds {
        let mut found = false;
        let keycodes: Vec<u8> = if let Some(xkb) = layout.xkb {
            (0..xkb.keys.len())
                .filter_map(|index| xkb.first_key_sym.checked_add(index as u8))
                .collect()
        } else {
            let rows = layout.keysyms.len().checked_div(layout.kpk).unwrap_or(0);
            (0..rows)
                .filter_map(|index| row_keycode(layout.min, index))
                .collect()
        };

        for keycode in keycodes {
            for raw_mask in layout.grab_masks_for_binding(keycode, bind_mask, keysym) {
                found = true;
                if seen.insert((raw_mask, keycode)) {
                    plan.grabs.push((raw_mask, keysym, keycode));
                }
            }
        }
        if !found {
            plan.missing.push((bind_mask, keysym));
        }
    }
    plan
}

/// Core fallback level selection. XKB never calls this path; it remains only
/// for servers where the XKB extension is unavailable.
#[inline]
fn dispatch_col(shift: bool, lock: bool, kpk: usize) -> usize {
    usize::from(shift ^ lock).min(1).min(kpk.saturating_sub(1))
}

/// Match a resolved key against the logical binding map. The exact observed
/// modifier mask wins. Otherwise the binding is the inverse of planner's rule:
/// `binding_mask | required_level_mask == observed_state`.
fn resolve_binding(
    keymap: &BTreeMap<(u16, u32), Action>,
    layout: ActiveLayout<'_>,
    resolved: ResolvedKey,
    mods: u16,
) -> Option<((u16, u32), Action)> {
    let layout = ActiveLayout {
        group: resolved.group,
        ..layout
    };
    let keysym = normalize_ksym(resolved.keysym);
    if keysym == 0 {
        return None;
    }
    if let Some(action) = keymap.get(&(mods, keysym)) {
        crate::log::config_trace(
            "key_binding",
            format_args!(
                "keycode={} group={} level={} state={:#x} required={:#x} mods={:#x} selector=explicit resolved={:#x} action={action:?} outcome=matched",
                resolved.keycode,
                resolved.group,
                resolved.level,
                mods,
                resolved.required_core,
                mods,
                keysym,
            ),
        );
        return Some(((mods, keysym), action.clone()));
    }

    let observed = mods | resolved.required_core;
    let mut fallback: Option<(u16, Action)> = None;
    for (&(binding_mods, candidate_keysym), action) in keymap {
        if candidate_keysym != keysym || binding_mods | resolved.required_core != observed {
            continue;
        }
        if fallback
            .as_ref()
            .is_none_or(|(best, _)| binding_mods.count_ones() > best.count_ones())
        {
            fallback = Some((binding_mods, action.clone()));
        }
    }
    if let Some((binding_mods, action)) = fallback {
        crate::log::config_trace(
            "key_binding",
            format_args!(
                "keycode={} group={} level={} state={:#x} required={:#x} mods={:#x} selector=level_implicit resolved={:#x} action={action:?} outcome=matched",
                resolved.keycode,
                resolved.group,
                resolved.level,
                mods,
                resolved.required_core,
                binding_mods,
                keysym,
            ),
        );
        return Some(((binding_mods, keysym), action));
    }

    let mut configured: Option<(u16, u32, Action)> = None;
    for (&(binding_mods, candidate_keysym), action) in keymap {
        if binding_mods & !mods != 0 {
            continue;
        }
        let Some(candidate) = layout.resolve_configured_key(resolved.keycode, candidate_keysym)
        else {
            continue;
        };
        if (binding_mods | candidate.required_core) & !mods != 0 {
            continue;
        }
        if configured
            .as_ref()
            .is_none_or(|(best, _, _)| binding_mods.count_ones() > best.count_ones())
        {
            configured = Some((binding_mods, candidate_keysym, action.clone()));
        }
    }
    if let Some((binding_mods, candidate_keysym, action)) = configured {
        crate::log::config_trace(
            "key_binding",
            format_args!(
                "keycode={} group={} level={} state={:#x} required={:#x} mods={:#x} selector=configured_key resolved={:#x} action={action:?} outcome=matched",
                resolved.keycode,
                resolved.group,
                resolved.level,
                mods,
                resolved.required_core,
                binding_mods,
                candidate_keysym,
            ),
        );
        return Some(((binding_mods, candidate_keysym), action));
    }

    crate::log::config_trace(
        "key_binding",
        format_args!(
            "keycode={} group={} level={} state={:#x} required={:#x} mods={:#x} resolved={:#x} outcome=unmatched",
            resolved.keycode,
            resolved.group,
            resolved.level,
            mods,
            resolved.required_core,
            mods,
            keysym,
        ),
    );
    None
}

/// Human-readable name of a bind, for diagnostics (`Super+Shift+k`).
fn bind_name(mask: u16, keysym: u32) -> String {
    let mods = crate::userconfig::mods_name(mask);
    let key = crate::userconfig::keysym_name(keysym);
    if mods.is_empty() {
        key
    } else {
        format!("{mods}+{key}")
    }
}

/// Short rendering of a checked request's failure. `ReplyError`'s `Display`
/// dumps the whole `X11Error` struct (sequence numbers, opcodes, `bad_value`),
/// which buries the one word that matters — `Access`, `Value`, `Window` — in a
/// line of noise.
fn x_error_kind(e: &x11rb::errors::ReplyError) -> String {
    match e {
        x11rb::errors::ReplyError::X11Error(err) => format!("{:?}", err.error_kind),
        x11rb::errors::ReplyError::ConnectionError(err) => err.to_string(),
    }
}

/// Read a window title without needing a mutable Client reference.
/// Both `net_wm_name` and `WM_NAME` requests are fired before any reply is
/// read, so the two round trips collapse into one.
fn read_title_value(
    conn: &XConn,
    win: Window,
    atoms: &Atoms,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let c_net = conn.get_property(false, win, atoms.net_wm_name, atoms.utf8_string, 0, 256);
    let c_wm = conn.get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256);

    if let Ok(c) = c_net {
        if let Ok(ref prop) = c.reply() {
            if !prop.value.is_empty() {
                return Ok(Some(String::from_utf8_lossy(&prop.value).into_owned()));
            }
        }
    }
    if let Ok(c) = c_wm {
        if let Ok(ref prop) = c.reply() {
            return Ok(Some(String::from_utf8_lossy(&prop.value).into_owned()));
        }
    }
    Ok(None)
}

type WmHints = (bool, bool, bool); // no_focus, wants_input, urgent

/// Read `WM_HINTS` flags without needing a mutable Client reference.
fn read_wm_hints_value(
    conn: &XConn,
    win: Window,
) -> Result<Option<WmHints>, Box<dyn std::error::Error>> {
    if let Ok(c) = conn.get_property(false, win, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9) {
        if let Ok(ref prop) = c.reply() {
            if let Some(vals) = prop.value32() {
                let v: Vec<u32> = vals.collect();
                if !v.is_empty() {
                    let no_focus = v[0] & 1 != 0 && v.len() > 1 && v[1] == 0;
                    let wants_input = if v[0] & 1 != 0 && v.len() > 1 {
                        v[1] != 0
                    } else {
                        true
                    };
                    let urgent = v[0] & 256 != 0;
                    return Ok(Some((no_focus, wants_input, urgent)));
                }
            }
        }
    }
    Ok(None)
}

#[inline]
fn mod_variants(numlock: u16, scroll: u16) -> [u16; 8] {
    let lock = u16::from(ModMask::LOCK);
    [
        0,
        numlock,
        lock,
        scroll,
        numlock | lock,
        numlock | scroll,
        lock | scroll,
        numlock | lock | scroll,
    ]
}

#[inline]
fn normalize_ksym(k: u32) -> u32 {
    if (0x41..=0x5a).contains(&k) {
        k + 0x20
    } else {
        k
    }
}

#[inline]
fn clean_mask(state: u16, numlock: u16, scroll: u16) -> u16 {
    let lock: u16 = ModMask::LOCK.into();
    // The Scroll Lock column arrives from the modifier map; when the key is
    // unmapped this is simply 0 and behaves exactly like before.
    state
        & !(numlock | lock | scroll)
        & (u16::from(ModMask::SHIFT)
            | u16::from(ModMask::CONTROL)
            | u16::from(ModMask::M1)
            | u16::from(ModMask::M2)
            | u16::from(ModMask::M3)
            | u16::from(ModMask::M4)
            | u16::from(ModMask::M5))
}
