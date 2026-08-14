use super::render::clamp_float_to_workarea;
use super::*;

// ── input-trace instrumentation (feature `input-trace`) ───────────────────────
// `itrace!` is a no-op unless the `input-trace` feature is on, so the call sites
// below can stay in the code without any runtime cost in normal builds.
#[cfg(feature = "input-trace")]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{
        eprintln!("[INPUT-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "input-trace"))]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{}};
}

// ── window-trace instrumentation (feature `window-trace`) ─────────────────────
#[cfg(feature = "window-trace")]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "window-trace"))]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{}};
}

// Drop-guard that GUARANTEES the SYNC pointer grab is always released. The
// per-window `grab_button(..., GrabMode::SYNC, ...)` freezes the pointer on every
// ButtonPress until `allow_events` runs; an active drag grab freezes it until
// `ungrab_pointer` runs. If the handler returns via `?` before reaching those
// calls, the pointer stays frozen on the *server* side — a global input freeze
// that survives workspace/window changes. The guard holds a clone of the X
// connection and performs the release itself on *every* exit path (including
// errors), so the freeze can never happen regardless of the focus logic above.
enum GrabRelease {
    AllowReplay(u32),
    Ungrab,
}

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
        // A code path returned before releasing the SYNC pointer/active grab,
        // which would have frozen the pointer on the X server. We auto-released
        // it to avoid a global input freeze. Log it (always — it indicates a
        // focus/dispatch error worth surfacing) so the early-return site can be
        // found and fixed.
        eprintln!(
            "[INPUT-TRACE] FREEZE-RISK: {} exited WITHOUT releasing the SYNC pointer grab — auto-released on drop (pointer was about to freeze)",
            self.tag
        );
    }
}

#[derive(Debug)]
pub(super) struct DragState {
    pub(super) win: Window,
    pub(super) start_geom: Rect,
    pub(super) ptr_x: i32,
    pub(super) ptr_y: i32,
    pub(super) resize: bool,
    /// Grip handed: which corner the resize grows toward. True means the
    /// pointer grabbed the left/top edge, so width/height grow against it.
    pub(super) resize_l: bool,
    pub(super) resize_t: bool,
    /// Whether the pointer actually travelled (≥4px) — distinguishes a click
    /// from a drag, so only real drags can drop into a column.
    pub(super) moved: bool,
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

        // Scroll buttons (4=up,5=down,6=left,7=right). With no modifier they are
        // just delivered to the application (REPLAY_POINTER). With Mod4 held they
        // scroll the camera of the scroll (niri-style) ribbon layout left/right
        // (and, in Overview, also vertically) — the characteristic interaction of
        // this WM that was previously unreachable (bug C9).
        if e.detail >= 4 {
            let sup: u16 = ModMask::M4.into();
            let clean = clean_mask(u16::from(e.state), self.numlock);
            if clean == sup {
                self.scroll_camera_with_wheel(e.detail, e.root_x as i32, e.root_y as i32)?;
                return Ok(());
            }
            _guard.emitted = true;
            self.conn.allow_events(Allow::REPLAY_POINTER, e.time)?;
            return Ok(());
        }
        self.last_event_time = e.time;

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
                mi, self.engine.state.sel_mon, m.focused, self.engine.state.x11_input_focus, m.active_ws, e.event, hit, e.root_x, e.root_y
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
        // cleared or the window stays announced as maximized while drawn as a
        // normal tile (bug B3) — which also made `Mod4+M` toggle it back up.
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
                        .and_then(|m| m.workspaces.get(m.active_ws))
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
                if fw != cw {
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
                    //    rewrites `_NET_WM_STATE`, keeping EWMH state consistent
                    //    (bug B3).
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
                } else {
                    // Click landed on the overlay itself (it is on top, so the
                    // event window resolves to `fw`). Normally this keeps the
                    // overlay so the user can interact with the fullscreen app.
                    // EXCEPTION: a window was silently added behind the overlay
                    // while it owned input (`pending_focus`, set in `manage`).
                    // That window is exactly what the user wants to reach, so the
                    // click dismisses the overlay and focuses the pending window
                    // — without this, B is unreachable by pointer while A stays
                    // fullscreen (the reported pointer-loss bug).
                    let ws_i = self.engine.state.monitors[mi].active_ws;
                    // Only consume the global deferral when it is bound to THIS
                    // monitor/workspace, was created by the overlay (`fw`) we are
                    // clicking, names a different (still-alive) window, and that
                    // window is still a live client. Otherwise leave it (it
                    // belongs to a different overlay/monitor/workspace and must
                    // not be orphaned).
                    let pending = self.engine.state.pending_focus.filter(|pf| {
                        pf.monitor == mi
                            && pf.workspace == ws_i
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
                        crate::core::commands::consume_pending_focus(
                            &mut self.engine.state,
                            mi,
                            ws_i,
                            Some(fw),
                        );
                        self.focus(Some(p))?;
                        // The click was consumed to tear down the overlay; do not
                        // replay the press (it would re-deliver to the now-empty
                        // spot and unfocus the window we just focused).
                        replay_event = false;
                    }
                    // else: genuine click on the overlay's own content → keep it.
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
        let clean = clean_mask(u16::from(e.state), self.numlock);
        if clean == sup && !focused_fs {
            if let Some(cw) = client_win {
                if let Some(c) = self.engine.state.clients.get(&cw) {
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
                            moved: false,
                        });
                        drag_started = true;
                    }
                }
            }
        }

        // After a click that *changes* focus, warp the pointer onto the newly
        // focused window. `focus()` just recentered the camera, so the clicked
        // window now sits somewhere else on screen; without the warp the next
        // click (at the same spot) would land on whatever scrolled under the
        // cursor — i.e. the *previous* window ("clicks act on the old window").
        // Skipped while a Mod4 drag is starting, so grabs keep working.
        if !drag_started {
            let new_focused = self.engine.state.monitors[mi].focused;
            if new_focused != prev_focused {
                if let Some(fw) = new_focused {
                    if let Some(c) = self.engine.state.clients.get(&fw) {
                        let g = c.geom;
                        let _ = self.conn.warp_pointer(
                            x11rb::NONE,
                            fw,
                            0,
                            0,
                            0,
                            0,
                            (g.w / 2) as i16,
                            (g.h / 2) as i16,
                        );
                    }
                }
            }
        }

        // REPLAY_POINTER: re-delivers the click to the application so popups,
        //   context menus and dialogs open normally.
        // ASYNC_POINTER (drag, or the overlay-dismiss path): releases the
        //   passive-grab freeze and discards the event — used when the press was
        //   consumed to tear down the overlay rather than delivered to a client.
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
        e: ButtonReleaseEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Unconditional: guarantees the active drag grab is released on every exit
        // path (see `SyncGrabGuard`), so a failed `focus`/`arrange` inside the
        // drag-drop handler can never strand the pointer/keyboard grabbed.
        let mut _guard = SyncGrabGuard {
            conn: self.conn.clone(),
            release: GrabRelease::Ungrab,
            emitted: false,
            tag: "on_button_release",
        };
        #[cfg(feature = "input-trace")]
        itrace!("BR-enter drag_active={}", self.drag.is_some());

        if let Some(drag) = self.drag.take() {
            #[cfg(feature = "input-trace")]
            {
                _guard.emitted = true;
                itrace!("BR-ungrab_pointer EMITTED (drag was active)");
            }
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

            // Clear any tile-insertion preview highlight left on the last
            // hovered window.
            if let Some(prev) = self.drag_target.take() {
                let _ = self.conn.change_window_attributes(
                    prev,
                    &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                        .border_pixel(self.engine.cfg.col_normal),
                );
            }

            // Drop-to-tile: a *real move* released over a tiled window inserts
            // the dragged window back into the tiling tree at that column and
            // row (dropping over empty space still leaves it floating).
            if drag.moved && !drag.resize {
                let (rx, ry) = (e.root_x as i32, e.root_y as i32);
                if let Some(target) = self.drop_candidate(win, mi, rx, ry) {
                    let ws_i = self.engine.state.monitors[mi].active_ws;
                    let col_idx = {
                        let ws = &self.engine.state.monitors[mi].workspaces[ws_i];
                        ws.columns
                            .iter()
                            .position(|col| col.windows.contains(&target))
                    };
                    if let Some(ci) = col_idx {
                        // Insert at the row whose windows sit above the pointer
                        // (count of windows with center above ry).
                        let insert_pos = {
                            let ws = &self.engine.state.monitors[mi].workspaces[ws_i];
                            ws.columns[ci]
                                .windows
                                .iter()
                                .take_while(|&&w| {
                                    self.engine
                                        .state
                                        .clients
                                        .get(&w)
                                        .is_some_and(|c| c.geom.y + c.geom.h as i32 / 2 < ry)
                                })
                                .count()
                        };
                        {
                            let ws = &mut self.engine.state.monitors[mi].workspaces[ws_i];
                            // Remove from its current place (floats or a column),
                            // then drop into the target column as its focused row.
                            ws.remove_window(win);
                            ws.drop_into_column(ci, win, insert_pos);
                        }
                        if let Some(c) = self.engine.state.clients.get_mut(&win) {
                            c.flags.clear(WinFlags::FLOAT);
                        }
                        self.stack_dirty = true;
                        self.arrange(mi)?;
                        self.focus(Some(win))?;
                        self.sync_window_prefs(win);
                        return Ok(());
                    }
                }
            }

            // Not dropped into a tile: keep it floating. If on_motion set the
            // FLOAT flag but the window is still in a column, promote it to
            // ws.floats now so arrange() treats it as a float and doesn't
            // retile it back to its column position.
            let is_float = self
                .engine
                .state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_float);
            if is_float {
                let ws_i = self.engine.state.monitors[mi].active_ws;
                let in_floats = self.engine.state.monitors[mi].workspaces[ws_i]
                    .floats
                    .contains(&win);
                if !in_floats {
                    // P3: mutate in-place, no clone
                    self.engine.state.monitors[mi].workspaces[ws_i].remove_window(win);
                    self.engine.state.monitors[mi].workspaces[ws_i]
                        .floats
                        .push(win);
                    self.stack_dirty = true;
                }
            }

            self.arrange(mi)?;
            self.sync_window_prefs(win);
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
                // Respect `WM_SIZE_HINTS` (bug B5): clamp to the client's
                // minimum size and snap to its size increments, so terminals /
                // emacs can't be dragged below their hinted minimum. The hard
                // `1px` floor above only guards against overflow; real limits
                // come from the hints. When the left/top edge is the grabbed
                // one, the opposite (anchored) corner must stay put after the
                // width/height snap.
                let (mi, bw) = {
                    let c = self.engine.state.clients.get(&win);
                    (
                        c.map(|c| c.monitor)
                            .filter(|&m| m < self.engine.state.monitors.len())
                            .unwrap_or(0),
                        c.map(|c| c.border_w).unwrap_or(0),
                    )
                };
                let hints = self
                    .engine
                    .state
                    .clients
                    .get(&win)
                    .map(|c| c.hints)
                    .unwrap_or_default();
                let min_w = if hints.min_w > 0 {
                    hints.min_w as u32
                } else {
                    1
                };
                let min_h = if hints.min_h > 0 {
                    hints.min_h as u32
                } else {
                    1
                };
                g.w = g.w.max(min_w);
                g.h = g.h.max(min_h);
                if hints.inc_w > 0 {
                    let base = hints.base_w.max(0);
                    let n = ((g.w as i32 - base).max(0) + hints.inc_w / 2) / hints.inc_w;
                    g.w = (base + n * hints.inc_w).max(0) as u32;
                }
                if hints.inc_h > 0 {
                    let base = hints.base_h.max(0);
                    let n = ((g.h as i32 - base).max(0) + hints.inc_h / 2) / hints.inc_h;
                    g.h = (base + n * hints.inc_h).max(0) as u32;
                }
                if resize_l {
                    let right = start_geom.x + start_geom.w as i32;
                    g.x = right - g.w as i32;
                }
                if resize_t {
                    let bottom = start_geom.y + start_geom.h as i32;
                    g.y = bottom - g.h as i32;
                }
                // Keep the drag inside the monitor workarea (bug B5): floats are
                // clamped elsewhere via `clamp_float_to_workarea`, but the drag
                // path skipped it and could push the window off-screen.
                let wa = self.engine.state.monitors[mi].workarea;
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

            if let Some(drag) = &mut self.drag {
                if gx != drag.start_geom.x || gy != drag.start_geom.y {
                    drag.moved = true;
                }
            }
            // Route the drag through the `MoveResize` Command so the float
            // geometry state mutation lives in the core funnel; the emitted
            // `Effect::ConfigureWindow` is carried out by the reconciler's
            // `apply_geom` (the single owner of `configure_window`).
            let rect = Rect::new(gx, gy, gw, gh);
            let effects = self
                .engine
                .execute(crate::core::commands::MoveResize(win, rect));
            self.run_effects(effects)?;

            // Tile-insertion preview: while *moving*, highlight the tiled
            // window under the pointer so the user sees where release would
            // insert the window. Resize drags skip the preview.
            if !resize {
                let mi = self.engine.state.mon_at(e.root_x as i32, e.root_y as i32);
                self.preview_drop_target(win, mi, e.root_x as i32, e.root_y as i32);
            }
        } else if self.engine.cfg.focus_mouse {
            // Focus-follows-mouse is handled via on_enter (EnterNotify)
            // to avoid an X11 query_tree round-trip on every motion event.
        }
        Ok(())
    }

    /// The tiled window under `(px, py)` that a dropped float would join, if
    /// any. Ignores floats and overlay-presented (fullscreen/maximized) ones.
    fn drop_candidate(&self, drag_win: Window, mi: usize, px: i32, py: i32) -> Option<Window> {
        let state = &self.engine.state;
        let mon = state.monitors.get(mi)?;
        let ws = mon.workspaces.get(mon.active_ws)?;
        for col in &ws.columns {
            for &w in &col.windows {
                if w == drag_win {
                    continue;
                }
                let Some(c) = state.clients.get(&w) else {
                    continue;
                };
                if c.is_fullscreen() || ws.presented_maximize == Some(w) {
                    continue;
                }
                let g = &c.geom;
                if px >= g.x && px < g.x + g.w as i32 && py >= g.y && py < g.y + g.h as i32 {
                    return Some(w);
                }
            }
        }
        None
    }

    /// Highlight (or clear) the tile-insertion preview while a move drag hovers
    /// a tiled window. Reverts the previously highlighted tile first.
    fn preview_drop_target(&mut self, drag_win: Window, mi: usize, px: i32, py: i32) {
        let target = self.drop_candidate(drag_win, mi, px, py);
        if target == self.drag_target {
            return;
        }
        if let Some(prev) = self.drag_target.take() {
            let _ = self.conn.change_window_attributes(
                prev,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .border_pixel(self.engine.cfg.col_normal),
            );
        }
        if let Some(t) = target {
            let _ = self.conn.change_window_attributes(
                t,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .border_pixel(self.engine.cfg.col_focused),
            );
            self.drag_target = Some(t);
        }
    }

    /// Mod4 + scroll wheel: drive the column-ribbon camera. We don't free-scroll
    /// the raw camera (that would leave it between columns, breaking the
    /// accordion target); instead we step the *focused
    /// column* one slot per notch, which recenters the camera via `ideal_scroll`
    /// — exactly like `OverviewNav`, just continuous. Mod4+wheel is the
    /// characteristic interaction of a scroll WM that was previously unreachable
    /// (bug C9).
    fn scroll_camera_with_wheel(
        &mut self,
        detail: u8,
        px: i32,
        py: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = match detail {
            7 | 5 => Dir::Right, // wheel right / down → next column
            _ => Dir::Left,      // wheel left / up → previous column (and any other)
        };
        // Reuse the existing focus-movement command so behaviour (row carry,
        // camera recenter, events) stays identical to the keybinding path.
        let effects = self.engine.dispatch(crate::types::Action::FocusDir(dir));
        self.run_effects(effects)?;
        // Keep the column under the pointer focused so the gesture and the
        // keyboard agree on what is selected.
        self.focus_column_at(px, py);
        Ok(())
    }

    /// Focus the tiled column whose screen rect contains `(px, py)` on `mon`,
    /// used by wheel-scroll so focus tracks the gesture. No-op if the point is
    /// not over any tiled column (floats/empty space don't steal focus this way).
    fn focus_column_at(&mut self, px: i32, py: i32) {
        let mi = self.engine.state.mon_at(px, py);
        if mi >= self.engine.state.monitors.len() {
            return;
        }
        let (ws_i, wa) = {
            let m = &self.engine.state.monitors[mi];
            (m.active_ws, m.workarea)
        };
        let fs = crate::core::layout::fs_ctx(
            &self.engine.state.clients,
            &self.engine.state.monitors[mi].workspaces[ws_i],
            self.engine.state.monitors[mi].screen,
        );
        let extents = crate::core::layout::column_screen_extents(
            &self.engine.state.monitors[mi].workspaces[ws_i],
            &self.engine.cfg,
            wa,
            &fs,
        );
        let vis_l = wa.x as f32;
        let vis_r = (wa.x + wa.w as i32) as f32;
        let col = extents.iter().position(|&(l, r)| {
            let l = l.max(vis_l);
            let r = r.min(vis_r);
            r > l && (px as f32) >= l && (px as f32) <= r
        });
        if let Some(ci) = col {
            self.engine.state.monitors[mi].workspaces[ws_i]
                .focus
                .column_idx = ci;
            if let Some(w) =
                self.engine.state.monitors[mi].workspaces[ws_i].columns[ci].focused_win()
            {
                let _ = self.focus(Some(w));
            }
        }
    }
}
