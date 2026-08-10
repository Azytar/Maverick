// maverick/src/backend/x11/mod.rs
// Window manager core — niri-style columnar layout, clean coords.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use maverick_gl::{XConn, XDisplay};

use crate::backend::atoms::Atoms;
use crate::config::Cfg;
use crate::core::layout::{arrange, ideal_scroll, Placements, RibbonScratch};
use crate::core::{parse_action, state_json, Effect, Engine};
use crate::log;
use crate::types::*;

mod actions;
mod compositor;
mod events;
mod ewmh;
mod hubevents;
mod input;
mod manage;
mod pointer;
mod render;
mod struts;
#[cfg(test)]
mod tests;
use pointer::DragState;

/// How long a keyboard-change notification waits for its siblings before the
/// keymap is re-read and every grab rebuilt. A single `setxkbmap` produces a
/// core `MappingNotify` *and* an XKB `MapNotify` (plus a `NewKeyboardNotify` on
/// hotplug); 50 ms is far below human perception and comfortably wider than the
/// gap between them.
const KBD_REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

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
    keymap: BTreeMap<(u16, u32), crate::types::Action>,
    raw_keymap: Vec<u32>,
    raw_kpk: usize,
    raw_min: u8,
    /// Keycodes that were grabbed through the *keysym-directed fallback* (the
    /// bound keysym is unreachable in group 1, so it only exists in an `AltGr` /
    /// second-group column), mapped to the normalised keysyms they stand for.
    /// `on_key` consults this after the group-1 columns fail, which is what
    /// keeps `Mod4+bracketleft` alive on `es`/`latam` without grabbing keys the
    /// dispatch could never resolve (R1/R2).
    code_bindings: std::collections::HashMap<u8, Vec<u32>>,
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
    /// P5: Deferred _`NET_CLIENT_LIST` update. Set on manage/unmanage, flushed in event loop.
    client_list_dirty: bool,
    /// P9: Deferred restack — only restack when floats/fullscreen change.
    stack_dirty: bool,
    /// P12: Reusable buffers for `hide_offscreen` — avoids reallocation per arrange.
    hide_ws_set: std::collections::HashSet<Window>,
    hide_mon_vec: Vec<Window>,
    /// P10: Reusable placements buffer — avoids allocation per `arrange()` call.
    placements_buf: Placements,
    /// P10: Reusable raise-list scratch for `live_placements` → `present_into`.
    /// The WM discards the raise list, so a fresh `Vec` here would allocate once
    /// per animating monitor per frame. Owned by the WM and threaded through.
    compositor_present_scratch: Vec<WindowId>,
    /// Per-monitor cached live placements; recomputed only when that monitor's
    /// layout actually changes (or it is still animating), so an idle monitor
    /// costs nothing while another scrolls. Parallel to `state.monitors`.
    live_cache: Vec<Vec<(Window, Rect, u32)>>,
    /// Reusable transform buffer for `set_transforms` — avoids a `Vec` alloc per
    /// animation frame.
    transforms_buf: Vec<(Window, Rect, u32)>,
    /// Reusable raise-list scratch for the per-frame projection (`present_into`).
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
    /// Instance name passed via --name (for identity/control).
    instance_name: String,
    /// Config file path that was loaded at boot (the --config override when
    /// given, otherwise the resolved XDG path, or `None` when the compiled
    /// defaults were used). `reload_config` re-reads this exact file (B10/T7):
    /// the override must survive a reload, not be silently replaced by the
    /// XDG default.
    config_path: Option<PathBuf>,
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
    /// Tiled window currently highlighted by the drag-to-tile preview (its
    /// border is painted `col_focused`). Reverted when the pointer moves away
    /// or the drag ends.
    drag_target: Option<Window>,
    /// Timestamp of the previous animation frame, for `dt` in `tick_animations`.
    last_frame: Instant,
    /// True while any camera/zoom/accordion spring is still moving; drives the
    /// frame-clock timeout (high rate while animating, idle 100ms otherwise).
    animating: bool,
    /// Count of unmap operations the WM itself initiated. X11 delivers the WM
    /// its own `UnmapNotify` back (`SubstructureNotify` on root), and without
    /// this counter `on_unmap` would treat that self-unmap as the client
    /// withdrawing and unmanage the window — permanently deleting it (see bug
    /// C1). Incremented before every `unmap_window` the WM performs;
    /// consumed in `on_unmap`.
    ignore_unmaps: std::collections::HashMap<Window, u32>,
    /// Per-monitor cached stacking order (top-to-bottom) so `stack_overlay`
    /// only re-issues `raise()` when the order actually changed, instead of
    /// re-raising every float/popup on every animation frame (bug C6).
    last_stack_order: std::collections::HashMap<usize, Vec<WindowId>>,
    /// Per-monitor record of which fullscreen window was "covering" (raised
    /// above the dock) on the previous frame, so the dock is only re-raised on
    /// the covering→not-covering transition — not every frame (which would push
    /// floats below the bar, a regression).
    fs_covering: std::collections::HashMap<usize, Option<WindowId>>,
    /// The OpenGL/GLX compositor, if enabled and a GL driver was available at
    /// startup. While `Some`, every animation frame is drawn here (GPU
    /// transforms + vsync) instead of re-`ConfigureWindow`ing each window. Falls
    /// back to `None` (the classic X11 path) on `MAVERICK_NO_COMPOSITOR`, a
    /// missing driver, or a runtime GL error.
    compositor: Option<compositor::Compositor>,
}

impl WindowManager {
    fn dispatch(&mut self, ev: x11rb::protocol::Event) -> Result<(), Box<dyn std::error::Error>> {
        match ev {
            Event::ButtonPress(e) => self.on_button_press(e)?,
            Event::ButtonRelease(e) => self.on_button_release(e)?,
            Event::ClientMessage(e) => self.on_client_message(e)?,
            Event::ConfigureNotify(e) => self.on_configure_notify(e)?,
            Event::ConfigureRequest(e) => self.on_configure_request(e)?,
            Event::CreateNotify(e) => self.on_create_notify(e)?,
            Event::DestroyNotify(e) => self.on_destroy(e)?,
            Event::EnterNotify(e) => self.on_enter(e)?,
            Event::KeyPress(e) => self.on_key(e)?,
            Event::MappingNotify(e) => self.on_mapping(&e),
            Event::MapNotify(e) => self.on_map_notify(e)?,
            Event::MapRequest(e) => self.on_map_request(e)?,
            Event::MotionNotify(e) => self.on_motion(e)?,
            Event::PropertyNotify(e) => self.on_property(e)?,
            Event::UnmapNotify(e) => self.on_unmap(e)?,
            Event::DamageNotify(e) => self.on_damage_notify(e)?,
            // RandR change events (config/grab selected in `setup_root`): both
            // the 1.5 `NotifyEvent` (crtc/output changes) and the classic
            // `ScreenChangeNotifyEvent` funnel into the same re-detect handler as
            // a root ConfigureNotify would.
            Event::RandrNotify(_) | Event::RandrScreenChangeNotify(_) => {
                self.handle_monitor_change()?
            }
            // XKB keyboard changes. `MapNotify` covers remaps that never raise a
            // core `MappingNotify` (a pure XKB `setxkbmap`), `NewKeyboardNotify`
            // covers hotplug. Both share the debounced refresh with the core
            // path, so the usual "all three at once" burst regrabs only once.
            Event::XkbMapNotify(_) | Event::XkbNewKeyboardNotify(_) => {
                self.schedule_keyboard_refresh();
            }
            // Errors from the many fire-and-forget requests the WM issues
            // (`let _ = …`). Debug, not warn: `BadWindow` from a client that
            // died between our request and the server processing it is routine
            // — `maverick-gl` installs a silent Xlib error handler for the same
            // reason. Without this arm a `BadAccess` from a rejected grab was
            // simply invisible (R4).
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
    /// keyboard must never take the WM down with it (R3) — the previous keymap
    /// stays in place and the next notification retries.
    pub(super) fn refresh_keyboard(&mut self) {
        match fetch_keyboard_state(&self.conn) {
            Ok(ks) => {
                self.raw_keymap = ks.keysyms;
                self.raw_kpk = ks.kpk;
                self.raw_min = ks.min;
                self.numlock = ks.numlock;
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
            "keyboard: keymap refreshed ({} keysyms/keycode, {} fallback keycodes)",
            self.raw_kpk,
            self.code_bindings.len()
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
    pub fn cleanup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        // Restore root event mask: remove SUBSTRUCTURE_REDIRECT so that
        // the next WM doesn't fail on startup.
        let _ = self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );

        // Ungrab buttons on all managed windows
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

        self.conn.flush()?;

        // Tear down the control socket + identity ficha so external tools stop
        // listing this (now dead) instance. The ControlServer thread stops when
        // its handle is dropped at the end of the process; explicitly remove the
        // on-disk meta here.
        if !self.instance_name.is_empty() {
            maverick_sys::identity::cleanup_meta(&self.instance_name);
        }
        drop(self.control.take());
        Ok(())
    }
    fn run_once(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::io::AsRawFd;

        // ── signal phase ─────────────────────────────────────────────────────────
        // SIGCONT (resume from stop) requests a key regrab; SIGTERM requests quit.
        // Both are set by the maverick-sys signal handlers (the only unsafe code).
        if maverick_sys::need_regrab() {
            maverick_sys::clear_regrab();
            self.refresh_keyboard();
        }
        if maverick_sys::quit_requested() {
            maverick_sys::clear_quit();
            self.engine.state.running = false;
            return Ok(());
        }

        // ── flush phase ─────────────────────────────────────────────────────────
        // Drain the deferred _NET_CLIENT_LIST update (if any manage/unmanage
        // marked it dirty) before blocking on the next event, so all X11
        // output from the previous event batch is flushed in one shot.
        self.flush_client_list()?;
        self.conn.flush()?;

        // ── animation phase ──────────────────────────────────────────────────
        // Advance camera (and accordion/zoom) springs. While anything is still
        // moving we keep ticking at a high frame rate; otherwise we fall back to
        // the idle 100ms wake so control-socket commands are still drained
        // promptly.
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().clamp(0.0, 0.05);
        self.last_frame = now;

        // Whether the compositor paced this frame on the real vblank (drives the
        // wait phase below: when true we don't also sleep on a timer).
        let mut vblank_synced = false;
        if let Some(comp) = self.compositor.as_mut() {
            // Compositor path: the camera is substepped (its semi-implicit
            // integrator is unstable above ~8 ms) and the *live* layout — read
            // from the spring's current value — is drawn by the GPU with vsync.
            // The WM's settled geometry was already written by whichever action
            // triggered the change, so no per-frame `ConfigureWindow` storm.
            let mut anim = false;
            for sub in compositor::substep_bounds(dt) {
                anim |= self.engine.state.tick_animations(sub);
            }
            self.animating = anim;
            if self.animating || comp.needs_frame() {
                // Pace to the vertical retrace *before* drawing so the GPU flip
                // lands on a retrace. This is the core fluidity fix: a fixed
                // 16 ms sleep drifted against the real refresh and made us miss
                // vblanks (~30 fps during animation). When `GLX_SGI_video_sync`
                // is present the renderer blocks here; otherwise swap interval
                // 1 (set at init) paces the present and the loop falls back to a
                // short poll in the wait phase.
                vblank_synced = comp.wait_vblank();
                // Keep the per-monitor cache sized to the live monitor set; a
                // change in monitor count (hotplug) invalidates everything.
                if self.live_cache.len() != self.engine.state.monitors.len() {
                    self.live_cache = vec![Vec::new(); self.engine.state.monitors.len()];
                    for m in &mut self.engine.state.monitors {
                        m.layout_dirty = true;
                    }
                }
                self.transforms_buf.clear();
                for i in 0..self.engine.state.monitors.len() {
                    // Recompute only monitors whose layout changed or that are
                    // still animating. Idle monitors keep their last projection,
                    // so a scroll on one monitor costs nothing on the others.
                    let recompute = self.animating || self.engine.state.monitors[i].layout_dirty;
                    if recompute {
                        self.placements_buf.clear();
                        compositor::live_placements(
                            &self.engine.state,
                            i,
                            &self.engine.cfg,
                            &self.layout_registry,
                            &mut self.placements_buf,
                            &mut self.compositor_present_scratch,
                            &mut self.ribbon_scratch,
                        );
                        self.live_cache[i].clear();
                        self.live_cache[i].extend(self.placements_buf.iter().copied());
                        self.engine.state.monitors[i].layout_dirty = false;
                    }
                    self.transforms_buf
                        .extend(self.live_cache[i].iter().copied());
                }
                comp.set_transforms(&self.transforms_buf);
                // A GL failure disables the compositor and returns us to the
                // classic path. `panic = "abort"` means a GL panic would kill
                // the whole WM, so the draw is isolated behind `catch_unwind`.
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| comp.render()))
                    .unwrap_or(false);
                if !ok {
                    log::warn!("compositor: GL error — disabling, falling back to X11 path");
                    if let Some(c) = self.compositor.as_mut() {
                        c.disable();
                    }
                    self.compositor = None;
                }
            }
        } else {
            self.animating = self.engine.state.tick_animations(dt);
            if self.animating {
                for i in 0..self.engine.state.monitors.len() {
                    let _ = self.arrange_live(i);
                }
            }
        }

        // ── drain + wait phase ─────────────────────────────────────────────────
        // Drain *first*, block second. `poll(2)` only sees bytes still sitting
        // in the socket; every GLX round-trip (`glXSwapBuffers`, `XSync`) makes
        // libxcb read the socket dry, so a KeyPress that arrives in that window
        // ends up in libxcb's internal queue with the fd no longer readable.
        // Waiting first meant sleeping the full timeout on top of an event we
        // already had — keyboard stutter that only showed up with the
        // compositor on. `xcb_poll_for_event` returns the internal queue first,
        // which is exactly what `poll(2)` cannot see.
        //
        // Pacing: while animating we no longer sleep on a fixed 16 ms timer
        // (that drifted against the real refresh and made us miss vblanks,
        // dropping to ~30 fps). The frame is already synced to the retrace by
        // `comp.wait_vblank()` above (or by swap interval 1); here we only
        // block on the socket so X/control-socket events are drained promptly.
        // `block_ms` is 0 when the vblank already paced us, a couple of ms as a
        // fallback poll when it did not (avoids 100% CPU spin), or 100 ms idle.
        let fd = self.conn.as_raw_fd();
        let pending = self.animating
            || self
                .compositor
                .as_ref()
                .is_some_and(compositor::Compositor::needs_frame);
        let mut timeout_ms: u64 = if pending {
            if vblank_synced {
                0
            } else if self.compositor.is_some() {
                // Compositor present but no video_sync: a short poll avoids a
                // 100% CPU spin while swap interval 1 still paces the present.
                2
            } else {
                // Non-composited fallback path: keep the original 16 ms poll
                // (no vsync here; this just bounds the ConfigureWindow rate).
                16
            }
        } else {
            100
        };
        // Never sleep past a pending keyboard refresh, or the coalescing window
        // would stretch to the idle timeout.
        if let Some(due) = self.kbd_refresh_due {
            let left = due.saturating_duration_since(Instant::now());
            timeout_ms = timeout_ms.min(left.as_millis() as u64);
        }

        while let Some(ev) = self.conn.poll_for_event()? {
            self.dispatch(ev)?;
        }
        if timeout_ms > 0 {
            maverick_sys::wait_readable(fd, std::time::Duration::from_millis(timeout_ms));
            // Drain again for anything that arrived while we were blocked.
            while let Some(ev) = self.conn.poll_for_event()? {
                self.dispatch(ev)?;
            }
        }

        // ── keyboard phase ─────────────────────────────────────────────────────
        // One regrab per burst of keyboard-change notifications (see
        // `schedule_keyboard_refresh`).
        if self
            .kbd_refresh_due
            .is_some_and(|due| Instant::now() >= due)
        {
            self.kbd_refresh_due = None;
            self.refresh_keyboard();
        }

        // ── control phase ────────────────────────────────────────────────────────
        // Execute any commands from the control socket, then publish state.
        self.drain_control()?;
        self.publish_state();

        // Loop back → flush_client_list() rewrites _NET_CLIENT_LIST at most once per batch.
        Ok(())
    }
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        while self.engine.state.running {
            if let Err(e) = self.run_once() {
                return if is_x11_connection_loss(&*e) {
                    log::info!("maverick: X11 connection lost (X server disconnected)");
                    Ok(())
                } else {
                    Err(e)
                };
            }
        }
        Ok(())
    }
    pub fn new(
        cfg: Cfg,
        replace: bool,
        config_path: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (dpy, conn, screen_num) = maverick_gl::open_x()?;
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

        // create EWMH check window
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

        let ks = fetch_keyboard_state(&conn)?;
        let (raw_keymap, raw_kpk, raw_min, numlock) = (ks.keysyms, ks.kpk, ks.min, ks.numlock);
        let keymap = build_keymap(&engine.cfg);

        // Bring up the compositor (if enabled and GL is available). It claims
        // `_NET_WM_CM_S0`, redirects every subwindow to Manual, and sets up the
        // GLX context. On any failure it logs and returns `None`, leaving the WM
        // on the classic `ConfigureWindow` path.
        let compositor = if crate::config::compositor_enabled(&engine.cfg) {
            compositor::Compositor::init(
                conn.clone(),
                dpy,
                root,
                screen_num,
                check_win,
                &engine.cfg,
            )
        } else {
            None
        };

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
            keymap,
            raw_keymap,
            raw_kpk,
            raw_min,
            code_bindings: std::collections::HashMap::new(),
            last_grab_warnings: Vec::new(),
            kbd_refresh_due: None,
            drag: None,
            client_list_dirty: false,
            stack_dirty: false,
            hide_ws_set: std::collections::HashSet::with_capacity(32),
            hide_mon_vec: Vec::with_capacity(64),
            placements_buf: Placements::with_capacity(32),
            compositor_present_scratch: Vec::with_capacity(32),
            live_cache: Vec::new(),
            transforms_buf: Vec::with_capacity(256),
            present_scratch: Vec::with_capacity(32),
            ribbon_scratch: RibbonScratch::default(),
            last_key_times: std::collections::BTreeMap::new(),
            control: None,
            instance_name: String::new(),
            config_path,
            hub: None,
            last_state_json: String::new(),
            docks: std::collections::HashMap::new(),
            pointer_guard_until: None,
            last_event_time: 0,
            drag_target: None,
            last_frame: std::time::Instant::now(),
            animating: false,
            ignore_unmaps: std::collections::HashMap::new(),
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

        wm.conn.flush()?;
        log::info!("maverick ready");
        Ok(wm)
    }
}

// ── Free functions ─────────────────────────────────────────────────────────────

/// Interpret a strut vector as a single (edge, thickness). Both `_NET_WM_STRUT`
/// (4 values) and `_NET_WM_STRUT_PARTIAL` (12 values) start with
/// `[left, right, top, bottom]`; we take the first non-zero edge.
fn strut_edge(v: &[u32]) -> Option<(Edge, u32)> {
    let (left, right, top, bottom) = (v[0], v[1], v[2], v[3]);
    if top > 0 {
        Some((Edge::Top, top))
    } else if bottom > 0 {
        Some((Edge::Bottom, bottom))
    } else if left > 0 {
        Some((Edge::Left, left))
    } else if right > 0 {
        Some((Edge::Right, right))
    } else {
        None
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
        // to be stored under `0x61` (`a`) or it could never be matched (R8).
        // The grab side still searches for the raw keysym — `0x41` genuinely
        // lives in column 1 of the `a` keycode — so both halves agree.
        //
        // First wins: a later duplicate `(mods, keysym)` does not overwrite the
        // earlier one (B7). Mirrors the conflict policy in `parse_keybindings`.
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
}

/// P2: Pipelined keyboard+modifier state — fire both requests, then collect both replies.
/// 2 RTTs → 1.
fn fetch_keyboard_state(conn: &XConn) -> Result<KeyboardState, Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = (max as u16 - min as u16 + 1) as u8;

    let c_kb = conn.get_keyboard_mapping(min, count)?;
    let c_mod = conn.get_modifier_mapping()?;

    let map = c_kb.reply()?;
    let kpk = map.keysyms_per_keycode as usize;
    let keysyms = map.keysyms.clone();

    let numlock = if let Ok(modmap) = c_mod.reply() {
        let kpm = modmap.keycodes_per_modifier() as usize;
        compute_numlock(&modmap.keycodes, kpm, &keysyms, kpk, min, max)
    } else {
        0
    };

    Ok(KeyboardState {
        keysyms,
        kpk,
        min,
        numlock,
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
    for (i, codes) in keycodes.chunks(kpm).enumerate() {
        for &code in codes {
            if code == 0 || code < min || code > max {
                continue;
            }
            let idx = (code - min) as usize * kpk;
            if (0..kpk).any(|j| keysyms[idx + j] == XK_NUM_LOCK) {
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

/// Keycodes whose **group 1** produces `keysym` — columns 0 and 1 only
/// (unshifted / shifted).
///
/// Group 1 is the only part of the core keymap with an unambiguous meaning.
/// Columns 2 and 3 hold *either* group 2 (a second layout, e.g. `us,de`) *or*
/// levels 3/4 of group 1 (`AltGr`), and which one it is varies **per key** — the
/// server decides row by row. Grabbing on those columns is what made `Mod4+z`
/// under `us,de` also grab the physical `y` key: the grab is on root with
/// `owner_events=true` and the WM never selects `KeyPress` on client windows,
/// so X routed the press to the WM, `on_key` resolved nothing, and the
/// application simply never saw the key (R1).
fn keysym_to_codes_group1(keysyms: &[u32], min: u8, kpk: usize, keysym: u32) -> Vec<u8> {
    if kpk == 0 {
        return Vec::new();
    }
    keysyms
        .chunks(kpk)
        .enumerate()
        .filter(|(_, syms)| syms.iter().take(2).any(|s| *s == keysym))
        .filter_map(|(i, _)| row_keycode(min, i))
        .collect()
}

/// Keycodes that produce `keysym` in **any** column/group/level. Only used as a
/// keysym-directed fallback for binds that group 1 cannot reach at all (see
/// `plan_key_grabs`); grabbing everything this returns unconditionally is the
/// R1 bug.
fn keysym_to_codes_any(keysyms: &[u32], min: u8, kpk: usize, keysym: u32) -> Vec<u8> {
    if kpk == 0 {
        return Vec::new();
    }
    keysyms
        .chunks(kpk)
        .enumerate()
        .filter(|(_, syms)| syms.contains(&keysym))
        .filter_map(|(i, _)| row_keycode(min, i))
        .collect()
}

/// What `grab_keys` should ask the server for, computed without touching X so
/// it can be unit-tested against synthetic keymaps.
#[derive(Debug, Default)]
struct KeyGrabPlan {
    /// `(modifier mask, config keysym, keycode)` triples to grab, deduplicated
    /// by `(mask, keycode)` — the server answers a repeat of the same
    /// combination with `BadAccess`, which would look like a real conflict.
    grabs: Vec<(u16, u32, u8)>,
    /// Keycodes grabbed via the fallback, and the normalised keysyms they were
    /// grabbed *for*. Becomes `WindowManager::code_bindings`.
    code_bindings: std::collections::HashMap<u8, Vec<u32>>,
    /// Binds whose keysym does not exist anywhere in the current layout.
    missing: Vec<(u16, u32)>,
}

/// Resolve every configured bind to the keycodes to grab, under the
/// "strict group 1" policy:
///
/// 1. Look for the keysym in columns 0/1 (group 1). This is layout-independent
///    in the only way that matters — the dispatch reads those same columns.
/// 2. If it is not there, scan the whole row **for that keysym only** and
///    record the hits in `code_bindings` so `on_key` can still resolve them.
///    That keeps `Mod4+bracketleft` working on `es`/`latam`, where `[` only
///    exists at the `AltGr` level, without grabbing a single key the dispatch
///    would drop on the floor.
/// 3. If it exists nowhere, report it as missing so the caller can warn: a
///    silent grab that resolves to nothing steals the key from the application.
fn plan_key_grabs(keysyms: &[u32], min: u8, kpk: usize, binds: &[(u16, u32)]) -> KeyGrabPlan {
    let mut plan = KeyGrabPlan::default();
    if kpk == 0 {
        return plan;
    }
    let mut seen: std::collections::HashSet<(u16, u8)> = std::collections::HashSet::new();
    for &(mask, keysym) in binds {
        let mut codes = keysym_to_codes_group1(keysyms, min, kpk, keysym);
        let fallback = codes.is_empty();
        if fallback {
            codes = keysym_to_codes_any(keysyms, min, kpk, keysym);
        }
        if codes.is_empty() {
            plan.missing.push((mask, keysym));
            continue;
        }
        for code in codes {
            if fallback {
                // A `Vec`, not a single keysym: two different binds can land on
                // the same keycode at levels 3 and 4 (`bracketleft` and
                // `braceleft` on `es`), and both have to stay resolvable.
                let entry = plan.code_bindings.entry(code).or_default();
                let ks = normalize_ksym(keysym);
                if !entry.contains(&ks) {
                    entry.push(ks);
                }
            }
            if seen.insert((mask, code)) {
                plan.grabs.push((mask, keysym, code));
            }
        }
    }
    plan
}

/// Keymap column `on_key` reads for its shifted-symbol fallback.
///
/// Clamped to group 1 (columns 0/1). The old `col.min(kpk - 1)` reached column
/// 3 on a `kpk = 4` keymap — a *different group*, which is neither what was
/// grabbed nor what the user pressed (R2).
#[inline]
fn dispatch_col(shift: bool, lock: bool, kpk: usize) -> usize {
    usize::from(shift ^ lock).min(1).min(kpk.saturating_sub(1))
}

/// Match a physical key press against the bindings.
///
/// Pure so the resolution order is testable: group-1 unshifted, then the
/// group-1 shifted column, then the keysyms recorded for keycodes that were
/// grabbed through the fallback. Returns the `(mods, keysym)` that actually
/// matched, which is what the repeat rate-limiter must key on.
fn resolve_binding(
    keymap: &BTreeMap<(u16, u32), Action>,
    code_bindings: &std::collections::HashMap<u8, Vec<u32>>,
    mods: u16,
    code: u8,
    ks_primary: u32,
    ks_shifted: u32,
) -> Option<((u16, u32), Action)> {
    for ks in [normalize_ksym(ks_primary), normalize_ksym(ks_shifted)] {
        if ks == 0 {
            continue;
        }
        if let Some(a) = keymap.get(&(mods, ks)) {
            return Some(((mods, ks), a.clone()));
        }
    }
    for &ks in code_bindings.get(&code).into_iter().flatten() {
        if let Some(a) = keymap.get(&(mods, ks)) {
            return Some(((mods, ks), a.clone()));
        }
    }
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
/// P14: Fire both `net_wm_name` and `WM_NAME` requests before reading any reply.
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
fn mod_variants(numlock: u16) -> [u16; 4] {
    let lock = u16::from(ModMask::LOCK);
    [0, numlock, lock, numlock | lock]
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
fn clean_mask(state: u16, numlock: u16) -> u16 {
    let lock: u16 = ModMask::LOCK.into();
    state
        & !(numlock | lock)
        & (u16::from(ModMask::SHIFT)
            | u16::from(ModMask::CONTROL)
            | u16::from(ModMask::M1)
            | u16::from(ModMask::M2)
            | u16::from(ModMask::M3)
            | u16::from(ModMask::M4)
            | u16::from(ModMask::M5))
}
