//! Pointer interaction: grabs, drag/resize, focus-on-click, scroll wheel.
//!
//! # Pointer authority (I1/I2)
//!
//! During a drag the WM re-asserts its geometry on every
//! `ConfigureRequest`. The client cannot move/resize a
//! floating window while the drag is in progress — the
//! drag is the authoritative source of geometry until the
//! button release.
//!
//! # Grab lifecycle
//!
//! Every managed window carries a `SYNC` grab on buttons 1-3 and on
//! `Mod4`+wheel, installed once at manage time (see `input::grab_buttons`), so
//! the server freezes the pointer on those `ButtonPress`es until
//! `on_button_press` calls `allow_events`. A drag adds an
//! active `ASYNC` pointer grab on the root, released by `on_button_release`.
//! `SyncGrabGuard` releases *either* grab on every exit path (see below), so a
//! handler that returns early can never leave the server with a frozen device.
//!
//! # Focus-on-click
//!
//! Clicking a window focuses it, unless it declared `WM_HINTS.input` false —
//! the same eligibility rule `focus()` applies to every other route.
//! Focus-follows-mouse is guarded by `pointer_guard_until`
//! (50 ms after a keypress to avoid conflicting with
//! keyboard focus).
//!
//! # Pointer policy while Overview is on
//!
//! Overview adds no grab and no new gesture; the rules below are what the
//! existing paths do when the active workspace is in Overview, stated here so
//! selection is never confused with dragging:
//!
//! - *Motion* (hover) selects through the same `EnterNotify` focus path as
//!   outside Overview (and only when `focus_mouse` is set). It moves the
//!   logical focus and pans the viewport only if the selected tile is not
//!   already visible (`retarget_focus_to_window` → `overview_scroll`); it
//!   never writes geometry, never changes the stored entry scale, and never
//!   converts a tiled window to floating. `EnterNotify` fires once per window
//!   entry rather than per motion event, so repeated motion cannot pan
//!   unstably; an `Enter` that arrives without the pointer having moved (a
//!   pan slid another tile underneath it) carries no new intent and is
//!   ignored (`PointerTruth`), which is what stops a selection from
//!   cascading down the ribbon on its own. The 50 ms post-keypress guard
//!   keeps keyboard navigation from being undone by a parked pointer.
//! - *Click* selects and focuses through the same focus-on-click path, and the
//!   press is replayed to the client as usual, so application interaction is
//!   untouched. A click never floats a tile on its own.
//! - *Drag* (move/resize) starts only as `Mod4+Button` on an already-floating
//!   window — a `Mod4` drag on a tile is a no-op that can never detach it as a
//!   float, and a float drag can never drop back into a column. Selection and
//!   dragging therefore cannot be confused: the drag needs the modifier plus
//!   the floating state, selection needs neither.
//! - *Exit* restores the normal behaviour by clearing the mode; there is no
//!   Overview grab to release (no button, pointer or keyboard grab is taken on
//!   entry) and no drag state to complete — `SyncGrabGuard` still guards every
//!   handler exit as usual.
//!
//! # Scroll wheel
//!
//! `Mod4+wheel` scrolls the camera by stepping the focused column (reuses the
//! `FocusDir` action). Only `Mod4+wheel` is grabbed (see `grab_buttons`), so a
//! plain wheel notch goes straight to the client and never reaches the WM.
//!
//! # Quadrant resize
//!
//! Drag from a corner/edge resizes that corner:
//! top-left, top-right, bottom-left, bottom-right.
//! The `snap_float_to_hints` + `clamp_float_to_workarea`
//! normalisation keeps the float in-bounds after every
//! motion event.

use super::render::clamp_float_to_workarea;
use super::*;

// `itrace!` exists only when the `input-trace` feature is on; its call sites
// carry the same `cfg`, so a normal build compiles neither.
#[cfg(feature = "input-trace")]
macro_rules! itrace {
    ($($arg:tt)*) => {{
        eprintln!("[INPUT-TRACE] {}", format!($($arg)*));
    }};
}
// `wtrace!` is the window-level counterpart of `itrace!`: same arrangement
// against the `window-trace` feature, different event stream.
#[cfg(feature = "window-trace")]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
/// How a pointer grab must be undone on exit: either release the frozen
/// `ButtonPress` back to the client, or drop the active drag grab.
enum GrabRelease {
    AllowReplay(u32),
    Ungrab,
}

/// Drop guard that guarantees the pointer grab is released on *every* exit
/// path. The per-window `grab_button(..., GrabMode::SYNC, ...)` freezes the
/// pointer on every `ButtonPress` until `allow_events` runs, and an active drag
/// grab freezes it until `ungrab_pointer` runs. A handler that returns via `?`
/// before reaching those calls leaves the device frozen on the *server* side —
/// a global input freeze that survives workspace and window changes. Holding a
/// clone of the connection, the guard performs the release itself.
struct SyncGrabGuard {
    conn: Rc<XConn>,
    release: GrabRelease,
    emitted: bool,
    tag: &'static str,
}

impl Drop for SyncGrabGuard {
    fn drop(&mut self) {
        if self.emitted {
            return;
        }
        match self.release {
            GrabRelease::AllowReplay(t) => {
                let _ = self.conn.allow_events(Allow::REPLAY_POINTER, t);
            }
            GrabRelease::Ungrab => {
                let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
            }
        }
        // Reached only when the handler returned through `?` before releasing
        // the grab, so this is a dispatch fault rather than a trace event, and it
        // is the one place a bug would strand the pointer for every client on the
        // server. It therefore reports at `error`, which the logger prints at
        // every level the WM ships (`info` included) and which `MAVERICK_LOG=off`
        // is the documented way to silence. It must not wear an `input-trace`
        // label: that feature gates per-event tracing, and a mandatory
        // diagnostic filed under it is indistinguishable from the noise it
        // silences, so a reader cannot tell a real fault from a trace line.
        log::error!(
            "FREEZE-RISK: {} exited WITHOUT releasing the SYNC pointer grab — auto-released on drop (pointer was about to freeze)",
            self.tag
        );
    }
}

/// Root coordinates of the last genuine pointer observation (`MotionNotify`
/// or `ButtonPress`), or `None` before the first one. This is the whole of
/// what `on_enter` needs to tell a real pointer movement apart from a window
/// that moved underneath a stationary cursor.
///
/// An `EnterNotify` that arrives with exactly these coordinates means the
/// pointer did not move: an arrange re-projected the ribbon after a camera
/// pan, or a client mapped/unmapped there. There is no new user intent in such
/// an event, so `on_enter` ignores it for focus — acting on it re-selects
/// whatever slid under the cursor, and in Overview each re-selection pans
/// again, which slides the next tile under the cursor: a runaway that walks
/// the selection to the end of the ribbon.
///
/// The truth stays fresh because every managed window selects
/// `POINTER_MOTION` (see `manage`), so `on_motion` observes motion over tiles
/// as well as over the root — without that subscription it would only ever
/// see the root, and the comparison would run on a stale position. A warp
/// (including `xdotool mousemove --sync`, which the Xephyr suite uses for its
/// hover probes) is observed the same way: the server delivers the warp's own
/// `EnterNotify` *before* its trailing `MotionNotify`, so the entering event
/// still differs from the recorded position and takes the normal path, and
/// the trailing motion then records the parked position for the next check.
/// Focusing a newly mapped window is `manage()`'s own decision, never this
/// path's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PointerTruth {
    last: Option<(i32, i32)>,
}

impl PointerTruth {
    /// Record a genuine pointer observation: the pointer is observably here.
    pub(super) fn note(&mut self, x: i32, y: i32) {
        self.last = Some((x, y));
    }

    /// Whether an `EnterNotify` at root `(x, y)` carries new user intent: true
    /// unless the pointer is exactly where it was last observed.
    pub(super) fn enter_carries_intent(&self, x: i16, y: i16) -> bool {
        self.last != Some((i32::from(x), i32::from(y)))
    }
}

/// Live drag/resize state for the window currently grabbed by `Mod4+Button`.
/// Owned by `WindowManager::drag`; `None` when no drag is active.
#[derive(Debug)]
pub(super) struct DragState {
    /// Window being dragged.
    pub(super) win: Window,
    pub(super) start_geom: Rect,
    pub(super) ptr_x: i32,
    pub(super) ptr_y: i32,
    pub(super) resize: bool,
    /// Grip handed: which corner the resize grows toward. True means the pointer
    /// grabbed the left/top half, so that edge follows the pointer and the
    /// opposite corner stays anchored.
    pub(super) resize_l: bool,
    pub(super) resize_t: bool,
}

impl WindowManager {
    pub(super) fn on_button_press(
        &mut self,
        e: ButtonPressEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Unconditional: guarantees the SYNC pointer grab is released on every
        // exit path, even in non-trace builds (see `SyncGrabGuard`).
        let mut _guard = SyncGrabGuard {
            conn: self.conn.clone(),
            release: GrabRelease::AllowReplay(e.time),
            emitted: false,
            tag: "on_button_press",
        };

        // A click (or drag start) after queued notches acts on the state those
        // notches produced, so they are applied first.
        if e.detail < 4 {
            self.apply_wheel_steps()?;
        }

        // Scroll buttons (4=up,5=down,6=left,7=right). With no modifier they are
        // just delivered to the application (REPLAY_POINTER). With Mod4 held they
        // step the ribbon camera left/right through the focused column.
        if e.detail >= 4 {
            let sup: u16 = ModMask::M4.into();
            let clean = clean_mask(u16::from(e.state), self.numlock, self.scroll);
            if clean == sup {
                self.scroll_camera_with_wheel(e.detail)?;
                // Consumed as a WM gesture: release the SYNC grab WITHOUT
                // replay (ASYNC discards the press so the app doesn't also
                // scroll) and mark the guard emitted — falling through to
                // `Drop` would REPLAY plus log a bogus FREEZE-RISK.
                _guard.emitted = true;
                // Fire and forget: `.check()` was a round trip per notch, and a
                // failure here (a stale timestamp) is not worth taking the WM
                // down through `?` — the X error arrives as `Event::Error`.
                let _ = self.conn.allow_events(Allow::ASYNC_POINTER, e.time);
                return Ok(());
            }
            _guard.emitted = true;
            self.conn.allow_events(Allow::REPLAY_POINTER, e.time)?;
            return Ok(());
        }
        self.last_event_time = e.time;
        // A press is fresh pointer truth just like a motion (see `on_motion`):
        // the pointer is observably here, so a later `Enter` at other
        // coordinates still carries intent while one at these does not.
        self.ptr_truth
            .note(i32::from(e.root_x), i32::from(e.root_y));

        #[cfg(feature = "window-trace")]
        wtrace!(
            "on_button_press root=({},{}) detail={} focused={:?} sel_mon={}",
            e.root_x,
            e.root_y,
            e.detail,
            self.engine
                .state
                .monitors
                .get(self.engine.state.sel_mon)
                .and_then(|m| m.focused),
            self.engine.state.sel_mon
        );

        // Whether the original ButtonPress should be *replayed* to the client
        // (REPLAY_POINTER) or *discarded* (ASYNC_POINTER) when we release the
        // SYNC grab. Normal clicks replay so the app gets the click; the
        // overlay-dismiss path sets this to `false` because the click's only job
        // was to tear down the overlay and focus the pending window — replaying
        // it would re-deliver the press (now that the overlay is gone and nothing
        // is under the cursor) to the root and wrongly unfocus everything.
        let mut replay_event = true;

        #[cfg(feature = "input-trace")]
        {
            let mi = self.engine.state.mon_at(e.root_x as i32, e.root_y as i32);
            let m = &self.engine.state.monitors[mi];
            let hit = self.find_client(e.event);
            itrace!(
                "BP-enter mi={} sel_mon={} mon.focused={:?} x11_input_focus={:?} active_ws={} e.event={:#x} hit_client={:?} e.root=({},{})",
                mi, self.engine.state.sel_mon, m.focused, self.engine.state.x11_input_focus, m.active_index(), e.event, hit, e.root_x, e.root_y
            );
        }

        let mi = self.engine.state.mon_at(e.root_x as i32, e.root_y as i32);
        if mi != self.engine.state.sel_mon {
            if let Some(fw) = self.engine.state.monitors[self.engine.state.sel_mon].focused {
                self.unfocus(fw)?;
            }
            self.engine.state.sel_mon = mi;
        }
        let prev_focused = self.engine.state.monitors[mi].focused;

        // When the focused window is fullscreen, clicking the fullscreen window
        // itself keeps it locked (niri-style). But clicking a *different* tile
        // must work: drop fullscreen on the focused window so the clicked tile
        // becomes usable, then focus it (otherwise the fullscreen column keeps
        // covering everything and the mouse appears dead on the side tiles).
        //
        // `focused_fs` answers "is the focused window fullscreen (covering the
        // screen)?" — it is the *covering* concept, NOT the overlay-owner concept.
        // A `Column` fullscreen is covering but is NOT the `presented_overlay_owner`
        // (Column fullscreen is a ribbon tile, not an overlay), while a
        // `presented_maximize` window IS the overlay owner yet is NOT fullscreen
        // here. Do not substitute `State::presented_overlay_owner` for this, and
        // never read `Client::is_fullscreen()` as "is the overlay owner" — the
        // two semantics are disjoint in the `Column` case.
        let focused_fs = self.engine.state.monitors[mi]
            .focused
            .and_then(|fw| self.engine.state.clients.get(&fw))
            .is_some_and(crate::types::Client::is_fullscreen);

        // A maximized (non-fullscreen) focused window is also presented as an
        // overlay (see `core::present`), so clicking a *different* window must
        // drop that overlay too. Unlike fullscreen, its flags must be explicitly
        // cleared or the window stays announced as maximized in `_NET_WM_STATE`
        // while drawn as a normal tile.
        let focused_present = focused_fs
            || self
                .engine
                .state
                .monitors
                .get(mi)
                .and_then(|m| m.focused)
                .is_some_and(|fw| {
                    self.engine
                        .state
                        .monitors
                        .get(mi)
                        .and_then(|m| m.workspaces.get(m.active_index()))
                        .and_then(|ws| ws.presented_maximize)
                        == Some(fw)
                });

        let client_win = self.find_client(e.event);
        if !focused_present {
            if let Some(cw) = client_win {
                if self.engine.state.monitors[mi].focused != Some(cw) {
                    self.focus(Some(cw))?;
                    // `focus` already refreshes the overlay stacking, so a
                    // separate restack is redundant.
                }
            } else if e.event == self.root {
                self.focus(None)?;
            }
        } else if let Some(fw) = self.engine.state.monitors[mi].focused {
            if let Some(cw) = client_win {
                if fw == cw {
                    // Click landed on the overlay itself (it is on top, so the
                    // event window resolves to `fw`). Normally this keeps the
                    // overlay so the user can interact with the fullscreen app.
                    // EXCEPTION: a window was silently added behind the overlay
                    // while it owned input (`pending_focus`, set in `manage`).
                    // That window is exactly what the user wants to reach, so the
                    // click dismisses the overlay and focuses the deferred
                    // window — otherwise it stays unreachable by pointer for as
                    // long as the overlay is up.
                    let view = self.engine.state.monitors[mi].ws().id;
                    // Only consume the global deferral when it is bound to THIS
                    // monitor/View, was created by the overlay (`fw`) we are
                    // clicking, names a different (still-alive) window, and that
                    // window is still a live client. Otherwise leave it (it
                    // belongs to a different overlay/monitor/View and must
                    // not be orphaned). Compared by `ViewId`, so a deferral still
                    // belongs here after a View is inserted or removed ahead of it.
                    let pending = self.engine.state.pending_focus.filter(|pf| {
                        pf.monitor == mi
                            && pf.workspace == view
                            && pf.owner == fw
                            && pf.window != fw
                            && self.engine.state.clients.contains_key(&pf.window)
                    });
                    if let Some(pf) = pending {
                        let p = pf.window;
                        // Tear down the overlay via the canonical Command funnel.
                        if self
                            .engine
                            .state
                            .clients
                            .get(&fw)
                            .is_some_and(crate::types::Client::is_fullscreen)
                        {
                            // Route through the `ToggleFullscreen` Command
                            // (single funnel) instead of mutating state here.
                            let effects = self
                                .engine
                                .execute(crate::core::commands::ToggleFullscreen(Some(fw)));
                            self.run_effects(effects)?;
                        } else {
                            let effects = self
                                .engine
                                .execute(crate::core::commands::ToggleMaximize(Some(fw)));
                            self.run_effects(effects)?;
                        }
                        // Consume the deferral (its owner overlay is being torn
                        // down) and focus the deferred window through the sink.
                        // `view` was read above, before the overlay was torn down;
                        // its position is resolved first, outside the mutable
                        // borrow `consume_pending_focus` needs.
                        let view_i = self.engine.state.monitors[mi].view_index(view).unwrap_or(0);
                        crate::core::commands::consume_pending_focus(
                            &mut self.engine.state,
                            mi,
                            view_i,
                            Some(fw),
                        );
                        self.focus(Some(p))?;
                        // The click was consumed to tear down the overlay; do not
                        // replay the press (it would re-deliver to the now-empty
                        // spot and unfocus the window we just focused).
                        replay_event = false;
                    }
                    // else: genuine click on the overlay's own content → keep it.
                } else {
                    // Clicking something other than the presented window. Two
                    // cases:
                    //  • A popup/dialog that *belongs* to the presented app — its
                    //    transient chain reaches `fw`. The overlay's popups are
                    //    deliberately raised above the fullscreen layer (see
                    //    `stack_overlay`); dropping the overlay here would break
                    //    that and close the app's own menu/save-dialog. So we
                    //    keep the overlay, just focus the popup so it also
                    //    receives keyboard input. The click is replayed to it.
                    //  • A genuinely different window: drop the overlay so the
                    //    tile becomes usable, then focus it (niri-style "sticky"
                    //    fullscreen/maximize: the window itself never exits on
                    //    click unless it is part of another app). For a maximized
                    //    window this also clears its `MAXIMIZED_*` flags and
                    //    rewrites `_NET_WM_STATE`, keeping EWMH state consistent.
                    if self.transient_of(cw, &[fw]) {
                        self.focus(Some(cw))?;
                    } else if self
                        .engine
                        .state
                        .clients
                        .get(&fw)
                        .is_some_and(crate::types::Client::is_fullscreen)
                    {
                        // Route through the `ToggleFullscreen` Command (single
                        // funnel) instead of mutating state directly here.
                        let effects = self
                            .engine
                            .execute(crate::core::commands::ToggleFullscreen(Some(fw)));
                        self.run_effects(effects)?;
                        self.focus(Some(cw))?;
                    } else {
                        let effects = self
                            .engine
                            .execute(crate::core::commands::ToggleMaximize(Some(fw)));
                        self.run_effects(effects)?;
                        self.focus(Some(cw))?;
                    }
                }
            }
        }

        let mut drag_started = false;

        #[cfg(feature = "input-trace")]
        {
            let m = &self.engine.state.monitors[mi];
            itrace!(
                "BP-after-dispatch mi={} mon.focused={:?} x11_input_focus={:?} focused_fs_was={} drag_started={}",
                mi, m.focused, self.engine.state.x11_input_focus, focused_fs, drag_started
            );
        }

        let sup: u16 = ModMask::M4.into();
        let clean = clean_mask(u16::from(e.state), self.numlock, self.scroll);
        if clean == sup && !focused_fs {
            if let Some(cw) = client_win {
                // Only already-floating windows are draggable (move with
                // Button1, resize with Button3). Tiled windows are managed
                // exclusively by the keyboard (Mod4+Shift+h/l/j/k); a Mod4
                // drag on a tile is a no-op — it can never detach it as a
                // float and a float drag can never drop back into a column.
                if let Some(c) = self.engine.state.clients.get(&cw).filter(|c| c.is_float()) {
                    let geom = c.geom;
                    let is_resize = e.detail == ButtonIndex::M3.into();
                    let grab_ok = self
                        .conn
                        .grab_pointer(
                            false,
                            self.root,
                            EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                            x11rb::NONE,
                            x11rb::NONE,
                            x11rb::CURRENT_TIME,
                        )
                        .ok()
                        .and_then(|cookie| cookie.reply().ok())
                        .is_some_and(|reply| u8::from(reply.status) == 0);

                    if grab_ok {
                        let resize_l =
                            is_resize && (e.root_x as i32) < geom.x + (geom.w as i32) / 2;
                        let resize_t =
                            is_resize && (e.root_y as i32) < geom.y + (geom.h as i32) / 2;
                        self.drag = Some(DragState {
                            win: cw,
                            start_geom: geom,
                            ptr_x: e.root_x as i32,
                            ptr_y: e.root_y as i32,
                            resize: is_resize,
                            resize_l,
                            resize_t,
                        });
                        // The WM claims geometry for the rest of the drag: the
                        // client-authority seal dies here, not on the next
                        // request, because the rect the motion handler computes
                        // is no longer the client's to assert.
                        if let Some(c) = self.engine.state.clients.get_mut(&cw) {
                            c.float_client_authority = false;
                        }
                        drag_started = true;
                    }
                }
            }
        }

        // `prev_focused` / `drag_started` are consumed by the trace and
        // allow_events branches below. The pointer warp after a focus change is
        // deliberately *not* repeated here: `render::focus()` owns it and
        // honours `cfg.warp_cursor`, and a second warp would fight it — each
        // click on a neighbouring tile would move the pointer twice and desync
        // the next click's hit-test, most visibly on floats.
        let _ = prev_focused;
        let _ = drag_started;

        // A drag (active grab) and the overlay-dismiss path consumed the press,
        // so it must be discarded (ASYNC releases the freeze and drops the
        // event); a normal click is replayed so the client receives it.
        #[cfg(feature = "input-trace")]
        {
            _guard.emitted = true;
            itrace!(
                "BP-allow_events EMITTED mode={} drag_started={}",
                if drag_started || !replay_event {
                    "ASYNC"
                } else {
                    "REPLAY"
                },
                drag_started
            );
        }
        #[cfg(not(feature = "input-trace"))]
        {
            _guard.emitted = true;
        }
        self.conn
            .allow_events(
                if drag_started || !replay_event {
                    Allow::ASYNC_POINTER
                } else {
                    Allow::REPLAY_POINTER
                },
                e.time,
            )?
            .check()?;
        Ok(())
    }

    pub(super) fn on_button_release(
        &mut self,
        _e: ButtonReleaseEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Unconditional: the drag grab must be released on every exit path,
        // including non-trace builds (see `SyncGrabGuard`).
        let mut _guard = SyncGrabGuard {
            conn: self.conn.clone(),
            release: GrabRelease::Ungrab,
            emitted: false,
            tag: "on_button_release",
        };
        #[cfg(feature = "input-trace")]
        itrace!("BR-enter drag_active={}", self.drag.is_some());

        if let Some(drag) = self.drag.take() {
            // Explicit ungrab below: mark emitted in every build, or `Drop`
            // double-ungrabs and logs a spurious FREEZE-RISK on each release.
            _guard.emitted = true;
            #[cfg(feature = "input-trace")]
            itrace!("BR-ungrab_pointer EMITTED (drag was active)");
            self.conn.ungrab_pointer(x11rb::CURRENT_TIME)?.check()?;
            // Use the window's actual monitor, not sel_mon (H3).
            // After a hotplug during a drag, sel_mon may be stale.
            let win = drag.win;
            let mi = self
                .engine
                .state
                .clients
                .get(&win)
                .map(|c| c.monitor)
                .filter(|&m| m < self.engine.state.monitors.len())
                .unwrap_or(0);

            // A float drag never changes tiling membership: it stays floating
            // wherever it is released (clamped to the workarea by on_motion).
            // No preview highlight to clear — drag-release never re-tiles.
            self.arrange(mi)?;
            self.sync_window_prefs(win);
        } else {
            // No drag was active: the press path already released the SYNC
            // grab via allow_events, so there is nothing to ungrab. Mark
            // emitted so Drop doesn't fire a spurious ungrab_pointer +
            // FREEZE-RISK log on every plain click release.
            _guard.emitted = true;
        }
        Ok(())
    }

    pub(super) fn on_motion(
        &mut self,
        e: MotionNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // A real pointer movement lifts the keyboard-navigation guard so
        // focus-follows-mouse resumes normally (see on_enter/on_key).
        self.pointer_guard_until = None;
        // Fresh pointer truth for `on_enter`'s stationary check: motion is
        // what moves the pointer, so this is the position a later `Enter`
        // must differ from to carry new intent.
        self.ptr_truth
            .note(i32::from(e.root_x), i32::from(e.root_y));
        let drag_snapshot = self.drag.as_ref().map(|d| {
            (
                d.win,
                d.start_geom,
                d.ptr_x,
                d.ptr_y,
                d.resize,
                d.resize_l,
                d.resize_t,
            )
        });
        if let Some((win, start_geom, ptr_x, ptr_y, resize, resize_l, resize_t)) = drag_snapshot {
            let dx = e.root_x as i32 - ptr_x;
            let dy = e.root_y as i32 - ptr_y;

            // saturating_add in drag coordinates: fast pointer movement on
            // 4K+ high-refresh displays can overflow i32 → panic (debug)
            // or corrupted geometry → BadValue (release).
            let (gx, gy, gw, gh) = if resize {
                // Quadrant-aware resize: when the grab sits over the left/top
                // half, that edge follows the pointer (the window grows
                // against that corner); otherwise the opposite corner stays
                // anchored.
                let mut g = Rect::new(start_geom.x, start_geom.y, start_geom.w, start_geom.h);
                if resize_l {
                    g.x = start_geom.x.saturating_add(dx);
                    g.w = (start_geom.w as i32).saturating_sub(dx).max(1) as u32;
                } else {
                    g.w = (start_geom.w as i32).saturating_add(dx).max(1) as u32;
                }
                if resize_t {
                    g.y = start_geom.y.saturating_add(dy);
                    g.h = (start_geom.h as i32).saturating_sub(dy).max(1) as u32;
                } else {
                    g.h = (start_geom.h as i32).saturating_add(dy).max(1) as u32;
                }
                // Respect `WM_SIZE_HINTS`: clamp to the client's minimum/maximum
                // size and snap to its size increments, so terminals / emacs
                // can't be dragged below their hinted minimum. The hard 1 px
                // floor above only guards against overflow; the real limits come
                // from the hints, shared with the client `ConfigureRequest` path
                // through `snap_float_to_hints` so the two never disagree. With
                // the left/top edge grabbed, the opposite (anchored) corner must
                // stay put after the width/height snap.
                let (mi, bw) = {
                    let c = self.engine.state.clients.get(&win);
                    (
                        c.map(|c| c.monitor)
                            .filter(|&m| m < self.engine.state.monitors.len())
                            .unwrap_or(0),
                        c.map_or(0, |c| c.border_w),
                    )
                };
                let hints = self
                    .engine
                    .state
                    .clients
                    .get(&win)
                    .map(|c| c.hints)
                    .unwrap_or_default();
                g = snap_float_to_hints(g, hints);
                if resize_l {
                    let right = start_geom.x + start_geom.w as i32;
                    g.x = right - g.w as i32;
                }
                if resize_t {
                    let bottom = start_geom.y + start_geom.h as i32;
                    g.y = bottom - g.h as i32;
                }
                // Single normalization (see `layout::normalize_float_geom`):
                // drag-resize shares snap → frame-aware clamp → settle with
                // `arrange` and with `ConfigureRequest`, so the dragged rect is
                // already a fixed point and the next arrange has nothing to
                // correct (no jump). The grabbed-corner re-anchor above is
                // repeated after the settle because settling can shrink `w`/`h`
                // by one grid line: the grabbed corner stays put, the anchored
                // one must not move.
                let wa = self.engine.state.monitors[mi].workarea;
                g = clamp_float_to_workarea(g, wa, bw);
                g = crate::core::layout::settle_to_grid(g, hints);
                if resize_l {
                    let right = start_geom.x + start_geom.w as i32;
                    g.x = right - g.w as i32;
                }
                if resize_t {
                    let bottom = start_geom.y + start_geom.h as i32;
                    g.y = bottom - g.h as i32;
                }
                g = clamp_float_to_workarea(g, wa, bw);
                (g.x, g.y, g.w, g.h)
            } else {
                (
                    start_geom.x.saturating_add(dx),
                    start_geom.y.saturating_add(dy),
                    start_geom.w,
                    start_geom.h,
                )
            };

            // Route the drag through the `MoveResize` Command so the float
            // geometry state mutation lives in the core funnel; the emitted
            // `Effect::ConfigureWindow` is carried out by the reconciler's
            // `apply_geom` (the single owner of `configure_window`).
            let rect = Rect::new(gx, gy, gw, gh);
            let effects = self
                .engine
                .execute(crate::core::commands::MoveResize(win, rect));
            self.run_effects(effects)?;
        } else if self.engine.cfg.focus_mouse {
            // Focus-follows-mouse is handled via on_enter (EnterNotify)
            // to avoid an X11 query_tree round-trip on every motion event.
        }
        Ok(())
    }

    /// Mod4 + scroll wheel: drive the column-ribbon camera. We don't free-scroll
    /// the raw camera (that would leave it between columns, breaking the
    /// accordion target); instead we step the *focused column* one slot per
    /// notch, which follows the selection through the same overview-aware
    /// scroll policy as the keybinding — exactly like `OverviewNav`, just
    /// continuous.
    ///
    /// This only *records* the notch. A wheel delivers notches faster than a
    /// focus change can be applied (each is X requests plus a re-arrange), and
    /// the pointer is frozen until every one is handled, so applying them inline
    /// made the backlog grow for as long as the wheel spun. `flush_pending`
    /// applies whatever accumulated, once per turn, in `apply_wheel_steps`.
    fn scroll_camera_with_wheel(&mut self, detail: u8) -> Result<(), Box<dyn std::error::Error>> {
        // Bound the queue: past this, extra notches in a single turn add nothing
        // a user could perceive and only lengthen the replay.
        const MAX_STEPS_PER_TURN: usize = 256;
        let dir = match detail {
            7 | 5 => Dir::Right, // wheel right / down → next column
            _ => Dir::Left,      // wheel left / up → previous column (and any other)
        };
        if self.wheel_steps.len() < MAX_STEPS_PER_TURN {
            self.wheel_steps.push(dir);
        }
        Ok(())
    }

    /// Apply the notches queued by [`Self::scroll_camera_with_wheel`].
    ///
    /// Each step runs through the same `FocusDir` command as the keybinding, so
    /// the resulting state is exactly what N sequential notches produce. Only the
    /// *effects* are squeezed: intermediate columns are never visibly focused, so
    /// their `FocusWindow`/`Unfocus` are dropped — the first `Unfocus` (the
    /// window that had focus) and the last `FocusWindow` (where it ended up) are
    /// kept — and the arrange marks are idempotent, so they collapse into one
    /// pass.
    pub(super) fn apply_wheel_steps(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.wheel_steps.is_empty() {
            return Ok(());
        }
        let steps = std::mem::take(&mut self.wheel_steps);
        let mut first_unfocus = None;
        let mut last_focus = None;
        let mut rest = Vec::new();
        for dir in steps {
            for eff in self.engine.dispatch(crate::types::Action::FocusDir(dir)) {
                match eff {
                    Effect::Unfocus(w) => {
                        first_unfocus.get_or_insert(w);
                    }
                    Effect::FocusWindow(w) => last_focus = Some(w),
                    other => rest.push(other),
                }
            }
        }
        let mut effects = Vec::with_capacity(rest.len() + 2);
        effects.extend(first_unfocus.map(Effect::Unfocus));
        effects.extend(rest);
        effects.extend(last_focus.map(Effect::FocusWindow));
        self.run_effects(effects)
    }
}

#[cfg(test)]
mod tests {
    use super::PointerTruth;

    // Drive a scripted `(observation, enter)` sequence through the same two
    // operations the handlers perform — `note` on `MotionNotify`/`ButtonPress`
    // (`on_motion`/`on_button_press`), `enter_carries_intent` on `EnterNotify`
    // (`on_enter`) — and return the intent verdict per `Enter`.
    fn run(script: &[(char, i16, i16)]) -> Vec<bool> {
        let mut truth = PointerTruth::default();
        let mut out = Vec::new();
        for &(kind, x, y) in script {
            match kind {
                'm' | 'p' => truth.note(i32::from(x), i32::from(y)),
                'e' => out.push(truth.enter_carries_intent(x, y)),
                _ => panic!("unknown script step {kind}"),
            }
        }
        out
    }

    // 1. A stationary pointer while geometry produces window entries: every
    // one of them is intent-free, so focus can never walk the ribbon on its
    // own no matter how many pans in a row slide tiles underneath.
    #[test]
    fn stationary_pointer_geometry_enters_carry_no_intent() {
        // Parked by a motion, then five entries at the parked position (five
        // pans, five tiles sliding under the cursor).
        assert_eq!(
            run(&[
                ('m', 900, 500),
                ('e', 900, 500),
                ('e', 900, 500),
                ('e', 900, 500),
                ('e', 900, 500),
                ('e', 900, 500),
            ]),
            [false, false, false, false, false]
        );
        // Same when the parked position was established by a press instead.
        assert_eq!(
            run(&[('p', 900, 500), ('e', 900, 500), ('e', 900, 500)]),
            [false, false]
        );
    }

    // 2. A real movement from one window to another carries intent: the entry
    // reports coordinates the pointer was never observed at, because the last
    // motion sample sits just before the window boundary.
    #[test]
    fn real_movement_to_another_window_carries_intent() {
        assert_eq!(
            run(&[
                ('m', 100, 500),
                ('m', 300, 500),
                ('m', 590, 500),
                ('e', 610, 500),
            ]),
            [true]
        );
    }

    // 3. Motion without click keeps the filter's position fresh: after motion
    // to a new spot, an entry at the *old* spot still carries intent (the
    // pointer really was there last), while an entry at the new spot does
    // not — the record followed the pointer without any button involved.
    #[test]
    fn motion_without_click_refreshes_the_recorded_position() {
        assert_eq!(
            run(&[
                ('m', 100, 500),
                ('e', 100, 500),
                ('m', 700, 500),
                ('e', 700, 500),
                ('e', 100, 500),
            ]),
            // Enter at the parked spot: no intent. Enter back at the old
            // spot: the pointer is not there either — but it is also not
            // where it was last seen, and only the no-intent case may be
            // suppressed, so this stays intent (a stale suppression here
            // would swallow a legitimate revisit).
            [false, false, true]
        );
    }

    // 4. Click and focus: a press records the position exactly like a motion,
    // so the press path (`on_button_press`, which focuses through its own
    // logic) owns the activation while the trailing `Enter` at the same spot
    // stays quiet instead of refocusing.
    #[test]
    fn press_records_truth_and_quietens_the_trailing_enter() {
        assert_eq!(run(&[('p', 400, 300), ('e', 400, 300)]), [false]);
        // ... while an entry anywhere else still carries intent.
        assert_eq!(run(&[('p', 400, 300), ('e', 401, 300)]), [true]);
    }

    // 5. Map/unmap with a stationary pointer: entries at the parked position
    // are intent-free, so appearing or disappearing clients never steal focus
    // through this path — that decision belongs to `manage`/`unmanage`.
    #[test]
    fn map_and_unmap_under_a_stationary_pointer_carry_no_intent() {
        assert_eq!(
            run(&[
                ('m', 960, 540),
                ('e', 960, 540),
                ('e', 960, 540),
                ('m', 961, 540),
                ('e', 960, 540),
            ]),
            // The first two entries coincide with the parked pointer.
            // The 1 px nudge is a real movement, so the entry back at the old
            // spot carries intent again.
            [false, false, true]
        );
    }

    // 6. The tracker holds no mode, grab or scale state: entering, navigating
    // and leaving Overview cannot leave a stale filter behind, because there
    // is nothing overview-scoped to go stale — only the last observed
    // position, which the next motion or press refreshes.
    #[test]
    fn tracker_holds_no_overview_state() {
        let mut truth = PointerTruth::default();
        truth.note(200, 200);
        assert!(!truth.enter_carries_intent(200, 200));
        // A motion after the mode is gone refreshes the record as usual.
        truth.note(800, 200);
        assert!(!truth.enter_carries_intent(800, 200));
        assert!(truth.enter_carries_intent(200, 200));
    }

    // Before the very first observation there is no position to coincide
    // with, so nothing is suppressed: a parked pointer the WM never saw must
    // not start by dropping entries.
    #[test]
    fn no_observation_suppresses_nothing() {
        assert_eq!(run(&[('e', 10, 10)]), [true]);
    }
}
