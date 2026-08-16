use super::*;
use crate::backend::x11::reconciler::{reconcile, GeometryEffect};
use crate::core::commands::retarget_focus_to_window;
use crate::core::desired::DesiredState;
use crate::core::grid;
use crate::core::layout::Phase;
use crate::core::present::present_into;
use x11rb::protocol::shape;

// ── input-trace instrumentation (feature `input-trace`) ───────────────────────
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
// Mirrors `itrace!` but for the desired→applied→real→x11_focus pipeline. No-op
// unless `window-trace` is enabled, so the call sites below carry no runtime
// cost in normal builds. Fase 8 of the real-client compatibility plan.
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

/// Approximate a rounded rectangle of size `w`×`h` with corner radius `r` as
/// a list of X11 `Rectangle`s: one full-width middle band, plus one 1px-tall
/// rectangle per row of each rounded corner (inset by the circle's chord at
/// that row). This is the same technique window managers have used for
/// XShape-based rounding for decades — O(r) rectangles, no external deps,
/// no compositor required. `r` is clamped so it can never exceed half of
/// either dimension.
fn rounded_rectangles(w: i32, h: i32, r: i32) -> Vec<Rectangle> {
    let r = r.clamp(0, w.min(h) / 2);
    if r <= 0 || w <= 0 || h <= 0 {
        return vec![Rectangle {
            x: 0,
            y: 0,
            width: w.max(0) as u16,
            height: h.max(0) as u16,
        }];
    }

    let mut rects = Vec::with_capacity(2 * r as usize + 1);
    rects.push(Rectangle {
        x: 0,
        y: r as i16,
        width: w as u16,
        height: (h - 2 * r).max(0) as u16,
    });

    for i in 0..r {
        // Row i (0 = outermost) sits `dy` pixels from the corner circle's
        // vertical center; the circle's horizontal chord at that row gives
        // how far to inset from the edge.
        let dy = r - i;
        let chord = ((r * r - dy * dy).max(0) as f64).sqrt() as i32;
        let inset = (r - chord).clamp(0, w / 2);
        let width = (w - 2 * inset).max(0) as u16;
        rects.push(Rectangle {
            x: inset as i16,
            y: i as i16,
            width,
            height: 1,
        });
        rects.push(Rectangle {
            x: inset as i16,
            y: (h - 1 - i) as i16,
            width,
            height: 1,
        });
    }
    rects
}

/// Clamp a floating window's geometry so the whole frame (content + the border
/// on both sides) fits inside `wa`.
///
/// Size is clamped *before* position on purpose: with a float wider or taller
/// than the workarea the naive `max_x = wa.x + wa.w - g.w - 2*bw` goes below
/// `wa.x`, so clamping the position alone parks the window at a negative
/// coordinate while its size still overflows the screen. Clamping the size
/// first keeps `min <= max` and guarantees the result is inside `wa`.
pub(crate) fn clamp_float_to_workarea(mut g: Rect, wa: Rect, bw: u32) -> Rect {
    let frame = 2 * bw as i32;
    let max_w = (wa.w as i32 - frame).max(1) as u32;
    let max_h = (wa.h as i32 - frame).max(1) as u32;
    // Floor at 1: X11 rejects a 0x0 configure with BadValue, so a client
    // requesting 0x0 (or any zero dimension) must never reach X11 as such. The
    // WM policy normalizes the degenerate request to the X11-valid minimum.
    g.w = g.w.min(max_w).max(1);
    g.h = g.h.min(max_h).max(1);
    let max_x = (wa.x + wa.w as i32 - g.w as i32 - frame).max(wa.x);
    let max_y = (wa.y + wa.h as i32 - g.h as i32 - frame).max(wa.y);
    g.x = g.x.clamp(wa.x, max_x);
    g.y = g.y.clamp(wa.y, max_y);
    g
}

/// How many `transient_parent` links a single ownership question may follow.
///
/// `WM_TRANSIENT_FOR` is client-controlled and completely unvalidated by the X
/// server: a buggy (or hostile) client can point a window at itself, or two
/// windows at each other, producing a *cycle* in the ownership graph. The walk
/// below therefore has to be bounded — an unbounded one would hang the WM's
/// stacking pass forever. 4 is the depth real toolkits need (window → dialog →
/// sub-dialog → menu/tooltip); beyond that the chain is not a legitimate
/// popup-of-popup relation, and the cost of the check (O(depth) per float, per
/// restack) stays constant.
///
/// Exceeding the limit is *fail-safe*, never fail-open: the deep window is
/// simply not recognised as owned by the overlay, so it is not raised above it.
/// It keeps its own client entry, focus, geometry and workspace — nothing is
/// orphaned and no reference is dropped by the bound itself.
pub(super) const MAX_TRANSIENT_DEPTH: usize = 4;

/// True when `win`'s `transient_parent` chain reaches any window in `roots`,
/// following at most [`MAX_TRANSIENT_DEPTH`] links.
///
/// Pure over the client map (no X11, no `&self`) so the depth/cycle behaviour is
/// unit-testable. A link that names a window which is no longer a client ends
/// the walk (returns false) instead of panicking — destroyed parents are always
/// treated as "no parent".
pub(super) fn transient_chain_reaches(
    clients: &std::collections::HashMap<WindowId, Client>,
    win: WindowId,
    roots: &[WindowId],
) -> bool {
    let mut cur = clients.get(&win).and_then(|c| c.transient_parent);
    for _ in 0..MAX_TRANSIENT_DEPTH {
        let Some(p) = cur else {
            return false;
        };
        if roots.contains(&p) {
            return true;
        }
        cur = clients.get(&p).and_then(|c| c.transient_parent);
    }
    false
}

impl WindowManager {
    /// Apply (or clear) rounded corners on `win` via the Shape extension's
    /// bounding-shape mask. Only called when `corner_radius > 0` — with the
    /// default of `0` this codepath, and every X11 Shape request, never
    /// runs, so there's zero cost for users who don't opt in. `radius` is
    /// the *effective* radius for this call — callers pass `0` to force a
    /// square mask (e.g. fullscreen, which must stay edge-to-edge like niri:
    /// rounding an overlay that touches the screen border just clips the
    /// content under a curved corner instead of producing a real rounded
    /// look, since there's no desktop showing behind it to round into).
    pub(super) fn round_corners(&self, win: Window, outer_w: u32, outer_h: u32, radius: i32) {
        let rects = rounded_rectangles(outer_w as i32, outer_h as i32, radius);
        // Fire-and-forget, same rationale as apply_geom's configure_window:
        // this runs on every geometry change, a synchronous round-trip per
        // window would be unacceptable. Servers without the Shape extension
        // (essentially none — it's been near-universal since the 90s) just
        // silently ignore the request.
        let _ = shape::rectangles(
            &self.conn,
            shape::SO::SET,
            shape::SK::BOUNDING,
            ClipOrdering::UNSORTED,
            win,
            0,
            0,
            &rects,
        );
    }

    pub(super) fn arrange(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full(mon_idx, true)
    }

    /// Like `arrange`, but projects with `Phase::Live` so the X11-only path
    /// animates each frame (reads the live camera `position`). The compositor
    /// path must NOT use this — it configures X only once, at the settled
    /// (`target`) position, and animates on the GPU instead.
    pub(super) fn arrange_live(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full_phase(mon_idx, true, Phase::Live)
    }

    /// Reposition floating windows to stay within the workarea after a monitor
    /// geometry change. Clamps each float's size *and* position so its whole
    /// frame remains inside the new workarea — including floats that are larger
    /// than the workarea itself (see `clamp_float_to_workarea`).
    pub(super) fn reposition_floats(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() {
            return Ok(());
        }
        let wa = self.engine.state.monitors[mon_idx].workarea;

        // Collect all floats that need repositioning to avoid borrow conflicts
        let mut to_reposition: Vec<(WindowId, Rect, u32)> = Vec::new();

        // Regular floats in workspaces
        for ws in &self.engine.state.monitors[mon_idx].workspaces {
            for &win in &ws.floats {
                if let Some(client) = self.engine.state.clients.get(&win) {
                    let bw = client.border_w;
                    let g = clamp_float_to_workarea(client.geom, wa, bw);
                    if g != client.geom {
                        to_reposition.push((win, g, bw));
                    }
                }
            }
        }

        // Sticky floats that belong to this monitor
        for (&win, client) in &self.engine.state.clients {
            if client.monitor == mon_idx && client.is_sticky() && client.is_float() {
                let bw = client.border_w;
                let g = clamp_float_to_workarea(client.geom, wa, bw);
                if g != client.geom {
                    to_reposition.push((win, g, bw));
                }
            }
        }

        // Apply all repositionings
        for (win, g, bw) in to_reposition {
            self.apply_geom(win, g, bw, true)?;
        }

        Ok(())
    }

    /// P8/P11: arrange with optional `hide_offscreen`. Stacking is always
    /// refreshed (cheap, and done inside here) so a separate restack step is
    /// unnecessary.
    pub(super) fn arrange_full(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full_phase(mon_idx, do_hide, Phase::Settled)
    }

    /// `arrange_full` with an explicit projection phase. `Settled` (the
    /// compositor path, one-shot) projects to the camera's rest `target`;
    /// `Live` (the X11-only animation path, per-frame) projects to the live
    /// `position` so windows ease smoothly.
    pub(super) fn arrange_full_phase(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
        phase: Phase,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() {
            return Ok(());
        }

        if do_hide && self.drag.is_none() {
            self.hide_offscreen(mon_idx)?;
        }

        // P10: reuse pre-allocated buffer
        arrange(
            &self.engine.state,
            mon_idx,
            &self.engine.cfg,
            &self.layout_registry,
            phase,
            &mut self.desired,
            &mut self.ribbon_scratch,
        );

        // Capture the base grid geometry as a derived snapshot so the next
        // frame's `Grid` arrangement can keep existing windows stable, and so
        // spatial focus/move commands have geometry to navigate. The overlay
        // below rewrites placements in place, which must NOT leak into the
        // snapshot — the snapshot stays the pure base layout.
        {
            let mon = &self.engine.state.monitors[mon_idx];
            if mon.ws().layout == LayoutKind::Grid {
                let (_, snap) = grid::arrange_workspace(
                    mon.ws(),
                    &self.engine.cfg,
                    mon,
                    mon.ws().grid_snapshot.as_ref(),
                );
                self.engine.state.monitors[mon_idx].ws_mut().grid_snapshot = Some(snap);
            }
        }

        // Presentation layer: apply the fullscreen/maximized overlay in place.
        present_into(
            &self.engine.state,
            &self.engine.state.monitors[mon_idx],
            &mut self.desired,
            &mut self.present_scratch,
        );
        // Collect into a local so the immutable borrow of `desired`
        // ends before `apply_geom` mutates `self`.
        let desired = DesiredState::from_placements(&self.desired, &self.present_scratch);
        let effects = reconcile(&desired, &self.engine.state, &mut self.applied);
        #[cfg(feature = "window-trace")]
        let effect_count = effects.len();
        // Observability-only: mirror the per-window *desired* rect (Fase 8). Never
        // read for layout/focus/overlay decisions.
        for dw in &desired.windows {
            if let Some(c) = self.engine.state.clients.get_mut(&dw.window) {
                c.last_desired = Some(dw.rect);
            }
        }
        for e in effects {
            let GeometryEffect::Configure { win, rect, border } = e;
            self.emit_geometry(win, rect, border, true)?;
        }
        #[cfg(feature = "window-trace")]
        wtrace!(
            "arrange mon={} desired={} effects={} x11_focus={:?} sel_mon={}",
            mon_idx,
            desired.windows.len(),
            effect_count,
            self.engine.state.x11_input_focus,
            self.engine.state.sel_mon
        );
        self.desired.clear();
        // Overlay stacking: presented windows above tiles, popups of presented
        // windows above the overlay, focused window on top (or peek).
        self.stack_overlay(mon_idx);
        // When the compositor owns the screen, the new (settled) geometry must
        // trigger a redraw — the overlay is what's actually visible, not the
        // window's live X geometry.
        if let Some(c) = self.compositor.as_mut() {
            c.invalidate();
        }
        // The live projection for this monitor is now stale; the frame loop
        // recomputes it (and only it) on the next composited frame.
        self.engine.state.monitors[mon_idx].layout_dirty = true;
        Ok(())
    }

    pub(super) fn hide_offscreen(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = &mon.workspaces[mon.active_ws];

        // P12: reuse allocated buffers
        self.hide_ws_set.clear();
        self.hide_mon_vec.clear();
        self.hide_ws_set.extend(
            ws.columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .chain(ws.floats.iter().copied()),
        );
        // NOTE: a fullscreen column stays visible (it scrolls with the camera as
        // a normal ribbon participant), so it must remain *in* `hide_ws_set` —
        // i.e. it is NOT physically hidden here. Previously fullscreen windows
        // were removed from the set, which sent them through the hide branch
        // (off-screen + `wm_hidden = true`); `arrange` then re-showed them
        // physically but left `wm_hidden` stale. That stale flag later blocked
        // `hide_offscreen` from ever hiding the window on a workspace switch, so
        // a fullscreen tile kept covering the next workspace. Keeping it in the
        // set avoids the stale flag entirely.
        //
        // Sticky floats stay visible on every workspace of this monitor.
        self.hide_ws_set.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx && c.is_sticky())
                .map(|(w, _)| *w),
        );
        self.hide_mon_vec.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx)
                .map(|(w, _)| *w),
        );

        let hide_wins: Vec<_> = self.hide_mon_vec.drain(..).collect();
        for win in hide_wins {
            let in_ws = self.hide_ws_set.contains(&win);
            let client = match self.engine.state.clients.get_mut(&win) {
                Some(c) => c,
                None => continue,
            };
            if !in_ws && !client.wm_hidden {
                let w = client.geom.w.min(i32::MAX as u32) as i32;
                let off_x = w.saturating_add(200).saturating_neg();
                // Route the off-screen move through the single geometry sink.
                // The logical `client.geom` is intentionally left untouched
                // (`write_client_geom = false`) — only `AppliedState` and the
                // X11 configure are updated.
                let off_rect = Rect::new(off_x, client.geom.y, client.geom.w, client.geom.h);
                let bw = client.border_w;
                let on_other_ws = client.workspace != self.engine.state.monitors[mon_idx].active_ws;
                self.apply_geom(win, off_rect, bw, false)?;
                if on_other_ws {
                    if let Some(c) = self.compositor.as_mut() {
                        c.set_hidden(win, true);
                    }
                }
                if let Some(c) = self.engine.state.clients.get_mut(&win) {
                    c.wm_hidden = true;
                }
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                // Route the re-show through the single geometry sink. The logical
                // `client.geom` is left untouched (`write_client_geom = false`).
                let real_rect = Rect::new(gx, gy, client.geom.w, client.geom.h);
                let bw = client.border_w;
                self.apply_geom(win, real_rect, bw, false)?;
                if let Some(c) = self.compositor.as_mut() {
                    c.set_hidden(win, false);
                }
                if let Some(c) = self.engine.state.clients.get_mut(&win) {
                    c.wm_hidden = false;
                }
            }
        }
        Ok(())
    }

    /// Unify stacking for a monitor's active workspace:
    ///
    /// 1. floats above tiled windows (base float layer);
    /// 2. the presentation overlay — in `Grid`, every fullscreen window; in any
    ///    layout a `FullscreenPolicy::True` fullscreen window (games: exclusive,
    ///    outside the ribbon) and a maximized window while focused —
    ///    most-recently-focused last → on top. In the `Column` layout an
    ///    ordinary fullscreen window is NOT an overlay (it scrolls with the
    ///    ribbon), so it is excluded here;
    ///
    /// "fullscreen covering" (case 2-bis): when the focused window of a `Column`
    ///    workspace is fullscreen, the camera is settled and we are not in
    ///    Overview, the fullscreen tile is raised above *everything* (including
    ///    the dock/bar). The moment any of those conditions breaks it drops back
    ///    to a normal tile and the dock is re-raised so the bar returns on top
    ///    (only on that transition, so floats — which ride above the dock in the
    ///    base layer — are never pushed below it);
    /// 3. the focused window if it is a *floating* dialog/popup ("peek"): it
    ///    rises above the presented window so a focused popup stays visible.
    ///    A focused *tiled* window never peeks: h/l still moves focus freely
    ///    underneath a fullscreen window exactly like it should;
    /// 4. floating popups/dialogs whose `WM_TRANSIENT_FOR` chain reaches a
    ///    presented window — always above that overlay.
    ///
    /// Fire-and-forget `StackMode::ABOVE` (or `BELOW` for the covering→off
    /// transition): arrange/focus paths must not block.
    ///
    /// To avoid a `raise()` storm during the camera animation (arrange runs on
    /// every monitor every frame), the desired top-to-bottom order is computed
    /// into `order` and compared with the cached `last_stack_order[mon_idx]`;
    /// `raise` is only re-issued when the order actually changed (bug C6).
    fn stack_overlay(&mut self, mon_idx: usize) {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = mon.ws();

        let mut order: Vec<WindowId> = Vec::new();

        // 1. Base float layer.
        for &win in &ws.floats {
            if self.engine.state.clients.contains_key(&win) {
                order.push(win);
            }
        }
        // Sticky floats ride above every workspace's tiles by definition —
        // include them in the base layer regardless of which workspace is
        // active.
        for (&win, c) in &self.engine.state.clients {
            if c.monitor == mon_idx && c.is_sticky() {
                order.push(win);
            }
        }

        // 2. Presentation overlay. In the Column layout a fullscreen window is a
        //    ribbon participant, not an overlay, so it is excluded here; only
        //    Grid fullscreen, `FullscreenPolicy::True` fullscreen (exclusive in
        //    any layout, and already excluded from `fs_ctx`) and focused
        //    maximized count.
        let mut presented: Vec<WindowId> = ws
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .chain(ws.floats.iter().copied())
            .filter(|win| {
                self.engine.state.clients.get(win).is_some_and(|c| {
                    (c.is_fullscreen() && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
                        || ws.presented_maximize == Some(*win)
                })
            })
            .collect();
        presented.sort_by_key(|win| mon.focus_stack.iter().position(|&x| x == *win).unwrap_or(0));
        order.extend(presented.iter().copied());

        // 3. Peek: a focused *floating* window (dialog/popup) is raised above
        //    the overlay so the user sees where focus sits. Deliberately
        //    excludes tiled columns: h/l still moves focus underneath a
        //    fullscreen window exactly like it should (never blocked), but a
        //    plain tile must not visually climb above a fullscreen window just
        //    because it now has focus.
        if !presented.is_empty() {
            if let Some(fw) = mon.focused {
                if !presented.contains(&fw)
                    && self
                        .engine
                        .state
                        .clients
                        .get(&fw)
                        .is_some_and(Client::is_float)
                {
                    order.push(fw);
                }
            }
        }

        // 4. Owned popups of the overlay: a float whose transient-parent chain
        //    reaches a presented window must sit above it.
        for &win in &ws.floats {
            if self.transient_of(win, &presented) {
                order.push(win);
            }
        }

        if self
            .last_stack_order
            .get(&mon_idx)
            .is_none_or(|prev| *prev != order)
        {
            for &win in &order {
                self.raise(win);
            }
            self.last_stack_order.insert(mon_idx, order);
        }

        // 2-bis. Fullscreen covering. The focused fullscreen tile of a Column
        //    workspace, while the camera is settled and not in Overview, is
        //    raised above everything (incl. the dock). Otherwise it is a normal
        //    tile: when coverage ends we drop it to the bottom of the stack so
        //    the neighbouring tile (and the bar) paint over it, and re-raise the
        //    dock so the bar returns on top. Both only happen on the
        //    covering→not-covering transition, to avoid a `raise()` storm.
        // The window that currently covers the screen as a Column ribbon tile
        // (or `None`). The runtime raise/drop additionally requires the camera
        // and zoom to be settled; `covering_fullscreen_window` only names *which*
        // window covers — see its docs for why this is distinct from
        // `presented_overlay_owner`.
        let cover_win = self.engine.state.covering_fullscreen_window(mon_idx);
        let covering = cover_win.is_some()
            && (ws.camera.position - ws.camera.target).abs() < 0.5
            && ws.camera.velocity.abs() < 0.01
            && (ws.zoom - ws.zoom_target).abs() < 0.001;

        // `cover_win` is the *focused* fullscreen window (or `None`); exactly one
        // fullscreen column is raised above the dock at a time — the one you are
        // looking at. Track the transition so moving focus between fullscreen
        // columns drops the old one and raises the new one.
        let new_cover = if covering { cover_win } else { None };
        let prev_cover = self.fs_covering.get(&mon_idx).copied().flatten();
        if prev_cover != new_cover {
            // Drop the previously-covering window to the bottom so the neighbour
            // tile and the bar paint over it (only on this transition).
            if let Some(w) = prev_cover {
                if new_cover != Some(w) {
                    let _ = self.conn.configure_window(
                        w,
                        &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
                    );
                    for (&dock, &dock_mon) in &self.docks {
                        if dock_mon == mon_idx {
                            self.raise(dock);
                        }
                    }
                }
            }
            // Raise the newly-covering window above the dock and every tile.
            if let Some(w) = new_cover {
                self.raise(w);
                self.last_stack_order.entry(mon_idx).or_default().push(w);
            }
        }
        self.fs_covering.insert(mon_idx, new_cover);
    }

    /// True when `win`'s `transient_parent` chain reaches any window in `roots`.
    /// Thin wrapper over the pure [`transient_chain_reaches`] walk (which owns
    /// the `MAX_TRANSIENT_DEPTH` bound); kept as a method so the stacking code
    /// reads the same as before.
    pub(super) fn transient_of(&self, win: WindowId, roots: &[WindowId]) -> bool {
        transient_chain_reaches(&self.engine.state.clients, win, roots)
    }

    /// Non-diffing X11 geometry emitter. Emits EXACTLY the rect/border it is given.
    /// The Reconciler (`reconcile`) is the sole decider that updates `AppliedState`;
    /// this method only writes to X11 (and, for the normal path, mirrors the desired
    /// into `client.geom`). It must NOT re-diff — that would drop the effect.
    fn emit_geometry(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
        write_client_geom: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match self.engine.state.clients.get(&win) {
            Some(c) => c,
            None => return Ok(()),
        };
        // Monitor whose live projection is invalidated by this geometry write.
        let mon = client.monitor;
        // Captured before the mutable borrow below flips geom/border_w.
        let is_fullscreen = client.is_fullscreen();

        let _ = self.conn.configure_window(
            win,
            &ConfigureWindowAux::new()
                .x(geom.x)
                .y(geom.y)
                .width(geom.w)
                .height(geom.h)
                .border_width(bw),
        );

        let event = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: win,
            window: win,
            above_sibling: x11rb::NONE,
            x: geom.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            y: geom.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            width: geom.w.clamp(0, u16::MAX as u32) as u16,
            height: geom.h.clamp(0, u16::MAX as u32) as u16,
            border_width: bw.clamp(0, u16::MAX as u32) as u16,
            override_redirect: false,
        };
        // Fire-and-forget: no .check() here — this is called for every window
        // in arrange(), so a synchronous RTT per window is unacceptable.
        let _ = self
            .conn
            .send_event(false, win, EventMask::STRUCTURE_NOTIFY, event);

        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            if write_client_geom {
                c.geom = geom;
                c.border_w = bw;
            }
            c.geometry_dirty = false;
        }
        // Invalidate this monitor's cached live projection so the compositor
        // re-projects it on the next frame (the geometry it drew is now stale).
        if mon < self.engine.state.monitors.len() {
            self.engine.state.monitors[mon].layout_dirty = true;
        }

        if self.engine.cfg.corner_radius > 0 && self.compositor.is_none() {
            // Fullscreen is always square, niri-style — border-0 and edge-to-
            // edge, so a rounded mask has no desktop behind it to reveal and
            // just chops the content under a curved clip instead.
            let r = if is_fullscreen {
                0
            } else {
                self.engine.cfg.corner_radius as i32
            };
            self.round_corners(win, geom.w + 2 * bw, geom.h + 2 * bw, r);
        }

        Ok(())
    }

    pub(super) fn apply_geom(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
        write_client_geom: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let geom_dirty = self
            .engine
            .state
            .clients
            .get(&win)
            .map(|c| c.geometry_dirty)
            .unwrap_or(false);
        // The Reconciler is the single owner of "what geometry is already
        // applied to X11" (Fase 1, plan 1786564084575). Diff the desired
        // placement against `AppliedState`; only emit `configure_window` when it
        // actually changed. `geometry_dirty` forces the reconfigure even when
        // the rect is identical (a fullscreen/maximize transition changed the
        // border/state without moving the window), preserving the exact skip
        // rule the old `geom == client.geom` comparison enforced.
        if let Some((g, b)) = self.applied.diff(win, geom, bw, geom_dirty) {
            self.emit_geometry(win, g, b, write_client_geom)?;
        }
        Ok(())
    }

    /// Raise `win` above all its siblings (`TopLevel`). Fire-and-forget: called
    /// from arrange/focus paths where a synchronous RTT per window is not acceptable.
    fn raise(&self, win: Window) {
        let _ = self
            .conn
            .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }

    pub(super) fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

        #[cfg(feature = "input-trace")]
        itrace!(
            "focus() called win={:?} valid={:?} sel_mon={} prev_focused(sel_mon)={:?} x11_input_focus={:?}",
            win, valid_win, self.engine.state.sel_mon,
            self.engine.state.monitors.get(self.engine.state.sel_mon).and_then(|m| m.focused),
            self.engine.state.x11_input_focus
        );

        // The presentation overlay (core::present) is independent of focus, so
        // a focus change never recomputes or re-sizes geometry. The only thing
        // focus influences is stacking: a focused presented window rises above
        // the other presented ones, and a focused plain tile "peeks" above the
        // overlay (see focus() below).
        let prev_focused = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);

        if let Some(w) = valid_win {
            // P6: Single lookup — extract everything we need
            let (mon_i, geom, wants, urgent) = {
                let c = match self.engine.state.clients.get(&w) {
                    Some(c) => c,
                    None => return self.focus(None),
                };
                if c.no_focus() {
                    return Ok(());
                }
                (
                    c.monitor,
                    c.geom,
                    c.wants_input,
                    c.flags.has(WinFlags::URGENT),
                )
            };

            // Guard against stale client.monitor after hotplug
            if mon_i >= self.engine.state.monitors.len() {
                return Ok(());
            }

            // unfocus previous — only if we're actually about to focus the new one
            if prev_focused != valid_win {
                if let Some(pw) = prev_focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
            }

            self.engine.state.sel_mon = mon_i;

            // set X11 input focus. Use the real last-input timestamp, not
            // `CURRENT_TIME`: a `CurrentTime` focus request is silently ignored
            // by the server when a newer focus change has occurred, which is
            // exactly what desyncs logical vs real focus (the red-border bug).
            if wants {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::PARENT, w, self.last_event_time);
            } else {
                let _ =
                    self.conn
                        .set_input_focus(InputFocus::POINTER_ROOT, w, self.last_event_time);
            }
            if self.has_protocol(w, self.atoms.wm_take_focus)? {
                self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
            }
            // Commit the logical focus to `State` *before* reconciling against the
            // real X input focus below. `reconcile_focus()` compares the real X
            // focus (`get_input_focus`) to `mon.focused`; if `mon.focused` were
            // only written *after* that compare (its previous position, near the
            // end of this function), a focus request whose caller never updated
            // `mon.focused` first would leave it naming the window that was
            // focused on the just-left workspace. `ViewWorkspace` is the prime
            // example: it emits `FocusWindow(best_focus(mi))` but does not write
            // `mon.focused` itself (unlike `FocusDirection`). `reconcile_focus`
            // would then see logical != real and re-assert input focus onto that
            // previous, now hidden-but-viewable window — desyncing real focus from
            // the visible window (the reported "focus lost after returning to a
            // workspace" bug, only recoverable with h/l). Writing `mon.focused`
            // here makes logical == real for our own focus request, so the
            // reconcile is a no-op and the real focus stays on the intended
            // window. This matches the ordering already used by the `focus(None)`
            // branch, which writes `mon.focused` before its `reconcile_focus`.
            {
                let mon = &mut self.engine.state.monitors[mon_i];
                mon.focused = Some(w);
                mon.focus_stack.retain(|&x| x != w);
                mon.focus_stack.push(w);
            }
            // Verify the server accepted the focus (and fix it if an external
            // XSetInputFocus raced us). No polling: this runs only on a focus
            // action we just issued.
            self.reconcile_focus()?;

            // focused border color
            let col = if urgent {
                self.engine.cfg.col_urgent
            } else {
                self.engine.cfg.col_focused
            };
            let _ = self
                .conn
                .change_window_attributes(w, &ChangeWindowAttributesAux::new().border_pixel(col));
            self.grab_buttons(w, true)?;

            let serial = self.engine.state.next_serial();
            let was_urgent = if let Some(c) = self.engine.state.clients.get_mut(&w) {
                // Consume the urgency flag so its border color and the
                // `_NET_WM_STATE` demands-attention atom don't stick once the
                // window is actually focused.
                let was = c.flags.has(WinFlags::URGENT);
                if was {
                    c.flags.clear(WinFlags::URGENT);
                }
                c.focus_serial = serial;
                was
            } else {
                false
            };
            if was_urgent {
                self.write_net_wm_state(w);
            }

            #[cfg(feature = "input-trace")]
            itrace!(
                "focus() SET mon[{}].focused={:?} (was {:?}); x11_input_focus={:?}",
                mon_i,
                self.engine.state.monitors[mon_i].focused,
                prev_focused,
                self.engine.state.x11_input_focus
            );

            // Keep the maximize-overlay owner in sync with the new focus (single
            // writer; no read site infers it from `mon.focused`). Done after the
            // `itrace!` borrow above so it does not conflict with `mon`.
            self.engine.state.sync_presented_maximize(mon_i);

            // Keep the workspace's focused column/row in sync with the window
            // that is actually focused, and recenter the camera on it (niri-
            // style: focusing/clicking a window brings it to the centre).
            // Keep the focused column/row in sync with the window that is
            // actually focused, and recenter the camera so the focused column
            // is brought into view ("the camera looks at it"), exactly like the
            // `h`/`l` keyboard navigation — so a mouse click on a side tile
            // makes that tile usable, not stuck peeking off-screen.
            //
            // The recenter moves the just-focused window under the pointer; to
            // stop the *next* click (at the same spot) from landing on whatever
            // scrolled under the cursor, `on_button_press` warps the pointer
            // onto the newly-focused window after this runs. Keyboard navigation
            // recenters itself via `ideal_scroll` before calling `focus`, so it
            // stays unaffected.
            //
            // `retarget_focus_to_window` is the pure core helper the keyboard
            // path's `ideal_scroll` retarget also funnels through, and it is
            // `#[must_use]`: the monitor index it hands back is exactly the one
            // whose settled projection is still owed (`self.arrange` below), so
            // deleting that call turns the binding into an unused-variable
            // warning instead of a silent input-geometry regression.
            let retargeted = retarget_focus_to_window(&mut self.engine.state, &self.engine.cfg, w);

            // Keep X11 geometry (`client.geom`) in sync with the just-retargeted
            // camera. The keyboard focus path emits `ArrangeMonitor` before
            // `FocusWindow`, which makes `arrange` rewrite `client.geom` from the
            // new `camera.target`. The mouse path (`on_button_press`, `on_enter`,
            // `_NET_ACTIVE_WINDOW`) calls `focus()` directly with no
            // `ArrangeMonitor`, so `client.geom` was left pointing at the previous
            // settled position — and the next X hit-test (`find_client`) landed on
            // the wrong window. Projecting here closes that asymmetry: every
            // `camera.target` mutation now derives `client.geom` (design doc §5-§6).
            //
            // `arrange` only *reads* `camera.{target,position}` to compute
            // geometry; it never mutates the spring, so the compositor keeps
            // interpolating `position → target`. `hide_offscreen` is skipped while
            // a drag is in progress (guarded in `arrange_full_phase`), so a focus
            // change mid-drag can't un-hide/offscreen windows incorrectly.
            self.arrange(retargeted.unwrap_or(mon_i))?;

            // Overlay stacking (presented / popups-of-presented / peek).
            self.stack_overlay(mon_i);

            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[w],
            );

            if self.engine.cfg.warp_cursor {
                // `arrange` above rewrote `client.geom` to the new settled
                // position; warp onto *that* (not the stale pre-scroll `geom`
                // captured at the top), so the warped pointer lands on the
                // window we actually focused rather than wherever it slid from.
                let g = self
                    .engine
                    .state
                    .clients
                    .get(&w)
                    .map(|c| c.geom)
                    .unwrap_or(geom);
                let _ = self.conn.warp_pointer(
                    x11rb::NONE,
                    w,
                    0,
                    0,
                    0,
                    0,
                    (g.w / 2) as i16,
                    (g.h / 2) as i16,
                );
            }
        } else {
            // Only clear the focused window on the currently selected monitor.
            // Other monitors keep their own focused state independently.
            let sel = self.engine.state.sel_mon;
            if sel < self.engine.state.monitors.len() {
                if let Some(pw) = self.engine.state.monitors[sel].focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
                self.engine.state.monitors[sel].focused = None;
                // The maximize overlay loses its owner with the focus.
                self.engine.state.sync_presented_maximize(sel);
            }
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                self.root,
                self.last_event_time,
            );
            self.reconcile_focus()?;
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[x11rb::NONE],
            );
        }

        // Announce the transition on the typed EventBus. Every focus move —
        // pointer clicks, button grabs, EnterNotify, manage/unmanage re-focus,
        // commands — funnels through `focus`, so this is the single choke point.
        // `Window` and `WindowId` are both `u32` aliases, so the values pass
        // straight through to the core's id space.
        if prev_focused != valid_win {
            self.engine.notify(crate::core::event::Event::FocusChanged {
                from: prev_focused,
                to: valid_win,
            });
        }

        Ok(())
    }

    pub(super) fn unfocus(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let col = self.engine.cfg.col_normal;
        let _ = self
            .conn
            .change_window_attributes(win, &ChangeWindowAttributesAux::new().border_pixel(col));
        let _ = self.grab_buttons(win, false);
        Ok(())
    }

    pub(super) fn focus_best(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let candidate = self.engine.state.best_focus(mon_idx);
        self.focus(candidate)
    }

    /// Re-assert the WM's logical focus intent onto the X server and repaint
    /// borders so `logical == x11_input_focus == visual`.
    ///
    /// Event-driven only: called after we issue a `set_input_focus` (to verify
    /// the server accepted it) and from `FocusIn`/`FocusOut` handlers. It reads
    /// `GetInputFocus` to learn the real X focus, then — if it diverges from the
    /// logical focus (`mon.focused`) — re-issues `set_input_focus` and repaints
    /// the two affected borders. No polling, no per-frame work.
    pub(super) fn reconcile_focus(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Mirror the real X focus from the server. `None` means the root (no
        // client) is focused.
        let real = match self.conn.get_input_focus() {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => {
                    let f = reply.focus;
                    if f == 0 || f == self.root {
                        None
                    } else {
                        Some(f)
                    }
                }
                Err(_) => self.engine.state.x11_input_focus,
            },
            Err(_) => self.engine.state.x11_input_focus,
        };
        self.engine.state.x11_input_focus = real;

        // The WM's logical intent for the currently selected monitor.
        let logical = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);

        #[cfg(feature = "window-trace")]
        wtrace!(
            "reconcile_focus real={:?} logical(sel_mon={})={:?}",
            real,
            self.engine.state.sel_mon,
            logical
        );

        #[cfg(feature = "input-trace")]
        itrace!(
            "reconcile_focus real={:?} logical(sel_mon={})={:?}",
            real,
            self.engine.state.sel_mon,
            logical
        );

        if logical == real {
            #[cfg(feature = "input-trace")]
            itrace!("reconcile_focus OK (logical==real), no-op");
            return Ok(());
        }

        // Presentation-aware guard: after `manage()` records a `pending_focus`
        // behind the live overlay while a fullscreen/maximized overlay keeps the
        // real X input focus, `logical` (sel_mon's focus) may diverge from
        // `real` (the overlay) on purpose. Do NOT re-assert input focus toward
        // the logical window here — that would steal the keyboard from the live
        // overlay and re-introduce the exact input-theft this policy prevents.
        // Only bail when the *real* focus is the presented overlay itself.
        //
        // The guard is scoped to the monitor that OWNS `real` (resolved from the
        // window's `client.monitor`), NOT the global `sel_mon`. The old code used
        // `sel_mon`, so any FocusIn/Out anywhere bailed as long as *some* overlay
        // existed on the selected monitor — that made the guard global and froze
        // focus repair on every other monitor/workspace (invariant I3). Scoping to
        // `real`'s monitor confines the bail to the monitor that actually has the
        // overlay, exactly where the divergent focus is intentional.
        let guard_mon = real
            .and_then(|r| self.engine.state.clients.get(&r))
            .map(|c| c.monitor)
            .filter(|&m| m < self.engine.state.monitors.len())
            .unwrap_or(self.engine.state.sel_mon);
        if let Some(m) = self.engine.state.monitors.get(guard_mon) {
            if m.active_ws < m.workspaces.len() {
                if let Some(r) = real {
                    if self.engine.state.presented_overlay_owner(guard_mon) == Some(r) {
                        #[cfg(feature = "input-trace")]
                        itrace!(
                            "reconcile_focus BAIL guard: real={:#x} is a presented overlay on monitor={} active_ws={}",
                            r, guard_mon, m.active_ws
                        );
                        return Ok(());
                    }
                }
            }
        }

        // Divergence: re-assert the logical focus on X. This is exactly the
        // recovery path for the silent-focus-loss bug — an external
        // XSetInputFocus (popup/dialog/Wine/`_NET_ACTIVE_WINDOW` from another
        // tool) left real focus somewhere other than where the WM thinks it is.
        #[cfg(feature = "input-trace")]
        itrace!(
            "reconcile_focus REPAIR: re-asserting logical={:?} onto X (was real={:?}) on sel_mon={}",
            logical, real, self.engine.state.sel_mon
        );
        if let Some(w) = logical {
            if self.engine.state.clients.contains_key(&w) {
                let wants = match self.engine.state.clients.get(&w) {
                    Some(c) => c.wants_input,
                    None => true,
                };
                let mode = if wants {
                    InputFocus::PARENT
                } else {
                    InputFocus::POINTER_ROOT
                };
                let _ = self.conn.set_input_focus(mode, w, self.last_event_time);
                if self.has_protocol(w, self.atoms.wm_take_focus)? {
                    self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
                }
                let urgent = self
                    .engine
                    .state
                    .clients
                    .get(&w)
                    .is_some_and(|c| c.flags.has(WinFlags::URGENT));
                let col = if urgent {
                    self.engine.cfg.col_urgent
                } else {
                    self.engine.cfg.col_focused
                };
                let _ = self.conn.change_window_attributes(
                    w,
                    &ChangeWindowAttributesAux::new().border_pixel(col),
                );
            }
        } else {
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                self.root,
                self.last_event_time,
            );
        }

        // Repaint the previously-focused (now unfocused) window's border.
        if let Some(old) = real {
            if old != logical.unwrap_or(self.root) && self.engine.state.clients.contains_key(&old) {
                let _ = self.conn.change_window_attributes(
                    old,
                    &ChangeWindowAttributesAux::new().border_pixel(self.engine.cfg.col_normal),
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 22px-tall bar reserved at the top, so `workarea != screen` and the
    /// vertical clamp has a non-zero origin to respect.
    fn workarea() -> Rect {
        Rect::new(0, 22, 1920, 1058)
    }

    fn fits(g: Rect, wa: Rect, bw: u32) -> bool {
        let frame = 2 * bw as i32;
        g.x >= wa.x
            && g.y >= wa.y
            && g.x + g.w as i32 + frame <= wa.x + wa.w as i32
            && g.y + g.h as i32 + frame <= wa.y + wa.h as i32
    }

    #[test]
    fn float_inside_workarea_is_untouched() {
        let wa = workarea();
        let g = Rect::new(100, 200, 640, 480);
        assert_eq!(clamp_float_to_workarea(g, wa, 2), g);
    }

    #[test]
    fn float_past_the_edges_is_pulled_back() {
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(5000, 5000, 640, 480), wa, bw);
        assert!(
            fits(g, wa, bw),
            "off-screen float must be pulled inside: {g:?}"
        );
        assert_eq!(g.w, 640, "a float that fits keeps its size");
        assert_eq!(g.h, 480);

        let g = clamp_float_to_workarea(Rect::new(-500, -500, 640, 480), wa, bw);
        assert_eq!((g.x, g.y), (wa.x, wa.y));
    }

    #[test]
    fn float_larger_than_workarea_is_resized_and_stays_inside() {
        // The regression: with only x/y clamped, `max_x` lands below `min_x` for
        // an oversized float, so it was parked at a negative coordinate while
        // still overflowing the screen.
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(0, 0, 5000, 5000), wa, bw);
        assert!(
            fits(g, wa, bw),
            "an oversized float must be shrunk into the workarea, got {g:?}"
        );
        assert_eq!((g.x, g.y), (wa.x, wa.y));
        assert_eq!(g.w, wa.w - 2 * bw);
        assert_eq!(g.h, wa.h - 2 * bw);
    }

    #[test]
    fn zero_sized_workarea_never_produces_a_zero_dimension() {
        // Defensive: a degenerate workarea (mid-hotplug) must not yield a
        // width/height of 0, which X11 rejects with a BadValue.
        let wa = Rect::new(0, 0, 1, 1);
        let g = clamp_float_to_workarea(Rect::new(10, 10, 800, 600), wa, 4);
        assert!(g.w >= 1 && g.h >= 1);
        assert_eq!((g.x, g.y), (wa.x, wa.y));
    }

    // ── Riesgo 1: float ConfigureRequest must be normalized at the WM boundary ──
    // These exercise the exact policy applied in on_configure_request's float
    // branch: a client may size/move itself, but the rect is always run through
    // clamp_float_to_workarea before it becomes client.geom / Applied / X11.

    #[test]
    fn float_zero_size_request_is_clamped_to_minimum() {
        let wa = workarea();
        let bw = 2;
        // A client requesting 0x0 must never reach X11 as 0x0 (BadValue). The WM
        // policy floors the degenerate request to the X11-valid minimum.
        let g = clamp_float_to_workarea(Rect::new(100, 100, 0, 0), wa, bw);
        assert!(
            g.w >= 1 && g.h >= 1,
            "0x0 request must get a non-zero floor"
        );
        assert!(
            fits(g, wa, bw),
            "0x0-clamped float must still sit in workarea"
        );
    }

    #[test]
    fn float_partially_offscreen_is_pulled_back() {
        let wa = workarea();
        let bw = 2;
        // Horizontally inside but extends past the right/bottom edge.
        let g = clamp_float_to_workarea(Rect::new(1900, 1000, 640, 480), wa, bw);
        assert!(
            fits(g, wa, bw),
            "partially off-screen float must be pulled back: {g:?}"
        );
        assert_eq!(g.w, 640, "a float that fits keeps its size");
        assert_eq!(g.h, 480);
    }

    #[test]
    fn float_normal_resize_inside_is_honored() {
        let wa = workarea();
        let bw = 2;
        // A moderate resize that still fits is exactly the client's desired rect.
        let req = Rect::new(300, 300, 800, 600);
        let g = clamp_float_to_workarea(req, wa, bw);
        assert_eq!(g, req, "a fitting resize request is honored as-is");
    }

    #[test]
    fn float_huge_request_is_shrunk_to_workarea() {
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(50, 50, 9000, 7000), wa, bw);
        assert!(
            fits(g, wa, bw),
            "huge request must shrink into workarea: {g:?}"
        );
        assert_eq!(g.w, wa.w - 2 * bw);
        assert_eq!(g.h, wa.h - 2 * bw);
    }

    // ── Riesgo 5: transient-chain depth limit ──────────────────────────────────
    // `MAX_TRANSIENT_DEPTH` bounds the ownership walk because `WM_TRANSIENT_FOR`
    // is unvalidated client input (self-loops and cycles are expressible). These
    // pin the exact behaviour at, and beyond, the bound: reaching the root is
    // true up to depth 4 and false at depth 5 (fail-safe: the deep popup is not
    // raised above the overlay), and a cyclic chain always terminates.

    /// Chain `1 → 2 → … → depth+1`, where each window is transient for the
    /// previous one, so window `depth + 1` sits exactly `depth` links below the
    /// root window `1`. Returns the map and the deepest window's id.
    fn chain(depth: u32) -> (std::collections::HashMap<WindowId, Client>, WindowId) {
        let mut clients = std::collections::HashMap::new();
        let mut prev: Option<WindowId> = None;
        for w in 1..=(depth + 1) {
            let mut c = Client::new(w, 0, 0);
            c.transient_parent = prev;
            clients.insert(w, c);
            prev = Some(w);
        }
        (clients, depth + 1)
    }

    #[test]
    fn transient_chain_depth1_reaches_the_root() {
        let (clients, leaf) = chain(1);
        assert_eq!(leaf, 2);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "a direct dialog of the overlay is owned by it"
        );
        // The root itself is not transient of anything.
        assert!(!transient_chain_reaches(&clients, 1, &[1]));
        // An unknown window has no chain at all.
        assert!(!transient_chain_reaches(&clients, 99, &[1]));
    }

    #[test]
    fn transient_chain_depth2_reaches_the_root() {
        let (clients, leaf) = chain(2);
        assert_eq!(leaf, 3);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "popup-of-popup is still owned by the root"
        );
        assert!(
            transient_chain_reaches(&clients, leaf, &[2]),
            "…and by its direct parent"
        );
    }

    #[test]
    fn transient_chain_depth4_at_the_limit_reaches_the_root() {
        let (clients, leaf) = chain(MAX_TRANSIENT_DEPTH as u32);
        assert_eq!(leaf, 5);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "depth {MAX_TRANSIENT_DEPTH} is the last depth that still resolves"
        );
        // Every intermediate link resolves too.
        for root in 2..=4 {
            assert!(transient_chain_reaches(&clients, leaf, &[root]));
        }
    }

    #[test]
    fn transient_chain_depth5_beyond_the_limit_is_fail_safe() {
        let (clients, leaf) = chain(MAX_TRANSIENT_DEPTH as u32 + 1);
        assert_eq!(leaf, 6);
        assert!(
            !transient_chain_reaches(&clients, leaf, &[1]),
            "depth 5 exceeds the bound: ownership is denied, not looped"
        );
        // Fail-SAFE, not fail-open: the deep window still resolves against every
        // ancestor inside the bound, and the rest of the chain is unaffected.
        for root in 2..=5 {
            assert!(
                transient_chain_reaches(&clients, leaf, &[root]),
                "ancestor {root} is within {MAX_TRANSIENT_DEPTH} links of the leaf"
            );
        }
        assert!(
            transient_chain_reaches(&clients, 5, &[1]),
            "the depth-4 window is unaffected by its deeper child"
        );
    }

    #[test]
    fn transient_chain_cycle_terminates() {
        // Self-loop: WM_TRANSIENT_FOR pointing at the window itself.
        let mut clients = std::collections::HashMap::new();
        let mut c = Client::new(1, 0, 0);
        c.transient_parent = Some(1);
        clients.insert(1, c);
        assert!(!transient_chain_reaches(&clients, 1, &[42]));
        assert!(transient_chain_reaches(&clients, 1, &[1]));

        // Two-window cycle: 1 ↔ 2, neither reaches an unrelated root.
        let mut clients = std::collections::HashMap::new();
        let mut a = Client::new(1, 0, 0);
        a.transient_parent = Some(2);
        let mut b = Client::new(2, 0, 0);
        b.transient_parent = Some(1);
        clients.insert(1, a);
        clients.insert(2, b);
        assert!(!transient_chain_reaches(&clients, 1, &[42]));
        assert!(!transient_chain_reaches(&clients, 2, &[42]));
    }

    #[test]
    fn transient_chain_stops_at_a_destroyed_parent() {
        // The middle of a depth-3 chain is gone (the walk must end, not panic).
        let (mut clients, leaf) = chain(3);
        clients.remove(&2);
        assert!(
            !transient_chain_reaches(&clients, leaf, &[1]),
            "a hole in the chain ends the walk"
        );
        assert!(
            transient_chain_reaches(&clients, leaf, &[3]),
            "the surviving part of the chain still resolves"
        );
    }

    // ── Riesgo 4: "covering fullscreen" must never be conflated with "overlay owner" ──
    // A `Column` fullscreen covers the screen (it is the covering fullscreen used
    // by hit-testing) but is NOT the presentation overlay owner. A `presented_maximize`
    // window IS the overlay owner but is NOT fullscreen and NOT a covering fullscreen.
    #[test]
    fn covering_fullscreen_and_overlay_owner_are_distinct() {
        use crate::types::{Client, LayoutKind, Monitor, Rect, State, WinFlags};

        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        // The covering concept is, by definition, a Column-ribbon participant.
        mon.workspaces[0].layout = LayoutKind::Column;
        state.monitors.push(mon);

        // Window 1: a Column fullscreen tile.
        let mut c1 = Client::new(1, 0, 0);
        c1.geom = Rect::new(0, 0, 100, 100);
        c1.flags.set(WinFlags::FULLSCREEN);
        state.add_client(c1);
        state.monitors[0].workspaces[0].add_tiled(1, 0.5);
        state.monitors[0].workspaces[0].focus.column_idx = 0;
        state.monitors[0].focused = Some(1);
        state.monitors[0].focus_stack.push(1);

        // A Column fullscreen COVERS the screen (covering fullscreen for hit-testing)...
        assert_eq!(
            state.covering_fullscreen_window(0),
            Some(1),
            "focused Column fullscreen is the covering fullscreen"
        );
        // ...but it is NOT the presentation overlay owner (Column ribbon tile, not overlay).
        assert_eq!(
            state.presented_overlay_owner(0),
            None,
            "a Column fullscreen is covering but NOT the overlay owner"
        );
        assert!(
            state.clients.get(&1).unwrap().is_fullscreen(),
            "it is still fullscreen in the LayoutKind sense"
        );

        // Window 3: a transient/modal popup belonging to the fullscreen app, drawn
        // on top of it. It is neither covering nor the overlay owner.
        let mut c3 = Client::new(3, 0, 0);
        c3.geom = Rect::new(50, 50, 80, 60);
        c3.flags.set(WinFlags::FLOAT);
        c3.transient_parent = Some(1);
        state.add_client(c3);
        state.monitors[0].workspaces[0].floats.push(3);
        state.monitors[0].focused = Some(3);
        state.monitors[0].focus_stack.push(3);
        assert_eq!(
            state.covering_fullscreen_window(0),
            Some(1),
            "the popup sits on top, but the fullscreen is still the covering window"
        );
        assert_eq!(
            state.presented_overlay_owner(0),
            None,
            "a Column fullscreen + its popup are still not an overlay owner"
        );

        // Window 2: a maximized (presented_maximize) window. It IS the overlay
        // owner, but is NOT fullscreen and NOT a covering fullscreen.
        let mut c2 = Client::new(2, 0, 0);
        c2.geom = Rect::new(0, 0, 100, 100);
        c2.flags.set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        state.add_client(c2);
        state.monitors[0].workspaces[0].add_tiled(2, 0.5);
        state.monitors[0].focused = Some(2);
        state.monitors[0].focus_stack.push(2);
        state.sync_presented_maximize(0);

        assert_eq!(
            state.presented_overlay_owner(0),
            Some(2),
            "a presented_maximize window IS the overlay owner"
        );
        assert!(
            !state.clients.get(&2).unwrap().is_fullscreen(),
            "a presented_maximize window is NOT LayoutKind-fullscreen"
        );
        assert_eq!(
            state.covering_fullscreen_window(0),
            None,
            "a presented_maximize window is NOT a covering fullscreen"
        );
    }
}
