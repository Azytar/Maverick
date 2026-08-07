use super::*;

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
        // Scroll buttons (4=up,5=down,6=left,7=right) — early return.
        // Without this, every scroll tick goes through the full handler: find_client RTT,
        // focus change, drag intent, allow_events → C1 crash, erratic focus.
        if e.detail >= 4 {
            self.conn.allow_events(Allow::REPLAY_POINTER, e.time)?;
            return Ok(());
        }
        self.last_event_time = e.time;

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
                        let resize_l = is_resize && (e.root_x as i32) < geom.x + (geom.w as i32) / 2;
                        let resize_t = is_resize && (e.root_y as i32) < geom.y + (geom.h as i32) / 2;
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
        e: ButtonReleaseEvent,
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
                        {
                            let ws = &mut self.engine.state.monitors[mi].workspaces[ws_i];
                            // Remove from its current place (floats or a column).
                            ws.remove_window(win);
                            let cws = &mut ws.columns[ci];
                            // Insert at the row whose windows sit above the
                            // pointer (count of windows with center above ry).
                            let insert_pos = cws
                                .windows
                                .iter()
                                .take_while(|&&w| {
                                    self.engine
                                        .state
                                        .clients
                                        .get(&w)
                                        .is_some_and(|c| c.geom.y + c.geom.h as i32 / 2 < ry)
                                })
                                .count();
                            cws.windows.insert(insert_pos, win);
                            ws.focus.column_idx = ci;
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
                    g.w = (start_geom.w as i32).saturating_sub(dx).max(50) as u32;
                } else {
                    g.w = (start_geom.w as i32).saturating_add(dx).max(50) as u32;
                }
                if resize_t {
                    g.y = start_geom.y.saturating_add(dy);
                    g.h = (start_geom.h as i32).saturating_sub(dy).max(50) as u32;
                } else {
                    g.h = (start_geom.h as i32).saturating_add(dy).max(50) as u32;
                }
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
            if let Some(c) = self.engine.state.clients.get(&win) {
                let bw = c.border_w;
                self.apply_geom(win, Rect::new(gx, gy, gw, gh), bw)?;
            }
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                c.geom = Rect::new(gx, gy, gw, gh);
                c.flags.set(WinFlags::FLOAT);
            }

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
                if c.is_fullscreen() || c.is_maximized() {
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
}
