use super::*;

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
        Ok(())
    }

    pub(super) fn on_unmap(
        &mut self,
        e: UnmapNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
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
        // Tiled/floating windows keep the ICCCM behavior below (stay withdrawn
        // until destroy or re-map).
        let presented = self
            .engine
            .state
            .clients
            .get(&e.window)
            .is_some_and(|c| c.is_fullscreen() || c.is_maximized());
        if presented {
            return self.unmanage(e.window, false);
        }
        if e.response_type & 0x80 != 0 {
            let _ = self.set_wm_state(e.window, 0);
        } else {
            let _ = self.set_wm_state(e.window, 0);
            let mi = self
                .engine
                .state
                .clients
                .get(&e.window)
                .map_or(0, |c| c.monitor);
            if self.engine.state.monitors.get(mi).and_then(|m| m.focused) == Some(e.window) {
                self.focus_best(mi)?;
            }
        }
        Ok(())
    }

    pub(super) fn on_configure_request(
        &mut self,
        e: ConfigureRequestEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(client) = self.engine.state.clients.get(&e.window) {
            if !client.is_float() && !client.is_fullscreen() {
                let geom = client.geom;
                let bw = client.border_w;
                let ev = ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: e.window,
                    window: e.window,
                    above_sibling: x11rb::NONE,
                    x: geom.x as i16,
                    y: geom.y as i16,
                    width: geom.w as u16,
                    height: geom.h as u16,
                    border_width: bw as u16,
                    override_redirect: false,
                };
                let _ = self
                    .conn
                    .send_event(false, e.window, EventMask::STRUCTURE_NOTIFY, ev);
                return Ok(());
            }
        }
        // floating or unmanaged: honor the request
        let mut aux = ConfigureWindowAux::new();
        if e.value_mask.contains(ConfigWindow::X) {
            aux = aux.x(e.x as i32);
        }
        if e.value_mask.contains(ConfigWindow::Y) {
            aux = aux.y(e.y as i32);
        }
        if e.value_mask.contains(ConfigWindow::WIDTH) {
            aux = aux.width(e.width as u32);
        }
        if e.value_mask.contains(ConfigWindow::HEIGHT) {
            aux = aux.height(e.height as u32);
        }
        if e.value_mask.contains(ConfigWindow::BORDER_WIDTH) {
            aux = aux.border_width(e.border_width as u32);
        }
        if e.value_mask.contains(ConfigWindow::STACK_MODE) {
            aux = aux.stack_mode(e.stack_mode);
        }
        if e.value_mask.contains(ConfigWindow::SIBLING) {
            aux = aux.sibling(e.sibling);
        }
        let _ = self.conn.configure_window(e.window, &aux);

        if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
            if e.value_mask.contains(ConfigWindow::X) {
                c.geom.x = e.x as i32;
            }
            if e.value_mask.contains(ConfigWindow::Y) {
                c.geom.y = e.y as i32;
            }
            if e.value_mask.contains(ConfigWindow::WIDTH) {
                c.geom.w = e.width as u32;
            }
            if e.value_mask.contains(ConfigWindow::HEIGHT) {
                c.geom.h = e.height as u32;
            }
        }
        Ok(())
    }

    pub(super) fn on_configure_notify(
        &mut self,
        e: ConfigureNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.window == self.root {
            self.handle_monitor_change()?;
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
                // Replace monitors with fresh ones (empty workspaces).
                self.engine.state.monitors = new_mons;

                // Clamp sel_mon so no code tries to index a monitor that no longer exists.
                let n_mons = self.engine.state.monitors.len();
                self.engine.state.sel_mon =
                    self.engine.state.sel_mon.min(n_mons.saturating_sub(1));

                // Re-assign every client to a valid monitor/workspace,
                // preserving the original assignment where possible.
                let dw = self.engine.cfg.default_col_w;
                for win in old_clients {
                    if let Some(c) = self.engine.state.clients.get_mut(&win) {
                        c.monitor = c.monitor.min(n_mons.saturating_sub(1));
                        c.workspace = c.workspace.min(
                            self.engine.state.monitors[c.monitor]
                                .workspaces
                                .len()
                                .saturating_sub(1),
                        );
                    }
                    let is_float = self
                        .engine
                        .state
                        .clients
                        .get(&win)
                        .is_some_and(crate::types::Client::is_float);
                    let mi = self.engine.state.clients.get(&win).map_or(0, |c| c.monitor);
                    let ws_i = self
                        .engine
                        .state
                        .clients
                        .get(&win)
                        .map_or(0, |c| c.workspace);
                    let workarea_w = self.engine.state.monitors[mi].workarea.w;
                    if is_float {
                        self.engine.state.monitors[mi].workspaces[ws_i]
                            .floats
                            .push(win);
                    } else {
                        self.engine.state.monitors[mi].workspaces[ws_i]
                            .add_tiled(win, dw, workarea_w);
                    }
                }
            } else {
                // Geometry-only change: update screen/workarea in place,
                // preserving all workspace state and client assignments.
                for (new_mon, old_mon) in
                    new_mons.iter().zip(self.engine.state.monitors.iter_mut())
                {
                    old_mon.screen = new_mon.screen;
                    old_mon.workarea = new_mon.workarea;
                }
                for i in 0..self.engine.state.monitors.len() {
                    self.arrange(i)?;
                }
            }

            // Update EWMH properties for external taskbars
            self.update_ewmh_desktops()?;
            self.update_workarea()?;

            for i in 0..self.engine.state.monitors.len() {
                self.arrange(i)?;
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
        // and before the client lookup.
        if (e.atom == self.atoms.net_wm_strut_partial || e.atom == self.atoms.net_wm_strut)
            && !self.engine.state.clients.contains_key(&e.window)
        {
            self.apply_dock_strut(e.window)?;
            return Ok(());
        }

        if e.state == Property::DELETE {
            return Ok(());
        }

        if self.engine.state.clients.contains_key(&e.window) {
            let win = e.window;
            if e.atom == self.atoms.net_wm_name || e.atom == u32::from(AtomEnum::WM_NAME) {
                self.refresh_title(win)?;
            } else if e.atom == u32::from(AtomEnum::WM_HINTS) {
                self.refresh_hints(win)?;
            }
            // Other property changes (size hints, ICCCM state, etc.) need no
            // action here — `publish_state()` in the event loop diffs the JSON
            // snapshot and pushes updates to IPC subscribers (external bars,
            // maverickctl) exactly when something visible changed.
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
                    self.set_fullscreen(e.window, new_fs)?;
                }
            }
            let max_v = self.atoms.net_wm_state_maximized_vert;
            let max_h = self.atoms.net_wm_state_maximized_horiz;
            if a1 == max_v || a2 == max_v || a1 == max_h || a2 == max_h {
                let cur = self
                    .engine
                    .state
                    .clients
                    .get(&e.window)
                    .is_some_and(crate::types::Client::is_maximized);
                let new_max = match action {
                    0 => false,
                    1 => true,
                    _ => !cur,
                };
                if new_max != cur {
                    self.set_maximized(e.window, new_max)?;
                }
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
            // _NET_ACTIVE_WINDOW: focus the window on whatever monitor it's on.
            // Don't change the monitor's active_ws — that's the user's decision.
            if self.engine.state.clients.contains_key(&e.window) {
                self.focus(Some(e.window))?;
            }
        } else if e.type_ == self.atoms.net_close_window {
            self.kill(e.window)?;
        }
        Ok(())
    }

    pub(super) fn on_key(&mut self, e: KeyPressEvent) -> Result<(), Box<dyn std::error::Error>> {
        self.last_event_time = e.time;
        let ksym = self.keycode_to_keysym(e.detail, u16::from(e.state))?;
        let ksym = normalize_ksym(ksym);
        let mods = clean_mask(u16::from(e.state), self.numlock);
        let key = (mods, ksym);
        if let Some(action) = self.keymap.get(&key).cloned() {
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
            // Keyboard navigation must not be instantly undone by an
            // EnterNotify while the pointer is parked over another tile's
            // edge. Ignore pointer-focus for a short window; the first real
            // MotionNotify lifts the guard.
            self.pointer_guard_until = Some(
                std::time::Instant::now() + std::time::Duration::from_millis(50),
            );
        }
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
                if self.engine.state.monitors[self.engine.state.sel_mon].focused != Some(cw) {
                    self.focus(Some(cw))?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn on_mapping(
        &mut self,
        e: MappingNotifyEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if e.request == Mapping::KEYBOARD || e.request == Mapping::MODIFIER {
            let ks = fetch_keyboard_state(&self.conn)?;
            self.raw_keymap = ks.keysyms;
            self.raw_kpk = ks.kpk;
            self.raw_min = ks.min;
            self.numlock = ks.numlock;
            self.grab_keys()?;

            // Re-grab buttons on all existing windows.
            // Without this, existing grab_button still uses the old numlock
            // → Mod4+click stops working after NumLock toggle.
            let wins: Vec<Window> = self.engine.state.clients.keys().copied().collect();
            for win in wins {
                let _ = self.grab_buttons(win, false);
            }
        }
        Ok(())
    }
}
