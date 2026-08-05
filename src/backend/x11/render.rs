use super::*;

impl WindowManager {
    pub(super) fn arrange(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full(mon_idx, true, true)
    }

    /// P8/P11: arrange with optional `hide_offscreen` and restack.
    /// Lightweight arrange skips both — safe when only focus/row heights change.
    pub(super) fn arrange_full(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
        do_restack: bool,
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
            &mut self.placements_buf,
        );
        // Presentation layer: apply fullscreen (tied to focus) on top of the
        // pure layout geometry. Returns the window to raise, if any.
        let mut buf = std::mem::take(&mut self.placements_buf);
        let raise = present(
            &self.engine.state,
            &self.engine.state.monitors[mon_idx],
            &mut buf,
        );
        for &(win, geom, bw) in &buf {
            self.apply_geom(win, geom, bw)?;
        }
        buf.clear();
        self.placements_buf = buf;
        if let Some(w) = raise {
            let _ = self
                .conn
                .configure_window(w, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }

        if do_restack && self.stack_dirty {
            self.stack_dirty = false;
            self.restack(mon_idx)?;
        }
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
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().x(gx).y(gy));
                client.wm_hidden = false;
            }
        }
        Ok(())
    }

    pub(super) fn restack(&self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = mon.ws();

        // 1. Raise floats above tiled
        for &win in &ws.floats {
            if self.engine.state.clients.contains_key(&win) {
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            }
        }

        // 2. Raise the focused window if it is fullscreen. Only the focused
        //    fullscreen window is presented full-screen (see core::present), so
        //    only it should be raised — raising every fullscreen window in the
        //    stack could put a non-focused one on top of the focused one.
        if let Some(fw) = mon.focused {
            if self
                .engine
                .state
                .clients
                .get(&fw)
                .is_some_and(|c| c.is_fullscreen() || c.is_maximized())
            {
                let _ = self
                    .conn
                    .configure_window(fw, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            }
        }

        Ok(())
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
        if geom == client.geom && bw == client.border_w {
            return Ok(());
        }

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
        }
        Ok(())
    }

    pub(super) fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

        // Presentation (core::present) draws a fullscreen window full-screen only
        // while it is focused. So if the previously- or newly-focused window is
        // fullscreen, the rendered geometry must be recomputed after the focus
        // change. Capture that here and re-arrange at the end.
        let prev_focused = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);
        let fs_transition = |s: &crate::types::State, w: Option<Window>| -> bool {
            w.and_then(|w| s.clients.get(&w))
                .is_some_and(|c| c.is_fullscreen() || c.is_maximized())
        };
        let needs_rearrange =
            fs_transition(&self.engine.state, prev_focused) && prev_focused != valid_win;

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
                self.send_proto(w, self.atoms.wm_take_focus)?;
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
            if let Some(c) = self.engine.state.clients.get_mut(&w) {
                c.focus_serial = serial;
                c.flags.clear(WinFlags::URGENT);
            }

            let mon = &mut self.engine.state.monitors[mon_i];
            mon.focused = Some(w);
            mon.focus_stack.retain(|&x| x != w);
            mon.focus_stack.push(w);

            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[w],
            );

            if self.engine.cfg.warp_cursor {
                let _ = self.conn.warp_pointer(
                    x11rb::NONE,
                    w,
                    0,
                    0,
                    0,
                    0,
                    (geom.w / 2) as i16,
                    (geom.h / 2) as i16,
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

        // Re-render if a fullscreen presentation transition happened: either the
        // window we just left was fullscreen (must shrink back to layout) or the
        // one we just focused is fullscreen (must grow to cover the screen).
        let new_is_fs = fs_transition(&self.engine.state, valid_win);
        if needs_rearrange || new_is_fs {
            let mon_i = self.engine.state.sel_mon;
            self.stack_dirty = true;
            self.arrange(mon_i)?;
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
