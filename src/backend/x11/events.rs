//! X11 event handlers dispatched from `mod.rs::dispatch`.
//!
//! Each function handles one X11 event type and translates it into a `Command`
//! (executed by `Engine`) or a direct side effect (compositor track/damage,
//! EWMH update).
//!
//! # Ownership & lifecycle
//!
//! `WindowManager::dispatch` owns the event queue; each `on_*` borrows
//! `&mut self` and may mutate `State`, `AppliedState`, focus, or the compositor.
//! No handler retains a borrow across the X round-trip — cookies are collected
//! before any reply-driven branch.
//!
//! # Event flow
//!
//! ```text
//! X11 event → dispatch → events::<handler>
//!     → Command::execute (state mutation)
//!     → Effect (backend execution)
//!     → render::arrange_full (re-apply geometry)
//! ```
//!
//! # Important semantics
//!
//! - **`ConfigureRequest`** — drag authority (I1/I2): during a drag the WM
//!   re-asserts its geometry; tiled windows ignore client requests (model A);
//!   floats normalize + adopt via `normalize_float_request`.
//! - **`ConfigureNotify`** — convergence step 3: compares the client's reported
//!   rect against `AppliedState` via `classify_configure`. Floats adopt
//!   (follow); tiled re-asserts the desired rect.
//! - **`MapRequest`** — manages the window (creates `Client`, applies rules).
//! - **DestroyNotify/UnmapNotify** — unmanages, cleans compositor texture,
//!   refocuses; the synthetic `SendEvent` `ConfigureNotify` is discarded
//!   (`response_type & 0x80`).
//! - **`ClientMessage`** — `_NET_WM_STATE` fullscreen/maximize via
//!   `FullscreenPolicy::Deny` + `ToggleFullscreen` funnel; `_NET_ACTIVE_WINDOW`
//!   via `decide_active_window`; `_NET_CLOSE_WINDOW` → kill.
//! - **`KeyPress`** — updates `last_event_time`, `clean_mask`, `keysym_at_col`
//!   (group 0/shifted), `resolve_binding` (group 1 + fallback), rate-limits
//!   via `last_key_times`, then `do_action` and arms `pointer_guard_until`.
//! - **`EnterNotify`** — focus-follows-mouse guard (50 ms after key to avoid
//!   fighting keyboard focus).
//! - **MappingNotify/XkbMapNotify/NewKeyboardNotify** — coalesced 50 ms keyboard
//!   refresh window (`KBD_REFRESH_DELAY`).
//!
//! # Invariants
//!
//! - Synthetic `SendEvent` `ConfigureNotifies` are discarded.
//! - `Inferior`/`POINTER` focus events and non-`NORMAL` grab modes are ignored
//!   (INV-C).
//! - `last_event_time` is updated on every key/pointer event so
//!   `WM_TAKE_FOCUS` timestamps are monotonic and never `CurrentTime`.
//!
//! # Safety
//!
//! No `unsafe` in this module; all X11 interaction goes through `x11rb` safe
//! wrappers over the shared `Rc<XConn>` (see `maverick_x11::open_x` for the
//! `Display*`/`xcb_connection_t` invariant).

use super::render::adopt_float_request;

use super::*;
#[cfg(feature = "compositor-opengl")]
use x11rb::protocol::damage::NotifyEvent as DamageNotifyEvent;

impl WindowManager {
    pub(super) fn on_map_request(
        &mut self,
        e: MapRequestEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let attrs = match self.conn.get_window_attributes(e.window)?.reply() {
            Ok(a) => a,
            Err(err) => {
                log::debug!("Failed to get attributes for window {}: {}", e.window, err);
                return Ok(());
            }
        };
        if !attrs.override_redirect && !self.engine.state.clients.contains_key(&e.window) {
            if let Err(err) = self.manage(e.window, &attrs) {
                log::warn!(
                    "Failed to manage window {} on map request: {}",
                    e.window,
                    err
                );
            }
        }
        Ok(())
    }

    pub(super) fn on_destroy(
        &mut self,
        e: DestroyNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.engine.state.clients.contains_key(&e.window) {
            self.unmanage(e.window, true)?;
        }
        // Drop any dock reservation the (unmanaged) window held.
        if self.docks.contains_key(&e.window) {
            self.remove_dock(e.window)?;
        }
        if let Some(c) = self.compositor.as_mut() {
            c.on_destroy(e.window);
        }
        Ok(())
    }

    pub(super) fn on_unmap(
        &mut self,
        e: UnmapNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Drop the window's texture the moment it unmaps, whatever the cause —
        // its off-screen pixmap is gone and the compositor must not draw stale
        // (or freed) contents. `c.on_unmap` is a no-op for untracked windows.
        if let Some(c) = self.compositor.as_mut() {
            c.on_unmap(e.window);
        }
        // Drop the duplicate `UnmapNotify` the X server delivers to the root
        // (SubstructureNotify) for an unmap it forwards on behalf of the client:
        // only the variant targeted at the window itself (`e.event == e.window`)
        // reflects a real client unmap, and the root-targeted copy is just the
        // server's own broadcast of the same event. The WM never unmaps managed
        // windows itself today (it culls off-screen ones via ConfigureNotify),
        // so there is no self-unmap to ignore here.
        if e.event == self.root {
            return Ok(());
        }

        if !self.engine.state.clients.contains_key(&e.window) {
            if self.docks.contains_key(&e.window) {
                self.remove_dock(e.window)?;
            }
            return Ok(());
        }
        // A fullscreen/maximized window that unmapped is relinquishing the
        // presentation overlay (quit/crash/withdraw). Purge it from the tree
        // now — not later on DestroyNotify — so the overlay stack (`present`,
        // `stack_overlay`) can never raise a stale WindowId and hit BadWindow.
        // For normal (tiled/floating) windows, we also unmanage on unmap to
        // prevent "dead" unmapped windows leaving holes in the layout. The
        // destroyed=false path restores border + WM_STATE + ungrab, then
        // re-arranges and focuses the next best window.
        self.unmanage(e.window, false)
    }

    pub(super) fn on_configure_request(
        &mut self,
        e: ConfigureRequestEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // I1/I2: while a float is being dragged, WM drag geometry has exclusive
        // authority — client ConfigureRequest must not change x/y/width/height.
        if self.drag.as_ref().is_some_and(|d| d.win == e.window) {
            if let Some(client) = self.engine.state.clients.get(&e.window) {
                let geom = client.geom;
                let bw = client.border_w;
                let ev = ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: e.window,
                    window: e.window,
                    above_sibling: x11rb::NONE,
                    x: geom.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    y: geom.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    width: geom.w.clamp(1, u16::MAX as u32) as u16,
                    height: geom.h.clamp(1, u16::MAX as u32) as u16,
                    border_width: bw.clamp(0, u16::MAX as u32) as u16,
                    override_redirect: false,
                };
                let _ = self
                    .conn
                    .send_event(false, e.window, EventMask::STRUCTURE_NOTIFY, ev);
            }
            return Ok(());
        }
        if let Some(client) = self.engine.state.clients.get(&e.window) {
            // WM authority (tiled AND fullscreen): the client's request is
            // ignored and we re-assert the Desired rect. A fullscreen window's
            // Desired is the fullscreen rect the WM computed, so honouring a
            // client `ConfigureRequest` would let a game/browser shrink the
            // overlay — exactly the divergence `classify_configure(follow=false)`
            // already rejects for `ConfigureNotify`. Both request paths now agree
            // the WM owns tiled/fullscreen geometry (model A: request ignored).
            if !client.is_float() {
                let geom = client.geom;
                let bw = client.border_w;
                let ev = ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: e.window,
                    window: e.window,
                    above_sibling: x11rb::NONE,
                    x: geom.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    y: geom.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    width: geom.w.clamp(1, u16::MAX as u32) as u16,
                    height: geom.h.clamp(1, u16::MAX as u32) as u16,
                    border_width: bw.clamp(0, u16::MAX as u32) as u16,
                    override_redirect: false,
                };
                let _ = self
                    .conn
                    .send_event(false, e.window, EventMask::STRUCTURE_NOTIFY, ev);
                return Ok(());
            }
        }
        // Floating managed client: the *client* owns its geometry, so the request
        // is adopted verbatim (see `adopt_float_request`) — no hint snap, no
        // workarea clamp. That is the whole stability contract: the answer is
        // bit-for-bit what the client asked for, so a toolkit that re-asserts
        // "its" rect whenever a `ConfigureNotify` disagrees with it has nothing
        // left to correct and the conversation ends on the first request. Every
        // transformation the WM added here (snap to the client's increment grid,
        // clamp into the workarea) can only make the answer differ from the
        // request, and a differing answer is a fight — measured as a window that
        // jumps between two geometries with the WM burning a core.
        //
        // The per-field writes start from the current `client.geom`/`border_w`
        // and override only the masked fields: a lone `XResizeWindow` therefore
        // resizes in place instead of dragging the window to (0, 0), and a
        // request with no position keeps the placement the WM or the user chose.
        // (Stacking bits are dropped — float restacks are owned by
        // `arrange`/`stack_overlay`.)
        if let Some(client) = self.engine.state.clients.get(&e.window) {
            let mut geom = client.geom;
            let mut bw = client.border_w;
            let hidden = client.wm_hidden;
            if e.value_mask.contains(ConfigWindow::X) {
                geom.x = e.x as i32;
            }
            if e.value_mask.contains(ConfigWindow::Y) {
                geom.y = e.y as i32;
            }
            if e.value_mask.contains(ConfigWindow::WIDTH) {
                geom.w = e.width as u32;
            }
            if e.value_mask.contains(ConfigWindow::HEIGHT) {
                geom.h = e.height as u32;
            }
            if e.value_mask.contains(ConfigWindow::BORDER_WIDTH) {
                bw = e.border_width as u32;
            }
            let geom = adopt_float_request(geom);
            // Autoridad: este rect lo pidio el cliente y el WM lo adopto
            // verbatim. Sellar al cliente como autoridad de su flotante para
            // que el proximo `arrange` lo proyecte tal cual (solo sanidad de
            // protocolo) en vez de re-normalizarlo y reabrir el ping-pong
            // (cliente re-pide → WM re-escribe: el flotante "salta solo").
            // El sello se limpia cuando el WM vuelve a decidir la geometria
            // (drag, reglas, `ToggleFloat`, cambio de workarea/monitor — ver
            // `layout::settle_float_in_workarea`).
            if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                c.float_client_authority = true;
            }
            if hidden {
                // Parked off-screen because its workspace is not the active one:
                // record the new logical rect so it comes back with the right
                // size, but configure the window *parked*. Configuring the model
                // rect here is what used to resurrect a background window onto
                // the workspace the user was actually looking at (a float
                // "appearing out of nowhere" every time it self-resized).
                if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                    c.geom = geom;
                    c.border_w = bw;
                }
                self.apply_parked(e.window)?;
                return Ok(());
            }
            // `client` borrow ends here (geom/bw/hidden are copies); now &mut self.
            self.apply_geom(e.window, geom, bw, true)?;
            return Ok(());
        }
        // Unmanaged (override-redirect / not-yet-tracked) window: honor geometry
        // directly. These windows have no `AppliedState` entry, so the single
        // sink is a no-op for them; emit the raw configure to keep them correctly
        // placed. Stacking bits are intentionally dropped. Sizes are clamped
        // to >= 1: a 0 width/height is a `BadValue` the server would reject.
        let mut aux = ConfigureWindowAux::new();
        if e.value_mask.contains(ConfigWindow::X) {
            aux = aux.x(e.x as i32);
        }
        if e.value_mask.contains(ConfigWindow::Y) {
            aux = aux.y(e.y as i32);
        }
        if e.value_mask.contains(ConfigWindow::WIDTH) {
            aux = aux.width((e.width as u32).max(1));
        }
        if e.value_mask.contains(ConfigWindow::HEIGHT) {
            aux = aux.height((e.height as u32).max(1));
        }
        if e.value_mask.contains(ConfigWindow::BORDER_WIDTH) {
            aux = aux.border_width(e.border_width as u32);
        }
        let _ = self.conn.configure_window(e.window, &aux);
        Ok(())
    }

    pub(super) fn on_configure_notify(
        &mut self,
        e: ConfigureNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.window == self.root {
            self.handle_monitor_change()?;
            return Ok(());
        }
        // A `SendEvent`-generated ConfigureNotify. `apply_geom` fabricates one
        // of these for every client (ICCCM requires it), and because the WM
        // selects `STRUCTURE_NOTIFY` on each managed window the server hands it
        // straight back to us. Its `above_sibling` is a hard-coded `None`, which
        // in a `ConfigureNotify` means "no sibling above this window" (it would
        // read as "raise to the top" if replayed, and the geometry is redundant
        // anyway — we are the ones who set it), so the whole event is dropped.
        if e.response_type & 0x80 != 0 {
            return Ok(());
        }
        // ── Observation of X11 Real, never an instruction ──────────────────
        //
        // Only the root-targeted (SubstructureNotify) copy is considered here;
        // the window also delivers a StructureNotify copy that the compositor
        // sync below consumes.
        //
        // While this WM holds `SUBSTRUCTURE_REDIRECT` on the root the server
        // never applies a client's `ConfigureWindow` to a viewable child of the
        // root, so a reported rect can only be the echo of a request *we* issued
        // — possibly an older one still in flight. Classifying that echo
        // (`classify_configure`) may therefore never lead to adopting it into the
        // model: answering the client's *newest* request with a geometry the WM
        // has already left behind is what made floats jump between two positions
        // /sizes forever (measured: ~150 configures/s ping-pong, one core burnt).
        // Client intent arrives through `ConfigureRequest`, which is the sink that
        // owns `client.geom`.
        if e.event == self.root {
            let reported = Rect::new(e.x as i32, e.y as i32, e.width as u32, e.height as u32);
            let reported_bw = e.border_width as u32;
            // Observability-only: mirror the *real* geometry X11 reported back.
            // Never read for layout/focus/overlay decisions.
            let model = if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                c.last_reported = Some(reported);
                Some((c.geom, c.border_w, c.wm_hidden))
            } else {
                None
            };
            if let Some((model_rect, model_bw, hidden)) = model {
                // A parked window (its workspace is not the monitor's active one)
                // is configured by the WM to the off-screen parking spot on
                // purpose: never "correct" it back on-screen from an echo.
                if !hidden {
                    let verdict = self
                        .applied
                        .windows
                        .get(&e.window)
                        .map(|a| reconciler::classify_configure(reported, reported_bw, a));
                    match verdict {
                        // Our own echo: X11 agrees, nothing to do.
                        Some(reconciler::ConfigureObservation::Compliant) | None => {}
                        // Stale echo (or a genuinely external change): re-assert
                        // the *model*, never the reported rect. When X11 already
                        // matches the model — the normal case, because the
                        // divergence is our own older request — the reconciler's
                        // diff emits nothing at all, so a stale echo costs a hash
                        // lookup and no protocol traffic (this is what kills the
                        // ping-pong).
                        Some(reconciler::ConfigureObservation::Stale) => {
                            self.apply_geom(e.window, model_rect, model_bw, true)?;
                        }
                    }
                }
            }
        }

        // Keep the compositor's cached outer rect in sync (render-only) so the
        // live transform and texture crop match; also track restacking. This runs
        // for every managed or override-redirect child and is idempotent.
        if let Some(c) = self.compositor.as_mut() {
            c.on_configure(
                e.window,
                e.x as i32,
                e.y as i32,
                e.width as u32,
                e.height as u32,
                e.border_width as u32,
            );
            if e.event == self.root {
                let above = (e.above_sibling != x11rb::NONE).then_some(e.above_sibling);
                c.on_restack(e.window, above);
            }
        }
        Ok(())
    }

    /// Re-detect the monitor topology and redistribute clients. Shared by the
    /// root `ConfigureNotify` path (some servers report resolution changes
    /// there) and the `RandR` notify handlers in `mod.rs` (both feed this path). It only acts when
    /// the topology actually changed — same monitor count and geometry means
    /// nothing to do, so repeated events cause no reflow.
    pub(super) fn handle_monitor_change(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let setup = self.conn.setup();
        let screen = &setup.roots[self.screen_num];
        let new_mons = detect_monitors(&self.conn, screen, &self.engine.cfg)?;
        let len_changed = new_mons.len() != self.engine.state.monitors.len();
        let geom_changed = !len_changed
            && new_mons
                .iter()
                .zip(self.engine.state.monitors.iter())
                .any(|(a, b)| a.screen != b.screen || a.workarea != b.workarea);
        if len_changed || geom_changed {
            log::info!(
                "monitor topology changed ({} -> {})",
                self.engine.state.monitors.len(),
                new_mons.len()
            );

            // Collect all managed windows before replacing the monitor vec.
            let old_clients: Vec<Window> = self.engine.state.clients.keys().copied().collect();

            if len_changed {
                // Preserve the layout of monitors whose screen rect did not
                // change, and only re-home the windows that belonged to monitors
                // which have disappeared (hotplug / unplug). Replacing every
                // monitor wholesale used to wipe the tile/float layout of all
                // surviving monitors (N4).
                let old = std::mem::take(&mut self.engine.state.monitors);
                let old_sel = self.engine.state.sel_mon;

                // Fresh monitors from the new topology.
                let mut result = new_mons;

                // Match each old monitor to a surviving new monitor by identical
                // screen rect. Monitors that keep their rect across a hotplug
                // keep their contents; the rest fall through to the orphan
                // re-homing below.
                let mut matched_new = vec![false; result.len()];
                let mut old_to_new: Vec<Option<usize>> = vec![None; old.len()];
                for oi in 0..old.len() {
                    let mut found = None;
                    for ni in 0..result.len() {
                        if !matched_new[ni] && result[ni].screen == old[oi].screen {
                            found = Some(ni);
                            break;
                        }
                    }
                    if let Some(ni) = found {
                        matched_new[ni] = true;
                        old_to_new[oi] = Some(ni);
                    }
                }

                // Copy each preserved monitor's workspaces/clients across, but
                // adopt the new screen and reconcile the tag count.
                for oi in 0..old.len() {
                    if let Some(ni) = old_to_new[oi] {
                        let mut preserved = old[oi].clone();
                        preserved.screen = result[ni].screen;
                        preserved.recalc_geometry();
                        preserved.reconcile_workspaces(result[ni].workspaces.len());
                        result[ni] = preserved;
                    }
                }

                // Install the new monitor vec, then remap every surviving
                // client's monitor index to its matched new index.
                self.engine.state.monitors = result;
                // Re-apply the configured scroll-camera spring constants to the
                // (possibly freshly created) workspace cameras.
                self.engine.apply_camera_cfg();
                for c in self.engine.state.clients.values_mut() {
                    let idx = c.monitor;
                    if let Some(Some(ni)) = old_to_new.get(idx).copied() {
                        c.monitor = ni;
                    }
                }

                // Map the previously selected monitor to its new index, or clamp.
                self.engine.state.sel_mon = match old_to_new.get(old_sel).copied().flatten() {
                    Some(ni) => ni,
                    None => self
                        .engine
                        .state
                        .sel_mon
                        .min(self.engine.state.monitors.len().saturating_sub(1)),
                };

                // Re-home orphan windows (those on a monitor that disappeared):
                // land them on the first surviving monitor, clamped to its tag
                // count. Windows already on preserved monitors are untouched.
                for win in &old_clients {
                    let orphan = match self.engine.state.clients.get(win) {
                        Some(c) => old_to_new.get(c.monitor).copied().flatten().is_none(),
                        None => false,
                    };
                    if !orphan {
                        continue;
                    }
                    let is_float = self
                        .engine
                        .state
                        .clients
                        .get(win)
                        .is_some_and(crate::types::Client::is_float);
                    let target = (0..old_to_new.len())
                        .find_map(|oi| old_to_new[oi])
                        .unwrap_or(0);
                    if let Some(c) = self.engine.state.clients.get_mut(win) {
                        c.monitor = target;
                        let n_ws = self.engine.state.monitors[target].workspaces.len();
                        c.workspace = c.workspace.min(n_ws.saturating_sub(1));
                        let ws_i = c.workspace;
                        if is_float {
                            self.engine.state.monitors[target].workspaces[ws_i]
                                .floats
                                .push(*win);
                        } else {
                            self.engine.state.monitors[target].workspaces[ws_i]
                                .add_tiled(*win, self.engine.cfg.column_width);
                        }
                    }
                }

                // Re-clamp floats and re-lay every monitor's tree.
                for i in 0..self.engine.state.monitors.len() {
                    self.reposition_floats(i)?;
                    self.arrange(i)?;
                }
            } else {
                // Geometry-only change: update screen/workarea in place,
                // preserving all workspace state and client assignments.
                for (new_mon, old_mon) in new_mons.iter().zip(self.engine.state.monitors.iter_mut())
                {
                    old_mon.screen = new_mon.screen;
                    old_mon.recalc_geometry();
                }
                // Reposition floating windows to stay within the new workarea.
                for i in 0..self.engine.state.monitors.len() {
                    self.reposition_floats(i)?;
                    self.arrange(i)?;
                }
            }

            // Update EWMH properties for external taskbars. Only the
            // count/names change here — `_NET_CURRENT_DESKTOP` is published by
            // `ViewWorkspace` via `Effect::SetCurrentDesktop` and must NOT be
            // reset to 0 on every monitor topology change (N1).
            self.update_ewmh_desktop_count()?;
            self.update_workarea()?;

            // Keep the wallpaper output layout in sync with the new topology so a
            // native wallpaper covers every (possibly resized/rearranged) monitor.
            if let Some(comp) = self.compositor.as_mut() {
                let outs: Vec<crate::types::Rect> = self
                    .engine
                    .state
                    .monitors
                    .iter()
                    .map(|m| m.screen)
                    .collect();
                comp.set_outputs(&outs);
            } else {
                // No compositor: re-paint the root-pixmap wallpaper so it
                // covers the new (possibly resized/rearranged) monitors.
                self.apply_root_wallpaper();
            }
        }
        Ok(())
    }

    pub(super) fn on_property(
        &mut self,
        e: PropertyNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.window == self.root && e.atom == u32::from(AtomEnum::WM_NAME) {
            self.update_status()?;
            return Ok(());
        }
        // A dock changing (or clearing) its strut updates its reservation. This
        // fires for unmanaged windows too, so handle it before the DELETE guard
        // and before the client lookup. No `!contains_key` gate: a dock that
        // lost the map/manage race is already managed when its strut notify
        // arrives, and skipping the refresh would leave a stale reservation.
        // `apply_dock_strut` is idempotent (re-registers / clears), and windows
        // without any strut property fall through to a no-op `remove_dock`.
        if e.atom == self.atoms.net_wm_strut_partial || e.atom == self.atoms.net_wm_strut {
            self.apply_dock_strut(e.window)?;
            return Ok(());
        }

        // `_NET_WM_BYPASS_COMPOSITOR`: EWMH hint 1=force ON, 2=force bypass. Handled
        // even on DELETE (property removed → None). Policy re-evaluates each
        // `run_once` via `bypass_candidate`, so no compositor call needed.
        if e.atom == self.atoms.net_wm_bypass_compositor {
            if e.state == Property::DELETE {
                if let Some(cl) = self.engine.state.clients.get_mut(&e.window) {
                    cl.bypass_hint = None;
                }
            } else if let Some(v) =
                read_bypass_hint(&self.conn, e.window, self.atoms.net_wm_bypass_compositor)
            {
                if let Some(cl) = self.engine.state.clients.get_mut(&e.window) {
                    cl.bypass_hint = Some(v);
                }
            } else if let Some(cl) = self.engine.state.clients.get_mut(&e.window) {
                cl.bypass_hint = None;
            }
            return Ok(());
        }

        // `WM_NORMAL_HINTS`: keep `client.hints` fresh so the float
        // `ConfigureRequest` snap (and drag-resize) uses the client's *current*
        // min/max/increment constraints. Dialogs that rewrite their hints
        // mid-life as content changes (Qt download/progress dialogs do exactly
        // this while rows complete) would otherwise be snapped to stale
        // constraints and answer every update with a corrective resize — the
        // sustained bigger/smaller flicker. Handled even on DELETE (property
        // removed → constraints withdrawn). Only the `hints` struct is
        // refreshed here: flags (`FLOAT`/`FIXED`) are map-time layout
        // decisions and must never yank a window mid-life.
        if e.atom == u32::from(AtomEnum::WM_NORMAL_HINTS) {
            // Read before the mutable client borrow below (`read_size_hints`
            // only needs `&self`).
            let fresh = (e.state != Property::DELETE)
                .then(|| self.read_size_hints(e.window))
                .flatten();
            if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                if e.state == Property::DELETE {
                    c.hints = SizeHints::default();
                } else if let Some(h) = fresh {
                    c.hints = h;
                }
            }
            return Ok(());
        }

        if e.state == Property::DELETE {
            return Ok(());
        }

        // `_NET_WM_WINDOW_OPACITY`: tell the compositor the new value so it can
        // fade the window's texture without re-`ConfigureWindow`ing it.
        if e.atom == self.atoms.net_wm_window_opacity {
            if let Some(c) = self.compositor.as_mut() {
                let opacity =
                    read_window_opacity(&self.conn, e.window, self.atoms.net_wm_window_opacity)
                        .unwrap_or(1.0);
                c.on_opacity(e.window, opacity);
                return Ok(());
            }
        }

        if self.engine.state.clients.contains_key(&e.window) {
            let win = e.window;
            if e.atom == self.atoms.net_wm_name || e.atom == u32::from(AtomEnum::WM_NAME) {
                self.refresh_title(win)?;
            } else if e.atom == u32::from(AtomEnum::WM_HINTS) {
                self.refresh_hints(win)?;
            }
            // `WM_NORMAL_HINTS` is handled by its own early arm above (fresh
            // `client.hints` for the float snap); anything else just flows to
            // `publish_state()` below.
        }
        Ok(())
    }

    pub(super) fn on_create_notify(
        &mut self,
        e: CreateNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.compositor.as_mut() {
            c.on_create(e.window);
        }
        Ok(())
    }

    pub(super) fn on_map_notify(
        &mut self,
        e: MapNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.compositor.as_mut() {
            c.on_map(e.window);
        }
        Ok(())
    }

    #[cfg(feature = "compositor-opengl")]
    pub(super) fn on_damage_notify(
        &mut self,
        e: DamageNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.compositor.as_mut() {
            c.on_damage(e.drawable);
        }
        Ok(())
    }

    pub(super) fn on_client_message(
        &mut self,
        e: ClientMessageEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.type_ == self.atoms.net_wm_state {
            let data = e.data.as_data32();
            let action = data[0];
            let a1 = data[1];
            let a2 = data[2];
            let fs_atom = self.atoms.net_wm_state_fullscreen;
            if a1 == fs_atom || a2 == fs_atom {
                // A `_NET_WM_STATE_FULLSCREEN` client message is the *app*
                // asking — a browser's F11 is indistinguishable from any other
                // EWMH request and arrives right here. `FullscreenPolicy::Deny`
                // refuses exactly this path; the user's own `Mod4+F` comes in
                // via `Effect::SetFullscreen` and is never filtered.
                let denied = self
                    .engine
                    .state
                    .clients
                    .get(&e.window)
                    .is_some_and(crate::types::Client::denies_fullscreen);
                if denied {
                    log::debug!(
                        "denied client fullscreen request for {} (deny_fullscreen rule)",
                        e.window
                    );
                    // Rewrite `_NET_WM_STATE` from our flags so the client sees
                    // its request did not take, instead of assuming it did.
                    self.write_net_wm_state(e.window);
                } else {
                    let cur = self
                        .engine
                        .state
                        .clients
                        .get(&e.window)
                        .is_some_and(crate::types::Client::is_fullscreen);
                    let new_fs = match action {
                        0 => false,
                        1 => true,
                        _ => !cur,
                    };
                    if new_fs != cur {
                        // Route through the `ToggleFullscreen` Command so the
                        // fullscreen transition is decided by the core (single
                        // funnel), not by ad-hoc backend state mutation. The
                        // Command emits `Effect::SetFullscreen`, which the
                        // backend then carries out.
                        let effects = self
                            .engine
                            .execute(crate::core::commands::ToggleFullscreen(Some(e.window)));
                        self.run_effects(effects)?;
                    }
                }
            }
            let max_v = self.atoms.net_wm_state_maximized_vert;
            let max_h = self.atoms.net_wm_state_maximized_horiz;
            let wants_v = a1 == max_v || a2 == max_v;
            let wants_h = a1 == max_h || a2 == max_h;
            if wants_v || wants_h {
                // `_NET_WM_STATE_MAXIMIZED_VERT` and `_..._HORZ` are two
                // independent states and a single client message can name one
                // or both. Resolve each requested axis on its own and leave the
                // other untouched (`None`), so a vertical-only maximize stops
                // being silently promoted to a full one.
                let (cur_v, cur_h) = self
                    .engine
                    .state
                    .clients
                    .get(&e.window)
                    .map_or((false, false), |c| (c.is_maximized_v(), c.is_maximized_h()));
                let resolve = |requested: bool, cur: bool| {
                    if !requested {
                        return None;
                    }
                    Some(match action {
                        0 => false,
                        1 => true,
                        _ => !cur,
                    })
                };
                crate::core::commands::apply_maximize(
                    &mut self.engine.state,
                    e.window,
                    resolve(wants_v, cur_v),
                    resolve(wants_h, cur_h),
                );
                self.set_maximized(e.window, resolve(wants_v, cur_v), resolve(wants_h, cur_h))?;
            }
            let urg = self.atoms.net_wm_state_demands_attention;
            if a1 == urg || a2 == urg {
                if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                    c.flags.set(WinFlags::URGENT);
                }
            }
        } else if e.type_ == self.atoms.net_current_desktop {
            let ws = e.data.as_data32()[0] as usize;
            self.do_action(Action::View(ws))?;
        } else if e.type_ == self.atoms.net_active_window {
            // _NET_ACTIVE_WINDOW (EWMH active-window request from an app/pager).
            // The focus decision is made by the pure core policy
            // `decide_active_window`, which refuses to let an unrelated window
            // steal focus from a presented fullscreen/overlay on the same
            // (monitor, workspace), and never switches the user's selected
            // monitor or active workspace.
            match crate::core::commands::decide_active_window(&self.engine.state, e.window) {
                crate::core::commands::ActiveWindowIntent::Focus(w) => {
                    // An explicit active-window request supersedes any deferred
                    // focus pending behind an overlay on this (monitor, ws).
                    if self.engine.state.pending_focus.is_some() {
                        self.engine.state.pending_focus = None;
                    }
                    self.focus(Some(w))?;
                }
                crate::core::commands::ActiveWindowIntent::Ignore => {
                    log::debug!(
                        "_NET_ACTIVE_WINDOW for {} ignored: would steal a presented overlay",
                        e.window
                    );
                }
            }
        } else if e.type_ == self.atoms.net_close_window {
            self.kill(e.window)?;
        }
        Ok(())
    }

    pub(super) fn on_key(&mut self, e: KeyPressEvent) -> Result<(), Box<dyn std::error::Error>> {
        self.last_event_time = e.time;
        let mods = clean_mask(u16::from(e.state), self.numlock);
        // Primary lookup uses the column-0 keysym (B6). Shift/Lock travel only in
        // `mods`, so `Mod4+Shift+bracketleft` resolves to the bound keysym.
        let ks_primary = self.keycode_to_keysym(e.detail, u16::from(e.state))?;
        // Fallback to the shifted/locked column: anyone who relied on the old
        // shifted-only resolution still works (B6). Clamped to group 1, which
        // is the half of the keymap `grab_keys` actually grabbed on.
        let shift = u16::from(e.state) & u16::from(ModMask::SHIFT) != 0;
        let lock = u16::from(e.state) & u16::from(ModMask::LOCK) != 0;
        let col = dispatch_col(shift, lock, self.raw_kpk);
        let ks_shifted = self.keysym_at_col(e.detail, col);

        // Last resort: keycodes grabbed through the keysym-directed fallback
        // (the bind only exists at an AltGr / second-group level).
        let resolved = resolve_binding(
            &self.keymap,
            &self.code_bindings,
            mods,
            e.detail,
            ks_primary,
            ks_shifted,
        );
        let Some((key, action)) = resolved else {
            log::debug!(
                "key press keycode={} mods={mods:#x} (keysyms {ks_primary:#x}/{ks_shifted:#x}) matched no binding",
                e.detail
            );
            return Ok(());
        };

        let min_interval = match action {
            Action::Spawn(_) => std::time::Duration::from_millis(200),
            _ => std::time::Duration::from_millis(60),
        };
        if let Some(t) = self.last_key_times.get(&key) {
            if t.elapsed() < min_interval {
                return Ok(());
            }
        }
        let cutoff = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        self.last_key_times.retain(|_, v| *v >= cutoff);
        self.last_key_times.insert(key, std::time::Instant::now());
        self.do_action(action)?;
        // Keyboard navigation must not be instantly undone by an EnterNotify
        // while the pointer is parked over another tile's edge. Ignore
        // pointer-focus for a short window; the first real MotionNotify lifts
        // the guard.
        self.pointer_guard_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(50));
        Ok(())
    }

    pub(super) fn on_enter(
        &mut self,
        e: EnterNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.mode != NotifyMode::NORMAL || e.detail == NotifyDetail::INFERIOR {
            return Ok(());
        }
        self.last_event_time = e.time;
        if self.engine.cfg.focus_mouse {
            // Guard: a key navigation that just ran must not be undone by an
            // EnterNotify caused by the pointer sitting over a tile edge (see
            // on_key) — ignore pointer-focus until the user actually moves the
            // pointer (MotionNotify clears `pointer_guard_until`).
            if let Some(until) = self.pointer_guard_until {
                if std::time::Instant::now() < until {
                    return Ok(());
                }
            }
            if let Some(cw) = self.find_client(e.event) {
                // Compare against the entered window's own monitor, not
                // `sel_mon`: with the pointer on another monitor this used to
                // compare (and refocus) against the wrong monitor's focus.
                // `find_client` resolves the client; its monitor is the
                // authority here. Guarded for a stale `sel_mon` after hotplug.
                let mon_of = self
                    .engine
                    .state
                    .clients
                    .get(&cw)
                    .map_or(self.engine.state.sel_mon, |c| c.monitor);
                let focused = self
                    .engine
                    .state
                    .monitors
                    .get(mon_of)
                    .and_then(|m| m.focused);
                if focused != Some(cw) {
                    self.focus(Some(cw))?;
                }
            }
        }
        Ok(())
    }

    /// `FocusIn` on a managed client. The WM selected `EventMask::FOCUS_CHANGE`
    /// on every client (manage.rs), so the server tells us the real X input
    /// focus moved. Mirror it into `state.x11_input_focus` and let `reconcile_focus`
    /// re-affirm the WM's logical intent if the two have drifted.
    ///
    /// Only a *real* focus change counts. A `mode` other than `Normal` (a
    /// keyboard grab, e.g. a menu or drag, or an explicit `XSetInputFocus`
    /// triggered while a grab is active) must not be treated as a focus move, and
    /// a `detail` of `Inferior` (focus merely moved to a child sub-window, which
    /// clients such as Gecko do constantly between their internal windows) means
    /// the top-level focus has not actually changed. Ignoring both prevents the
    /// spurious `reconcile_focus` churn that fights clients with child windows
    /// and causes focus ping-pong (INV-C).
    pub(super) fn on_focus_in(
        &mut self,
        e: FocusInEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.mode != NotifyMode::NORMAL {
            return Ok(());
        }
        if e.detail == NotifyDetail::INFERIOR || e.detail == NotifyDetail::POINTER {
            return Ok(());
        }
        self.engine.state.x11_input_focus = if e.event == self.root {
            None
        } else {
            Some(e.event)
        };
        self.reconcile_focus()?;
        Ok(())
    }

    /// `FocusOut` from a managed client: focus left `e.event`. If it went to
    /// another of our clients we'll receive the matching `FocusIn`; otherwise it
    /// moved to root / an unmanaged target (e.g. an OR popup, a Wine dialog, or
    /// an external `XSetInputFocus`). Clear our mirror if it pointed here and let
    /// `reconcile_focus` re-assert the logical focus so the WM stays authoritative
    /// and the border colors track reality.
    ///
    /// As with `on_focus_in`, ignore grab-mode and `Inferior` transitions: a
    /// child-window `FocusOut` is not the top-level window losing focus, and a
    /// grab-induced `FocusOut` (which will be paired with a `FocusIn` on
    /// ungrab) must not clear our mirror nor trigger a spurious repair (INV-C).
    pub(super) fn on_focus_out(
        &mut self,
        e: FocusOutEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.mode != NotifyMode::NORMAL {
            return Ok(());
        }
        if e.detail == NotifyDetail::INFERIOR || e.detail == NotifyDetail::POINTER {
            return Ok(());
        }
        if self.engine.state.x11_input_focus == Some(e.event) {
            self.engine.state.x11_input_focus = None;
        }
        self.reconcile_focus()?;
        Ok(())
    }

    /// Core `MappingNotify`. Only arms the debounced refresh — re-reading the
    /// keymap here (and propagating the error with `?`) used to take the whole
    /// WM down on a transient failure, since this runs inside `run_once` (R3).
    ///
    /// `POINTER` is ignored: it reports button mapping, which changes nothing
    /// about the keyboard grabs. Note that toggling `NumLock` does *not* generate
    /// a `MappingNotify` — the modifier mapping only changes when a tool like
    /// `xmodmap`/`setxkbmap` rewrites it.
    pub(super) fn on_mapping(&mut self, e: &MappingNotifyEvent) {
        if e.request == Mapping::KEYBOARD || e.request == Mapping::MODIFIER {
            self.schedule_keyboard_refresh();
        }
    }
}

/// Read `_NET_WM_WINDOW_OPACITY` (a 32-bit CARDINAL in `0..=0xFFFFFFFF`, where
/// the max value means fully opaque) and normalise it to `0.0..=1.0`. Returns
/// `None` when the property is absent or unreadable, so the caller keeps the
/// current opacity.
fn read_window_opacity(conn: &maverick_x11::XConn, win: Window, atom: Atom) -> Option<f32> {
    let ty = u32::from(AtomEnum::CARDINAL);
    let reply = conn
        .get_property(false, win, atom, ty, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let raw = reply.value32()?.next()?;
    Some(raw as f32 / 0xFFFF_FFFFu32 as f32)
}

/// Read `_NET_WM_BYPASS_COMPOSITOR` (CARDINAL 0/1/2). Returns `None` when absent.
fn read_bypass_hint(conn: &maverick_x11::XConn, win: Window, atom: Atom) -> Option<u32> {
    let ty = u32::from(AtomEnum::CARDINAL);
    let reply = conn
        .get_property(false, win, atom, ty, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let v = reply.value32().and_then(|mut it| it.next())?;
    Some(v)
}
