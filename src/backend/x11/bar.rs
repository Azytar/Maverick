use super::*;

impl WindowManager {
    /// Mark monitor `mi` as needing a bar repaint.
    /// Actual drawing is deferred to `flush_bars()`, called once per event batch.
    #[inline]
    /// Create (or recreate) the internal bar window + GC for one monitor and
    /// register it on the monitor. Only compiled with the `internal-bar` feature.
    #[cfg(feature = "internal-bar")]
    pub(super) fn create_bar_window(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let setup = self.conn.setup();
        let scr = &setup.roots[self.screen_num];
        let depth = scr.root_depth;
        let visual = scr.root_visual;

        let (bar_h, top, scr_x, scr_w, bar_y) = {
            let m = &self.engine.state.monitors[mon_idx];
            (
                self.engine.cfg.bar_height,
                self.engine.cfg.top_bar,
                m.screen.x,
                m.screen.w,
                m.bar_y(),
            )
        };

        let bar_win = self.conn.generate_id()?;
        self.conn
            .create_window(
                depth,
                bar_win,
                self.root,
                scr_x as i16,
                bar_y as i16,
                scr_w as u16,
                bar_h as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &CreateWindowAux::new()
                    .background_pixel(self.engine.cfg.col_bar_bg)
                    .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS)
                    .override_redirect(1u32),
            )?
            .check()?;

        self.conn
            .change_property32(
                PropMode::REPLACE,
                bar_win,
                self.atoms.net_wm_window_type,
                AtomEnum::ATOM,
                &[self.atoms.net_wm_window_type_dock],
            )?
            .check()?;

        let strut = if top {
            [
                0u32,
                0,
                bar_h,
                0,
                0,
                0,
                0,
                0,
                scr_x as u32,
                (scr_x + scr_w as i32) as u32,
                0,
                0,
            ]
        } else {
            [
                0u32,
                0,
                0,
                bar_h,
                0,
                0,
                0,
                0,
                0,
                0,
                scr_x as u32,
                (scr_x + scr_w as i32) as u32,
            ]
        };
        self.conn
            .change_property32(
                PropMode::REPLACE,
                bar_win,
                self.atoms.net_wm_strut_partial,
                AtomEnum::CARDINAL,
                &strut,
            )?
            .check()?;

        let gc = self.conn.generate_id()?;
        self.conn
            .create_gc(
                gc,
                bar_win,
                &CreateGCAux::new()
                    .foreground(self.engine.cfg.col_bar_fg)
                    .background(self.engine.cfg.col_bar_bg)
                    .font(self.bar.font_id),
            )?
            .check()?;

        self.conn.map_window(bar_win)?.check()?;

        self.engine.state.monitors[mon_idx].bar_win = Some(bar_win);
        self.engine.state.monitors[mon_idx].bar_gc = Some(gc);
        Ok(())
    }

    pub(super) fn mark_bar(&mut self, mi: usize) {
        if mi < 64 {
            self.bar_dirty |= 1u64 << mi;
        }
    }

    /// Mark all monitors dirty (e.g. on status/layout change).
    #[inline]
    pub(super) fn mark_all_bars(&mut self) {
        let n = self.engine.state.monitors.len().min(64);
        if n == 64 {
            self.bar_dirty = u64::MAX;
        } else {
            self.bar_dirty |= (1u64 << n) - 1;
        }
    }

    /// Paint every dirty bar. Called once at the top of each event-loop iteration,
    /// after all pending events have been drained from the socket.
    pub(super) fn flush_bars(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // P5: Batch-update _NET_CLIENT_LIST before flushing bars
        if self.client_list_dirty {
            self.client_list_dirty = false;
            self.update_client_list()?;
        }
        if self.bar_dirty == 0 {
            return Ok(());
        }
        let dirty = self.bar_dirty;
        self.bar_dirty = 0;
        #[cfg(feature = "internal-bar")]
        {
            let n = self.engine.state.monitors.len().min(64);
            for mi in 0..n {
                if dirty & (1u64 << mi) != 0 {
                    self.bar
                        .draw(&self.conn, &self.engine.state, mi, &self.engine.cfg)?;
                }
            }
        }
        #[cfg(not(feature = "internal-bar"))]
        let _ = dirty;
        Ok(())
    }

    /// Kept for call-sites that already exist in the code.
    /// All calls now just mark dirty; `flush_bars()` handles the actual paint.
    #[inline]
    pub(super) fn draw_bar(&mut self, mi: usize) {
        self.mark_bar(mi);
    }

    pub(super) fn update_status(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let prop = self
            .conn
            .get_property(
                false,
                self.root,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                0,
                256,
            )?
            .reply()?;
        self.engine.state.status = String::from_utf8_lossy(&prop.value).into_owned();
        self.mark_all_bars();
        Ok(())
    }
}
