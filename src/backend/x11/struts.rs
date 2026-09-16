//! Dock strut → workarea reservation.
//!
//! Reads `_NET_WM_STRUT_PARTIAL` (preferred, 12 CARDINAL)
//! or `_NET_WM_STRUT` (fallback, 4 CARDINAL) from dock
//! windows and reserves the corresponding screen regions.
//! Reserved regions shrink the workarea used for tiled
//! layout.
//!
//! # Protocol semantics
//!
//! `_NET_WM_STRUT_PARTIAL` defines reserved edges per
//! monitor: left/right/top/bottom in pixels, plus
//! `left_start_y`/`left_end_y` etc. for partial struts.
//! Maverick collapses all regions into a single
//! `ReservedArea` (saturating add per edge).
//!
//! # Monitor assignment
//!
//! `monitor_for_strut` uses the strut's perpendicular
//! midpoint + window centre to assign the dock to a
//! monitor, falling back to `mon_at` (window geometry
//! centre) then monitor 0.
//!
//! # Camera retarget
//!
//! When a dock is added/removed, `retarget_cameras`
//! recalculates `ideal_scroll` for every workspace on
//! the affected monitor using the spring target
//! (not a snap), so the scroll position animates
//! smoothly into the new workarea.
//!
//! # Invariants
//!
//! - A dock's previous reservation is cleared before
//!   the new one is set (no accumulation).
//! - `arrange` + `update_workarea` are called after
//!   every strut change so the layout and EWMH
//!   properties stay consistent.

use super::*;
use crate::core::layout::fs_ctx;

impl WindowManager {
    /// Read a window's strut as a list of (edge, thickness). A single dock may
    /// reserve space on more than one edge (e.g. a panel + a launcher reserving
    /// `top` *and* `left`), so this returns every non-zero edge instead of a
    /// single priority-picked one (bug B4). Returns `None` when the window has
    /// neither strut property.
    pub(super) fn read_strut(&self, win: Window) -> Option<Vec<(Edge, u32)>> {
        // Prefer _NET_WM_STRUT_PARTIAL; fall back to _NET_WM_STRUT.
        let partial = self
            .conn
            .get_property(
                false,
                win,
                self.atoms.net_wm_strut_partial,
                AtomEnum::CARDINAL,
                0,
                12,
            )
            .ok()?
            .reply()
            .ok();
        if let Some(p) = partial {
            if p.type_ == u32::from(AtomEnum::CARDINAL) {
                let v: Vec<u32> = p
                    .value32()
                    .map(std::iter::Iterator::collect)
                    .unwrap_or_default();
                if v.len() >= 4 {
                    return strut_edge(&v);
                }
            }
        }

        let basic = self
            .conn
            .get_property(
                false,
                win,
                self.atoms.net_wm_strut,
                AtomEnum::CARDINAL,
                0,
                4,
            )
            .ok()?
            .reply()
            .ok()?;
        if basic.type_ == u32::from(AtomEnum::CARDINAL) {
            let v: Vec<u32> = basic
                .value32()
                .map(std::iter::Iterator::collect)
                .unwrap_or_default();
            if v.len() >= 4 {
                return strut_edge(&v);
            }
        }
        None
    }

    /// Pick the monitor a strut belongs to. Uses the dock's reserved-extent
    /// centre to find the containing monitor, falling back to the window
    /// geometry centre and then the primary monitor.
    ///
    /// The extent centre is taken from `_NET_WM_STRUT_PARTIAL`'s start/end
    /// fields (bug B4): a dock that covers only part of an edge — or spans two
    /// monitors — is attributed to the monitor actually containing its span,
    /// not just its window centre.
    pub(super) fn monitor_for_strut(&self, win: Window, struts: &[(Edge, u32)]) -> usize {
        // Hostile CARDINALs can exceed i32::MAX: `as i32` would wrap them
        // negative and misattribute the dock. Saturate instead.
        fn sat(v: u32) -> i32 {
            v.min(i32::MAX as u32) as i32
        }
        let geom = || -> Option<(i32, i32)> {
            if let Ok(Ok(g)) = self
                .conn
                .get_geometry(win)
                .map(x11rb::cookie::Cookie::reply)
            {
                Some((
                    g.x as i32 + g.width as i32 / 2,
                    g.y as i32 + g.height as i32 / 2,
                ))
            } else {
                None
            }
        };
        // Candidate points, one per reserved edge: a multi-edge dock
        // (`top` + `left`) must be attributable by ANY of its spans, not
        // just `struts.first()`.
        let mut candidates: Vec<(i32, i32)> = Vec::new();
        if let Some(p) = self
            .conn
            .get_property(
                false,
                win,
                self.atoms.net_wm_strut_partial,
                AtomEnum::CARDINAL,
                0,
                12,
            )
            .ok()
            .and_then(|c| c.reply().ok())
        {
            if p.type_ == u32::from(AtomEnum::CARDINAL) {
                let v: Vec<u32> = p
                    .value32()
                    .map(std::iter::Iterator::collect)
                    .unwrap_or_default();
                if v.len() >= 12 {
                    if let Some((cx, cy)) = geom() {
                        for (edge, _) in struts {
                            // Midpoint of the dock's span along the *perpendicular*
                            // axis; the coordinate on the edge axis comes from the
                            // window centre.
                            let span_mid = match edge {
                                Edge::Left | Edge::Right => {
                                    let s = sat(v[4]);
                                    let e = sat(v[5]);
                                    if e > s {
                                        Some((s + e) / 2)
                                    } else {
                                        Some(s)
                                    }
                                }
                                Edge::Top | Edge::Bottom => {
                                    let s = sat(v[8]);
                                    let e = sat(v[9]);
                                    if e > s {
                                        Some((s + e) / 2)
                                    } else {
                                        Some(s)
                                    }
                                }
                            };
                            candidates.push(match edge {
                                Edge::Left | Edge::Right => (cx, span_mid.unwrap_or(cy)),
                                Edge::Top | Edge::Bottom => (span_mid.unwrap_or(cx), cy),
                            });
                        }
                    }
                }
            }
        }
        // First monitor containing ANY candidate span point wins.
        for &(cx, cy) in &candidates {
            for (i, m) in self.engine.state.monitors.iter().enumerate() {
                let s = &m.screen;
                if cx >= s.x
                    && cx < s.x.saturating_add(s.w.min(i32::MAX as u32) as i32)
                    && cy >= s.y
                    && cy < s.y.saturating_add(s.h.min(i32::MAX as u32) as i32)
                {
                    return i;
                }
            }
        }
        let (cx, cy) = geom().unwrap_or((0, 0));
        for (i, m) in self.engine.state.monitors.iter().enumerate() {
            let s = &m.screen;
            if cx >= s.x && cx < s.x + s.w as i32 && cy >= s.y && cy < s.y + s.h as i32 {
                return i;
            }
        }
        0
    }

    /// Recompute each of `mon_idx`'s workspaces' camera target against the
    /// current workarea. Called after a strut change resizes the workarea:
    /// without this, `ws.camera.position` stays at its old pixel value while
    /// the ribbon re-lays-out at the new width, so the focused column drifts
    /// out of alignment and sits there — silently wrong — until some later
    /// focus/grow command happens to call `ideal_scroll` itself and the
    /// camera has to cover the whole accumulated gap in one animated jump
    /// (bug: looks like a sudden bounce, is really a stale target).
    /// Uses `target`, not `snap`, so the correction still eases in via the
    /// normal spring instead of teleporting.
    fn retarget_cameras(&mut self, mon_idx: usize) {
        if mon_idx >= self.engine.state.monitors.len() {
            return;
        }
        // Destructure `State` into disjoint field borrows so we can read
        // `clients` (for the fullscreen descriptor) and mutate `monitors`
        // (the camera targets) at the same time without fighting the borrow
        // checker.
        let State {
            clients, monitors, ..
        } = &mut self.engine.state;
        let screen = monitors[mon_idx].screen;
        let wa = monitors[mon_idx].workarea;
        let cfg = &self.engine.cfg;
        for ws in &mut monitors[mon_idx].workspaces {
            let fs = fs_ctx(clients, ws, screen);
            ws.camera.target = ideal_scroll(ws, cfg, wa, fs);
        }
    }

    /// Read `win`'s strut and, if it reserves space, register/refresh a
    /// `ReservedRegion` for it and re-arrange affected monitors.
    pub(super) fn apply_dock_strut(
        &mut self,
        win: Window,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match self.read_strut(win) {
            Some(struts) if !struts.is_empty() => {
                if self.engine.state.monitors.is_empty() {
                    return Ok(());
                }
                let mi = self
                    .monitor_for_strut(win, &struts)
                    .min(self.engine.state.monitors.len() - 1);
                // Register every reserved edge at once (bug B4): a single dock
                // may reserve `top` *and* `left`, and `set_reserved_region`
                // clears the owner's previous region — so all edges must go in
                // one call or the second would erase the first.
                self.engine.state.monitors[mi].set_reserved_regions(win, &struts);
                if self.docks.insert(win, mi).is_none() {
                    // Newly tracked dock: watch for later strut / destroy changes.
                    let _ = self.conn.change_window_attributes(
                        win,
                        &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                    );
                }
                // Retarget the camera *before* projecting, so `arrange` writes
                // `client.geom` from the new (post-strut) scroll target — not the
                // stale one. Otherwise the dock change leaves geometry on the old
                // target until the next unrelated arrange (invariant: every
                // `camera.target` mutation must precede the settled projection).
                self.retarget_cameras(mi);
                self.arrange(mi)?;
                self.update_workarea()?;
            }
            _ => {
                // No (longer any) strut — drop a previous reservation if present.
                self.remove_dock(win)?;
            }
        }
        Ok(())
    }

    /// Remove any reservation owned by a dock window (on destroy/unmap or when
    /// its strut is cleared). Re-arranges the affected monitor.
    pub(super) fn remove_dock(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(mi) = self.docks.remove(&win) {
            if mi < self.engine.state.monitors.len() {
                self.engine.state.monitors[mi].remove_reserved_region(win);
                // Same ordering rule as `apply_dock_strut`: retarget before
                // projecting so the settled geometry follows the new target.
                self.retarget_cameras(mi);
                self.arrange(mi)?;
                self.update_workarea()?;
            }
        }
        Ok(())
    }
}
