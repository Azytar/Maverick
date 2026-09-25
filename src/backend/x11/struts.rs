//! Dock strut → workarea reservation.
//!
//! Reads `_NET_WM_STRUT_PARTIAL` (preferred, 12 CARDINAL) or `_NET_WM_STRUT`
//! (fallback, 4 CARDINAL) from dock windows and reserves the corresponding
//! screen regions. Reserved regions shrink the workarea used for tiled layout.
//!
//! # Protocol semantics
//!
//! Both properties start with `[left, right, top, bottom]`; `_PARTIAL` appends a
//! start/end span per edge along the perpendicular axis. One dock may reserve
//! several edges at once, so every non-zero edge is read — collapsing them
//! would drop a panel that reserves both `top` and `left`. All regions are
//! summed into a single `ReservedArea` (saturating add per edge), so two docks
//! on the same edge both take effect.
//!
//! # Monitor assignment
//!
//! `monitor_for_strut` attributes a dock by a point inside its reserved span
//! (the strut's perpendicular midpoint combined with the window centre), which
//! is what makes a partial-edge dock land on the monitor that actually shows
//! it. Failing that it falls back to the window-geometry centre, then to
//! monitor 0.
//!
//! # Camera retarget
//!
//! When a dock is added or removed the workarea changes, so `retarget_cameras`
//! recomputes `ideal_scroll` for every workspace of the affected monitor and
//! re-targets (does not snap) the camera: the scroll position eases into the
//! new workarea instead of teleporting.
//!
//! # Invariants
//!
//! - A dock's previous reservation is replaced, never accumulated.
//! - `retarget_cameras` runs *before* `arrange`, so the geometry `arrange`
//!   writes comes from the new camera target and not the stale one.
//! - `arrange` + `update_workarea` run after every strut change, so the layout
//!   and the published EWMH properties stay consistent with the dock.

use super::*;
use crate::core::layout::fs_ctx;

impl WindowManager {
    /// Read a window's strut as a list of (edge, thickness), preferring
    /// `_NET_WM_STRUT_PARTIAL` and falling back to `_NET_WM_STRUT`. A single
    /// dock may reserve space on more than one edge (a panel plus a launcher
    /// reserving `top` *and* `left`), so this returns every non-zero edge
    /// instead of a single priority-picked one. Returns `None` when the window
    /// has neither strut property.
    pub(super) fn read_strut(&self, win: Window) -> Option<Vec<(Edge, u32)>> {
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
    /// geometry centre and then to monitor 0.
    ///
    /// The extent centre is taken from `_NET_WM_STRUT_PARTIAL`'s start/end
    /// fields: a dock that covers only part of an edge — or spans two monitors —
    /// is attributed to the monitor actually containing its span, not just its
    /// window centre.
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
    /// current workarea, using `target` (not `snap`) so the correction eases in
    /// through the normal spring.
    ///
    /// Called after a strut change resizes the workarea. Without it the camera
    /// keeps its old pixel target while the ribbon re-lays-out at the new width,
    /// so the focused column drifts out of alignment and stays there until some
    /// later focus/grow command happens to call `ideal_scroll` itself — at
    /// which point the camera covers the whole accumulated gap in one animated
    /// jump, which reads as a sudden bounce rather than a stale target.
    fn retarget_cameras(&mut self, mon_idx: usize) {
        if mon_idx >= self.engine.state.monitors.len() {
            return;
        }
        // Split borrow of `State` so `clients` (for the fullscreen descriptor)
        // and `monitors` (the camera targets) can be read and written together.
        let State {
            clients, monitors, ..
        } = &mut self.engine.state;
        let screen = monitors[mon_idx].screen;
        let wa = monitors[mon_idx].workarea;
        let cfg = &self.engine.cfg;
        for ws in &mut monitors[mon_idx].workspaces {
            let fs = fs_ctx(clients, ws, screen);
            ws.camera.retarget(ideal_scroll(ws, cfg, wa, fs));
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
                // Register every reserved edge in one call: a single dock may
                // reserve `top` *and* `left`, and `set_reserved_regions` drops
                // the owner's previous regions — two calls would erase each
                // other's edge.
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
