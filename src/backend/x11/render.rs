use super::*;
use crate::core::commands::retarget_focus_to_window;
use crate::core::layout::fs_ctx;
use crate::core::layout::Phase;
use crate::core::present::present_into;
use x11rb::protocol::shape;


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
fn clamp_float_to_workarea(mut g: Rect, wa: Rect, bw: u32) -> Rect {
    let frame = 2 * bw as i32;
    let max_w = (wa.w as i32 - frame).max(1) as u32;
    let max_h = (wa.h as i32 - frame).max(1) as u32;
    g.w = g.w.min(max_w);
    g.h = g.h.min(max_h);
    let max_x = (wa.x + wa.w as i32 - g.w as i32 - frame).max(wa.x);
    let max_y = (wa.y + wa.h as i32 - g.h as i32 - frame).max(wa.y);
    g.x = g.x.clamp(wa.x, max_x);
    g.y = g.y.clamp(wa.y, max_y);
    g
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
            self.apply_geom(win, g, bw)?;
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
            &mut self.placements_buf,
            &mut self.ribbon_scratch,
        );
        // Presentation layer: apply the fullscreen/maximized overlay in place.
        present_into(
            &self.engine.state,
            &self.engine.state.monitors[mon_idx],
            &mut self.placements_buf,
            &mut self.present_scratch,
        );
        // Collect into a local so the immutable borrow of `placements_buf`
        // ends before `apply_geom` mutates `self`.
        let placements: Vec<_> = self.placements_buf.to_vec();
        for &(win, geom, bw) in &placements {
            self.apply_geom(win, geom, bw)?;
        }
        self.placements_buf.clear();
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

        for win in self.hide_mon_vec.drain(..) {
            let in_ws = self.hide_ws_set.contains(&win);
            let client = match self.engine.state.clients.get_mut(&win) {
                Some(c) => c,
                None => continue,
            };
            if !in_ws && !client.wm_hidden {
                let w = client.geom.w.min(i32::MAX as u32) as i32;
                let off_x = w.saturating_add(200).saturating_neg();
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().x(off_x));
                client.wm_hidden = true;
                // Only hide from the compositor when the window truly lives on
                // another workspace. A fullscreen window in the *active*
                // workspace is deliberately excluded from `hide_ws_set` (it is a
                // participant of the scrolling ribbon, not an overlay) so it is
                // never physically hidden — marking it hidden here would make it
                // vanish. `client.workspace` is the authoritative membership.
                if client.workspace != self.engine.state.monitors[mon_idx].active_ws {
                    if let Some(c) = self.compositor.as_mut() {
                        c.set_hidden(win, true);
                    }
                }
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().x(gx).y(gy));
                client.wm_hidden = false;
                if let Some(c) = self.compositor.as_mut() {
                    c.set_hidden(win, false);
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

        // Derived fullscreen descriptor, shared with `core::layout`.
        let fs = fs_ctx(&self.engine.state.clients, ws, mon.screen);

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
                let focused = mon.focused;
                self.engine.state.clients.get(win).is_some_and(|c| {
                    (c.is_fullscreen() && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
                        || ((c.is_maximized_v() || c.is_maximized_h()) && focused == Some(*win))
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
        let covering = ws.layout == LayoutKind::Column
            && !ws.overview
            && fs.win.is_some()
            && (ws.camera.position - ws.camera.target).abs() < 0.5
            && ws.camera.velocity.abs() < 0.01
            && (ws.zoom - ws.zoom_target).abs() < 0.001;

        // `fs.win` is the *focused* fullscreen window (or `None`); exactly one
        // fullscreen column is raised above the dock at a time — the one you are
        // looking at. Track the transition so moving focus between fullscreen
        // columns drops the old one and raises the new one.
        let new_cover = if covering { fs.win } else { None };
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
    /// Walks at most `MAX_TRANSIENT_DEPTH` links to survive popup-of-popup
    /// ownership without risking cycles.
    pub(super) fn transient_of(&self, win: WindowId, roots: &[WindowId]) -> bool {
        const MAX_TRANSIENT_DEPTH: usize = 4;
        let mut cur = self
            .engine
            .state
            .clients
            .get(&win)
            .and_then(|c| c.transient_parent);
        for _ in 0..MAX_TRANSIENT_DEPTH {
            let Some(p) = cur else {
                return false;
            };
            if roots.contains(&p) {
                return true;
            }
            cur = self
                .engine
                .state
                .clients
                .get(&p)
                .and_then(|c| c.transient_parent);
        }
        false
    }

    pub(super) fn apply_geom(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match self.engine.state.clients.get(&win) {
            Some(c) => c,
            None => return Ok(()),
        };
        // Monitor whose live projection is invalidated by this geometry write.
        let mon = client.monitor;
        // A pending state transition (fullscreen/maximized on or off) forces the
        // reconfigure even when the rect is unchanged — see
        // `Client::geometry_dirty`. Otherwise identical geometry is skipped,
        // which is what keeps `arrange` from spamming the server every frame.
        if !client.geometry_dirty && geom == client.geom && bw == client.border_w {
            return Ok(());
        }
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
            c.geom = geom;
            c.border_w = bw;
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

    /// Raise `win` above all its siblings (`TopLevel`). Fire-and-forget: called
    /// from arrange/focus paths where a synchronous RTT per window is not acceptable.
    fn raise(&self, win: Window) {
        let _ = self
            .conn
            .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }

    pub(super) fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

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

            // set X11 input focus
            if wants {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::PARENT, w, x11rb::CURRENT_TIME);
            } else {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME);
            }
            if self.has_protocol(w, self.atoms.wm_take_focus)? {
                self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
            }

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

            let mon = &mut self.engine.state.monitors[mon_i];
            mon.focused = Some(w);
            mon.focus_stack.retain(|&x| x != w);
            mon.focus_stack.push(w);

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
            let retargeted =
                retarget_focus_to_window(&mut self.engine.state, &self.engine.cfg, w);

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
            }
            let _ =
                self.conn
                    .set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME);
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
}
