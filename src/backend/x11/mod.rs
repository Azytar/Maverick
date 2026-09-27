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
//! - **`rootwall`** applies the wallpaper as a root pixmap when the
//!   compositor is disabled.
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
use crate::backend::x11::compositor::DirtyReason;
use crate::backend::x11::framesched::FrameScheduler;
use crate::config::Cfg;
use crate::core::layout::{
    arrange, fixed_size_hints, ideal_scroll, parse_wm_normal_hints, snap_float_to_hints,
    Placements, RibbonScratch,
};
use crate::core::{parse_action, state_json, Effect, Engine};
use crate::log;
use crate::types::*;

mod actions;
mod compositor;
mod events;
mod ewmh;
mod framesched;
mod hubevents;
mod input;
mod manage;
mod pointer;
pub(crate) mod reconciler;
mod render;
mod rootwall;
mod struts;
mod trace;
use trace::trace;
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
    /// No-compositor wallpaper (`rootwall.rs`): the pixmap ID last installed as
    /// the root background, if any. `apply_root_wallpaper` runs repeatedly
    /// (startup, config reload, monitor reconfiguration, GL-failure fallback)
    /// and each run allocates a fresh root-sized pixmap, so the previous one is
    /// freed as soon as the root stops pointing at it — only our own creation
    /// can still reference it by then. Without that, every reload or `RandR`
    /// event would leak a full-screen (`root_w`*`root_h`*4 byte) pixmap in the
    /// X server for the rest of the session.
    last_root_pixmap: Option<Pixmap>,
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
    /// Timestamp of the previous animation frame, for `dt` in `tick_animations`.
    last_frame: Instant,
    /// True while any camera/zoom/accordion spring is still moving; drives the
    /// frame-clock timeout (high rate while animating, idle indefinite otherwise).
    animating: bool,
    /// Nominal fallback period when no presentation-completion feedback is
    /// available. It is a rate limit, not a replacement for GLX vsync.
    frame_period: std::time::Duration,
    /// Deadline for the next continuous animation frame, when pacing is needed.
    animation_due: Option<Instant>,
    /// Per-monitor cached stacking order (top-to-bottom) so `stack_overlay`
    /// only re-issues `raise()` when the order actually changed, instead of
    /// re-raising every float/popup on every animation frame.
    last_stack_order: std::collections::HashMap<usize, Vec<WindowId>>,
    /// Per-monitor record of which fullscreen window was "covering" (raised
    /// above the dock) on the previous frame, so the dock is only re-raised on
    /// the covering→not-covering transition. Re-raising it every frame would
    /// push floats below the bar.
    fs_covering: std::collections::HashMap<usize, Option<WindowId>>,
    /// The OpenGL/GLX compositor, if enabled and a GL driver was available at
    /// startup. While `Some`, every animation frame is drawn here (GPU
    /// transforms + vsync) instead of re-`ConfigureWindow`ing each window. Falls
    /// back to `None` (the classic X11 path) on `MAVERICK_NO_COMPOSITOR`, a
    /// missing driver, or a runtime GL error.
    compositor: Option<compositor::Compositor>,
}

/// Render a frame's dirty reasons as one human-readable phrase.
///
/// Built as a single string rather than collecting the reasons and joining
/// them: this is called on the compositor's per-turn path, and the
/// intermediate `Vec` was a second allocation to produce a message most runs
/// never print. The reason list is also what a user is asked to paste into a
/// bug report, so it stays in the same order the scheduler reports.
fn describe_reasons(sched: &framesched::FrameScheduler) -> String {
    let mut out = String::new();
    for reason in sched.reasons().map(framesched::FrameReason::as_str) {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(reason);
    }
    out
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
            Event::CreateNotify(e) => self.on_create_notify(e)?,
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
                            "keycode={} state={:#06x} group={} compositor_actual={} event={e:?}",
                            e.detail,
                            u16::from(e.state),
                            (u16::from(e.state) >> 13) & 3,
                            self.compositor.is_some()
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
            Event::MapNotify(e) => self.on_map_notify(e)?,
            Event::MapRequest(e) => self.on_map_request(e)?,
            Event::MotionNotify(e) => self.on_motion(e)?,
            Event::PropertyNotify(e) => self.on_property(e)?,
            Event::UnmapNotify(e) => self.on_unmap(e)?,
            #[cfg(feature = "compositor-opengl")]
            Event::DamageNotify(e) => self.on_damage_notify(e)?,
            #[cfg(feature = "compositor-opengl")]
            Event::XfixesSelectionNotify(e) => self.on_xfixes_selection_notify(e)?,
            #[cfg(feature = "compositor-opengl")]
            Event::ShapeNotify(e) => self.on_shape_notify(e)?,
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
            if let Err(e) = self.grab_buttons(win, false) {
                log::debug!("keyboard refresh: regrabbing buttons on {win} failed ({e})");
            }
        }
    }
    /// Tear down WM-owned X resources (grabs, root event mask, EWMH props,
    /// check window) and remove the control socket / identity ficha. Safe to
    /// call before `exec` in `restart`.
    pub fn cleanup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(mut compositor) = self.compositor.take() {
            compositor.disable();
        }
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        // A drag in flight holds an active pointer grab: release it so
        // `restart`/`quit` never depends on the server disconnect to free it.
        if self.drag.is_some() {
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
            self.drag = None;
        }

        // Restore root event mask: remove SUBSTRUCTURE_REDIRECT so that
        // the next WM doesn't fail on startup.
        let _ = self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );

        for win in self.engine.state.clients.keys() {
            let _ = self
                .conn
                .ungrab_button(ButtonIndex::ANY, *win, ModMask::ANY);
        }

        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_supporting_wm_check);
        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_active_window);
        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_client_list);
        let _ = self.conn.destroy_window(self.check_win);

        // The last root pixmap has no successor that would release it, so it is
        // freed here rather than on the next install.
        if let Some(pm) = self.last_root_pixmap.take() {
            let _ = self.conn.free_pixmap(pm);
        }

        self.conn.flush()?;

        // Tear down the control socket + identity ficha so external tools stop
        // listing this (now dead) instance. The ControlServer thread stops when
        // its handle is dropped at the end of the process; explicitly remove the
        // on-disk meta here.
        if !self.session_id.is_empty() {
            maverick_sys::identity::cleanup_meta(&self.session_id);
        }
        drop(self.control.take());
        trace::dump();
        Ok(())
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
        trace!(
            "geometry_flush_returned",
            "gl_active={} actual_visible=false",
            self.compositor.is_some()
        );

        // Drain X11 + control-socket events *before* deciding the frame: a
        // freshly arrived DamageNotify/ConfigureNotify must feed this turn's
        // `FrameScheduler`, not the one after the present, or the frame is
        // composed from a refresh of stale input state.
        while let Some(ev) = self.conn.poll_for_event()? {
            self.dispatch(ev)?;
        }

        // Advance camera (and accordion/zoom) springs. While anything is still
        // moving we use a refresh-derived deadline; once the scene settles the
        // loop parks on X11 plus the control self-pipe. Presentation and
        // animated-wallpaper state are included here so their dt uses the same
        // active-clock policy as the camera.
        let compositor_animating = self
            .compositor
            .as_ref()
            .is_some_and(|c| c.presentation_animating() || c.wallpaper_animating());
        let was_animating = self.animating || compositor_animating;
        let now = Instant::now();
        // `dt` is the time since the previous turn's animation phase. Because
        // the loop presents at most once per turn, that span *is* the
        // present-to-present interval — including the time `glXSwapBuffers`
        // spends blocked on the retrace, which is most of the frame and must be
        // the integrated elapsed time (seed one refresh on the idle→animating
        // edge so a scroll does not jump by the whole idle gap; bound only
        // pathological multi-second catch-up while active) lives in
        // `framesched::clamp_frame_dt` so it is unit-testable.
        let raw_dt = (now - self.last_frame).as_secs_f32();
        let dt = crate::backend::x11::framesched::clamp_frame_dt(raw_dt, was_animating);
        self.last_frame = now;
        trace!(
            "wm_dt",
            "raw_s={raw_dt} clamped_s={dt} was_animating={was_animating}"
        );

        // Single authoritative frame scheduler for this turn. Built once from
        // the animation flag (set by the tick below) and the dirty reasons
        // accumulated since the last present. Both the render gate and the wait
        // timeout read this one object, so no subsystem can request a redundant
        // render and multiple reasons (Damage×N, Geometry, Animation, …) coalesce
        // into a single pending frame.
        let mut sched;

        if let Some(comp) = self.compositor.as_mut() {
            // Composition policy (per-output fullscreen bypass). Pure decision
            // (see `crate::compositor_policy`): for each monitor, engage bypass on
            // the single eligible fullscreen window, or disengage it.
            // `engage_bypass`/`disengage_bypass` are no-ops when the mode is
            // unchanged, so re-evaluating every turn is stable and free of cycles.
            // Bypass never touches VSync — it only removes Maverick's redirection
            // of that one window.
            if self.engine.cfg.compositor.fullscreen_bypass {
                let nmon = self.engine.state.monitors.len();
                for i in 0..nmon {
                    // The policy is the single source of truth: it returns the
                    // mode for this output. When it says `Bypass` we resolve the
                    // concrete candidate window; otherwise we disengage.
                    let win = if crate::compositor_policy::mode_for(
                        &self.engine.cfg,
                        &self.engine.state,
                        i,
                    ) == crate::compositor_policy::CompositionMode::Bypass
                    {
                        crate::compositor_policy::bypass_candidate(
                            &self.engine.cfg,
                            &self.engine.state,
                            i,
                        )
                    } else {
                        None
                    };
                    match win {
                        Some(w) => comp.engage_bypass(i, w),
                        None => comp.disengage_bypass(i),
                    }
                }
            } else {
                comp.disengage_all_bypass();
            }
            // Compositor path: the camera samples an analytical damped
            // transition for each elapsed slice. The live layout reads the
            // animated camera value and is drawn by the GPU. Substeps remain a
            // defensive bound for the remaining exponential presentation
            // springs, but the camera trajectory is not Euler/FPS-dependent.
            // Swap interval 1 (set at init) paces the present from inside
            // `end_frame`, so there is no explicit vblank wait here — the flip
            // is scheduled by the server for the next retrace. The WM's settled
            // geometry was already written by whichever action triggered the
            // change, so no per-frame `ConfigureWindow` storm.
            let nmon = self.engine.state.monitors.len();
            if self.anim_per_mon.len() != nmon {
                self.anim_per_mon = vec![false; nmon];
            }
            let anim_enabled = crate::config::animations_enabled(&self.engine.cfg);
            let mut anim = false;
            if anim_enabled {
                for sub in compositor::substep_bounds(dt) {
                    anim |= self
                        .engine
                        .state
                        .tick_animations_multi(sub, &mut self.anim_per_mon);
                }
            } else {
                self.engine.state.snap_animations();
                self.anim_per_mon.fill(false);
            }
            self.animating = anim;
            // If the last animated tick snapped to its endpoint, the next
            // scheduler would otherwise see no reason to render and the GPU
            // could retain the previous (up to 0.5 px) transform indefinitely.
            // Queue one compositor frame that installs the exact endpoint.
            if framesched::needs_endpoint_frame(was_animating, anim) {
                comp.invalidate();
            }
            // Diagnostic snapshot: the compositor trace deliberately records the
            // logical target, the animated camera value, and the exact delta used
            // for this turn.  Keeping these in the same monotonic trace stream
            // makes retarget/FPS regressions measurable without changing the hot
            // path when tracing is disabled.
            if trace::enabled() {
                for (mi, mon) in self.engine.state.monitors.iter().enumerate() {
                    let ws = mon.ws();
                    trace!(
                        "camera_tick",
                        "monitor={} target={} current={} velocity={} raw_dt_s={} dt_s={} animating={}",
                        mi,
                        ws.camera.target,
                        ws.camera.position,
                        ws.camera.velocity,
                        raw_dt,
                        dt,
                        anim,
                    );
                }
            }
            // Advance the wallpaper animation clock with the same clamped `dt` the
            // WM springs use (no separate timer). A static wallpaper leaves
            // `wallpaper_animating` false and the loop goes idle.
            comp.tick_wallpaper(dt);
            // Build the single turn scheduler from the WM-side animation flag,
            // the wallpaper animation flag, and the compositor's *why* (its
            // reason bits), so the render-loop decision is explicit and testable.
            // Idle stays free: when the scheduler reports no reason we do no GL
            // work and the wait phase blocks on X11/control.
            sched = FrameScheduler::from_compositor(
                self.animating || comp.presentation_animating(),
                comp.wallpaper_animating(),
                comp.dirty_reasons(),
            );
            if log::enabled(log::DEBUG) {
                log::debug!(
                    "compositor: scheduling frame (animating={}, dirty={}): {}",
                    sched.is_animating(),
                    sched.has_dirty(),
                    describe_reasons(&sched)
                );
            }
            if comp.comp_trace {
                log::info!(
                    "x11 compositor decision dirty={} reasons={}",
                    comp.dirty_reasons_bits() != 0,
                    comp.dirty_reasons_bits()
                );
            }
            let wants_frame = sched.needs_frame();
            trace!("scheduler", "gl_active=true needs_frame={wants_frame} dirty={} reasons={} wm_animation={} presentation_animation={} wallpaper_animation={}", comp.dirty_reasons_bits(), sched.trace_bits(), self.animating, comp.presentation_animating(), comp.wallpaper_animating());
            if wants_frame {
                trace::begin_frame();
                if comp.float_trace {
                    let mut fids: Vec<WindowId> = Vec::new();
                    for (mi, mon) in self.engine.state.monitors.iter().enumerate() {
                        for ws in &mon.workspaces {
                            fids.extend(ws.floats.iter().copied());
                        }
                        for (&w, c) in &self.engine.state.clients {
                            if c.monitor == mi && c.is_sticky() && c.is_float() {
                                fids.push(w);
                            }
                        }
                    }
                    comp.set_debug_floats(&fids);
                }
                // Presentation state is owned by the compositor; the WM only
                // supplies state/cfg and the animation flags.
                let prepare_trace = trace::Span::new("prepare");
                comp.prepare_frame(
                    &mut self.engine.state,
                    &self.engine.cfg,
                    &self.layout_registry,
                    &self.anim_per_mon,
                    dt,
                );
                drop(prepare_trace);
                let render_trace = trace::Span::new("render");
                // A GL failure is reported through the render result and
                // disables the compositor; release builds use panic=abort, so
                // there is no unreliable catch_unwind fallback here.
                let ok = comp.render();
                drop(render_trace);
                trace!("frame_returned", "ok={ok}");
                if !ok {
                    log::warn!("compositor: GL error — disabling, falling back to X11 path");
                    if let Some(c) = self.compositor.as_mut() {
                        c.disable();
                    }
                    self.compositor = None;
                    // The desktop must not go black: paint the configured
                    // wallpaper on the root (feh-style) and keep going.
                    self.apply_root_wallpaper();
                }
                // The frame clock is deliberately *not* re-seeded here.
                // `last_frame` was already stamped at the top of the animation
                // phase, so the next turn's `dt` spans one whole turn — which,
                // with exactly one present per turn, is precisely the
                // inter-present interval. Re-seeding after the present would
                // subtract the present itself from `dt`, and with swap interval 1
                // the present is almost the entire frame, leaving the springs
                // advanced by only the loop overhead of each 16.7 ms frame.
            }
        } else {
            // No compositor: dwm-style, zero animation. Every state change has
            // already landed on its final geometry through the single
            // `Effect::ArrangeMonitor` → `arrange` (Phase::Settled) path, so
            // there is nothing to animate and nothing to reconfigure per
            // frame. The camera springs snap straight to their target so the
            // logical state stays settled; the loop then parks on X11 plus the
            // control self-pipe exactly like a settled compositor.
            self.engine.state.snap_animations();
            self.anim_per_mon.fill(false);
            self.animating = false;
            sched = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
        }

        // The present (if any) just consumed the accumulated dirty reasons; only
        // an ongoing animation keeps the loop tight. Clear the dirty bits from
        // the same scheduler so the wait phase consults one authoritative
        // decision instead of rebuilding it (which would duplicate the NEED_FRAME
        // logic and could drift).
        let after_trace = trace::Span::new("after_present");
        sched.after_present(
            self.animating
                || self
                    .compositor
                    .as_ref()
                    .is_some_and(compositor::Compositor::presentation_animating),
        );
        let vsync_on = self
            .compositor
            .as_ref()
            .is_some_and(compositor::Compositor::vsync_active);
        self.animation_due =
            if sched.is_continuous() && framesched::should_wait_after_swap(vsync_on) {
                Some(Instant::now() + self.frame_period)
            } else {
                None
            };

        drop(after_trace);
        trace!(
            "after_present_state",
            "reasons={} wm_animation={} presentation_animation={}",
            sched.trace_bits(),
            self.animating,
            self.compositor
                .as_ref()
                .is_some_and(compositor::Compositor::presentation_animating)
        );

        // Wait on X11 plus the control self-pipe. A continuous animation has a
        // refresh-derived rate limit; a settled WM blocks indefinitely, so idle
        // does not wake on a heartbeat timer.
        let fd = self.conn.as_raw_fd();
        let requested_timeout = sched.timeout_ms();
        let mut timeout = requested_timeout.map(std::time::Duration::from_millis);
        if sched.is_continuous() {
            if let Some(due) = self.animation_due {
                timeout = Some(due.saturating_duration_since(Instant::now()));
            }
        }
        // Every bound the loop owns, in one place: see `wait_timeout`. Never
        // sleep past a pending keyboard refresh, or the coalescing window would
        // stretch to the idle timeout.
        let timeout = wait_timeout(timeout, self.kbd_refresh_due, self.shutdown_deadline);

        trace!(
            "scheduler_wait",
            "requested_ms={:?} effective_ms={:?} reasons={}",
            requested_timeout,
            timeout.map(|d| d.as_millis()),
            sched.trace_bits()
        );
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

        // Loop back → flush_client_list() rewrites _NET_CLIENT_LIST at most once per batch.
        Ok(())
    }
    /// Drive the WM until `state.running` is false or the X connection is lost.
    /// One iteration is `run_once`; graceful `quit` waits up to `SHUTDOWN_BUDGET`
    /// for clients, then `force_kill_remaining`.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        crate::log::config_trace(
            "event_loop_start",
            format_args!(
                "compositor_requested={} compositor_actual={}",
                self.engine.cfg.compositor.enabled,
                self.compositor.is_some()
            ),
        );
        crate::log::config_trace(
            "scheduler_policy",
            format_args!("compositor_actual={} animations_enabled={} off_path=snap_animations on_path=analytic_substeps idle_poll=block pacing=swap_only existing_trace_enabled={}", self.compositor.is_some(), crate::config::animations_enabled(&self.engine.cfg), trace::enabled()),
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
        let frame_period = detect_frame_period(&conn, root);

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

        // Seed the native wallpaper from config: a configured `path` becomes the
        // wallpaper source (image/shader inferred by extension); the compositor
        // decodes/uploads it below when GL is available. A missing path leaves
        // `WallpaperSource::None` so the legacy root pixmap (if any) shows.
        if let Some(path) = engine.cfg.wallpaper.path.clone() {
            engine.state.wallpaper.source =
                crate::core::wallpaper::WallpaperSource::from_path(path.into());
            engine.state.wallpaper.mode = engine.cfg.wallpaper.mode;
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

        // Bring up the compositor (if enabled and GL is available). It claims
        // `_NET_WM_CM_S0`, redirects every subwindow to Manual, and sets up the
        // GLX context. On any failure it logs and returns `None`, leaving the WM
        // on the classic `ConfigureWindow` path.
        crate::log::config_trace(
            "compositor_init_start",
            format_args!(
                "requested={} actual=false skipped_off={}",
                engine.cfg.compositor.enabled,
                !crate::config::compositor_enabled(&engine.cfg)
            ),
        );
        let mut compositor = if crate::config::compositor_enabled(&engine.cfg) {
            if let Err(e) = crate::config::validate_compositor_backend(&engine.cfg) {
                log::warn!("compositor: {e}; staying on X11 path");
                None
            } else {
                compositor::Compositor::init(
                    conn.clone(),
                    dpy,
                    root,
                    screen_num,
                    check_win,
                    &engine.cfg,
                )
            }
        } else {
            None
        };

        crate::log::config_trace(
            "compositor_init_end",
            format_args!(
                "requested={} actual={} skipped_off={}",
                engine.cfg.compositor.enabled,
                compositor.is_some(),
                !crate::config::compositor_enabled(&engine.cfg)
            ),
        );
        if crate::log::config_trace_enabled() {
            crate::log::config_trace(
                "xkb_init_start",
                format_args!("phase=fetch_after_compositor diagnostic_only=true"),
            );
            match fetch_keyboard_state(&conn) {
                Ok(after) => {
                    crate::log::config_trace("xkb_init_end", format_args!("phase=fetch_after_compositor diagnostic_only=true status=ok"));
                    crate::log::config_trace(
                        "keyboard_snapshot_after_compositor",
                        format_args!("applied=false min={} kpk={} raw={:?} xkb={} xkb_group={} numlock={:#x} scroll={:#x}", after.min, after.kpk, after.keysyms, after.xkb.is_some(), after.xkb_group, after.numlock, after.scroll),
                    );
                    crate::log::config_trace(
                        "keyboard_compare_compositor",
                        format_args!("raw_equal={} xkb_equal={} locks_equal={} range_equal={} applied=false", raw_keymap == after.keysyms, ks.xkb.is_some() == after.xkb.is_some() && ks.xkb_group == after.xkb_group, numlock == after.numlock && ks.scroll == after.scroll, raw_min == after.min && raw_kpk == after.kpk),
                    );
                }
                Err(e) => crate::log::config_trace("xkb_init_end", format_args!("phase=fetch_after_compositor diagnostic_only=true status=failed applied=false error={e}")),
            }
        }

        // Apply the configured native wallpaper (if any) to the freshly-built
        // compositor. A path of `None` leaves the legacy root pixmap in place.
        if let Some(comp) = compositor.as_mut() {
            if engine.state.wallpaper.source != crate::core::wallpaper::WallpaperSource::None {
                comp.set_wallpaper(&engine.state.wallpaper);
            }
        }

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
            last_root_pixmap: None,
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
            last_frame: std::time::Instant::now(),
            animating: false,
            frame_period,
            animation_due: None,
            last_stack_order: std::collections::HashMap::new(),
            fs_covering: std::collections::HashMap::new(),
            compositor,
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

        // No compositor: paint the configured wallpaper on the root window
        // (feh-style) now — the WM is fully loaded, owns the screen and knows
        // the final monitor layout. This is exactly the moment `feh` would be
        // launched, except it needs no race-prone external process.
        wm.apply_root_wallpaper();

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
    use x11rb::protocol::randr::ConnectionExt as _;
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

fn frame_period_from_mode(mode: &x11rb::protocol::randr::ModeInfo) -> Option<std::time::Duration> {
    let pixels = u128::from(mode.htotal) * u128::from(mode.vtotal);
    let dot_clock = u128::from(mode.dot_clock) * 1_000;
    if dot_clock == 0 || pixels == 0 {
        return None;
    }
    let period_ns = 1_000_000_000u128
        .checked_mul(pixels)?
        .checked_div(dot_clock)?;
    let period = std::time::Duration::from_nanos(u64::try_from(period_ns).ok()?);
    // Reject impossible RandR values; retain slow displays but avoid a
    // zero/overflow deadline in the scheduler.
    (std::time::Duration::from_millis(3)..=std::time::Duration::from_millis(100))
        .contains(&period)
        .then_some(period)
}

fn detect_frame_period(conn: &XConn, root: Window) -> std::time::Duration {
    let Ok(resources) = conn.randr_get_screen_resources_current(root) else {
        return std::time::Duration::from_secs_f64(1.0 / 60.0);
    };
    let Ok(resources) = resources.reply() else {
        return std::time::Duration::from_secs_f64(1.0 / 60.0);
    };
    resources
        .crtcs
        .iter()
        .filter_map(|crtc| {
            conn.randr_get_crtc_info(*crtc, x11rb::CURRENT_TIME)
                .ok()?
                .reply()
                .ok()
        })
        .filter_map(|info| resources.modes.iter().find(|m| m.id == info.mode))
        .filter_map(frame_period_from_mode)
        .max_by_key(|p| *p) // lowest reported refresh across outputs
        .unwrap_or_else(|| std::time::Duration::from_secs_f64(1.0 / 60.0))
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

#[cfg(test)]
mod reason_phrase_tests {
    use super::describe_reasons;
    use super::framesched::{FrameReason, FrameScheduler};

    /// The phrase a user is asked to paste into a bug report, so the order and
    /// the separator are part of the contract.
    #[test]
    fn reasons_read_in_scheduler_order() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Animation);
        s.mark(FrameReason::Damage);
        s.mark(FrameReason::Geometry);
        assert_eq!(describe_reasons(&s), "animation, damage, geometry");
    }

    #[test]
    fn a_single_reason_has_no_separator() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Focus);
        assert_eq!(describe_reasons(&s), "focus");
    }

    /// An idle scheduler must produce an empty phrase rather than a stray
    /// separator, since the caller formats it unconditionally at debug level.
    #[test]
    fn no_reasons_read_as_nothing() {
        assert_eq!(describe_reasons(&FrameScheduler::new()), "");
    }
}
