use super::*;
use crate::core::layout::fs_ctx;

impl WindowManager {
    /// Read a window's strut as (edge, thickness). Returns `None` when the window
    /// has neither strut property (i.e. it reserves no space).
    pub(super) fn read_strut(&self, win: Window) -> Option<(Edge, u32)> {
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

    /// Pick the monitor a strut belongs to. Uses the dock window's geometry
    /// centre to find the containing monitor, falling back to the primary.
    pub(super) fn monitor_for_strut(&self, win: Window) -> usize {
        if let Ok(Ok(g)) = self
            .conn
            .get_geometry(win)
            .map(x11rb::cookie::Cookie::reply)
        {
            let cx = g.x as i32 + g.width as i32 / 2;
            let cy = g.y as i32 + g.height as i32 / 2;
            for (i, m) in self.engine.state.monitors.iter().enumerate() {
                let s = &m.screen;
                if cx >= s.x && cx < s.x + s.w as i32 && cy >= s.y && cy < s.y + s.h as i32 {
                    return i;
                }
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
        let State { clients, monitors, .. } = &mut self.engine.state;
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
            Some((edge, thickness)) if thickness > 0 => {
                let mi = self.monitor_for_strut(win);
                self.engine.state.monitors[mi].set_reserved_region(win, edge, thickness);
                if self.docks.insert(win, mi).is_none() {
                    // Newly tracked dock: watch for later strut / destroy changes.
                    let _ = self.conn.change_window_attributes(
                        win,
                        &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                    );
                }
                self.arrange(mi)?;
                self.retarget_cameras(mi);
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
                self.arrange(mi)?;
                self.retarget_cameras(mi);
                self.update_workarea()?;
            }
        }
        Ok(())
    }
}
