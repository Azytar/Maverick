use super::*;

#[derive(Debug, Clone)]
pub(super) struct DragState {
    pub(super) win: Window,
    pub(super) start_geom: Rect,
    pub(super) ptr_x: i32,
    pub(super) ptr_y: i32,
    pub(super) resize: bool,
}

impl WindowManager {
    pub(super) fn on_button_press(
        &mut self,
        e: ButtonPressEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Scroll buttons (4=up,5=down,6=left,7=right) — early return.
        // Without this, every scroll tick goes through the full handler: find_client RTT,
        // focus change, drag intent, allow_events → C1 crash, erratic focus.
        if e.detail >= 4 {
            self.conn.allow_events(Allow::REPLAY_POINTER, e.time)?;
            return Ok(());
        }

        // ── Bar click: switch workspace on the clicked monitor ───────────────
        #[cfg(feature = "internal-bar")]
        for mon_i in 0..self.engine.state.monitors.len() {
            if self.engine.state.monitors[mon_i].bar_win == Some(e.event) {
                if let Some(ws_i) = self.bar.tag_at_x(e.event_x, &self.engine.cfg.tag_names) {
                    if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
                        // Switch focus to clicked monitor if different.
                        if mon_i != self.engine.state.sel_mon {
                            if let Some(fw) =
                                self.engine.state.monitors[self.engine.state.sel_mon].focused
                            {
                                let _ = self.unfocus(fw);
                            }
                            self.engine.state.sel_mon = mon_i;
                        }
                        self.engine.state.monitors[mon_i].active_ws = ws_i;
                        let scroll =
                            ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
                        self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
                        self.arrange(mon_i)?;
                        self.focus_best(mon_i)?;
                    }
                }
                // Bar is override_redirect and has no passive grab — no allow_events needed.
                return Ok(());
            }
        }

        let mi = self.engine.state.mon_at(e.root_x as i32, e.root_y as i32);
        if mi != self.engine.state.sel_mon {
            if let Some(fw) = self.engine.state.monitors[self.engine.state.sel_mon].focused {
                self.unfocus(fw)?;
            }
            self.engine.state.sel_mon = mi;
        }

        // When the focused window is fullscreen, don't change focus on click
        // and don't start drags — the fullscreen window is locked (niri-style).
        let focused_fs = self.engine.state.monitors[mi]
            .focused
            .and_then(|fw| self.engine.state.clients.get(&fw))
            .is_some_and(crate::types::Client::is_fullscreen);

        let client_win = self.find_client(e.event);
        if !focused_fs {
            if let Some(cw) = client_win {
                if self.engine.state.monitors[mi].focused != Some(cw) {
                    self.focus(Some(cw))?;
                    self.restack(mi)?;
                }
            } else if e.event == self.root {
                self.focus(None)?;
            }
        }

        let mut drag_started = false;
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
                        self.drag = Some(DragState {
                            win: cw,
                            start_geom: geom,
                            ptr_x: e.root_x as i32,
                            ptr_y: e.root_y as i32,
                            resize: is_resize,
                        });
                        drag_started = true;
                    }
                }
            }
        }

        // REPLAY_POINTER: re-delivers the click to the application so popups,
        //   context menus and dialogs open normally.
        // ASYNC_POINTER (drag only): releases the passive-grab freeze and hands
        //   control to the active grab from grab_pointer().
        self.conn
            .allow_events(
                if drag_started {
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
        if let Some(drag) = self.drag.take() {
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

            // If on_motion set the FLOAT flag but the window is still in a column,
            // promote it to ws.floats now so arrange() treats it as a float and
            // doesn't retile it back to its column position.
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
        }
        Ok(())
    }

    pub(super) fn on_motion(
        &mut self,
        e: MotionNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(drag) = self.drag.clone() {
            let dx = e.root_x as i32 - drag.ptr_x;
            let dy = e.root_y as i32 - drag.ptr_y;

            // saturating_add in drag coordinates: fast pointer movement on
            // 4K+ high-refresh displays can overflow i32 → panic (debug)
            // or corrupted geometry → BadValue (release).
            let geom = if drag.resize {
                Rect::new(
                    drag.start_geom.x,
                    drag.start_geom.y,
                    ((drag.start_geom.w as i32).saturating_add(dx)).max(50) as u32,
                    ((drag.start_geom.h as i32).saturating_add(dy)).max(50) as u32,
                )
            } else {
                Rect::new(
                    drag.start_geom.x.saturating_add(dx),
                    drag.start_geom.y.saturating_add(dy),
                    drag.start_geom.w,
                    drag.start_geom.h,
                )
            };

            if let Some(c) = self.engine.state.clients.get(&drag.win) {
                let bw = c.border_w;
                self.apply_geom(drag.win, geom, bw)?;
            }
            if let Some(c) = self.engine.state.clients.get_mut(&drag.win) {
                c.geom = geom;
                c.flags.set(WinFlags::FLOAT);
            }
        } else if self.engine.cfg.focus_mouse {
            if let Some(cw) = self.find_client(e.event) {
                if self.engine.state.monitors[self.engine.state.sel_mon].focused != Some(cw) {
                    self.focus(Some(cw))?;
                }
            }
        }
        Ok(())
    }
}
