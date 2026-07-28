use super::*;

impl WindowManager {
    /// The backend's action entry point: the Engine owns *all* domain logic
    /// (State mutation) and returns the semantic Effects; the backend only
    /// carries them out. Fullscreen is presentation-only and tied to focus
    /// (see `core::present`), so every action is safe while fullscreen.
    pub(super) fn do_action(&mut self, action: Action) -> Result<(), Box<dyn std::error::Error>> {
        let effects = self.engine.dispatch(action);
        self.run_effects(effects)
    }

    /// Execute a batch of semantic effects emitted by the Engine, in order.
    pub(super) fn run_effects(
        &mut self,
        effects: Vec<Effect>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for eff in effects {
            self.execute(eff)?;
        }
        Ok(())
    }

    /// The single place that turns a semantic `Effect` into concrete X11 calls.
    /// A future Wayland backend would provide a different `execute` for the same
    /// effects without the core changing.
    pub(super) fn execute(&mut self, eff: Effect) -> Result<(), Box<dyn std::error::Error>> {
        match eff {
            Effect::ArrangeMonitor(mi) => self.arrange(mi)?,
            Effect::ArrangeAll => {
                for mi in 0..self.engine.state.monitors.len() {
                    self.arrange(mi)?;
                }
            }
            Effect::MarkRestack(_mi) => {
                self.stack_dirty = true;
            }
            Effect::FocusWindow(win) => self.focus(win)?,
            Effect::Unfocus(win) => self.unfocus(win)?,
            Effect::ConfigureWindow {
                win,
                geom,
                border_w,
            } => self.apply_geom(win, geom, border_w)?,
            Effect::KillWindow(win) => self.kill(win)?,
            Effect::MapWindow(win) => {
                let _ = self.conn.map_window(win);
            }
            Effect::UnmapWindow(win) => {
                let _ = self.conn.unmap_window(win);
            }
            Effect::SetFullscreen { win, on } => self.set_fullscreen(win, on)?,
            Effect::RecalcWorkarea(mi) => {
                if mi < self.engine.state.monitors.len() {
                    let bh = self.engine.cfg.bar_height;
                    self.engine.state.monitors[mi].recalc_workarea(bh);
                }
            }
            Effect::UpdateBar(mi) => self.draw_bar(mi),
            Effect::SyncBarVisibility(mi) => {
                if let Some(bw) = self.engine.state.monitors.get(mi).and_then(|m| m.bar_win) {
                    if self.engine.state.monitors[mi].show_bar {
                        let _ = self.conn.map_window(bw);
                    } else {
                        let _ = self.conn.unmap_window(bw);
                    }
                }
            }
            Effect::UpdateEwmhDesktops => self.update_ewmh_desktops()?,
            Effect::UpdateClientList => self.update_client_list()?,
            Effect::SetCurrentDesktop(ws) => {
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    self.root,
                    self.atoms.net_current_desktop,
                    AtomEnum::CARDINAL,
                    &[ws as u32],
                );
            }
            Effect::SetWindowDesktop { win, ws } => {
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    win,
                    self.atoms.net_wm_desktop,
                    AtomEnum::CARDINAL,
                    &[ws as u32],
                );
            }
            Effect::Spawn(cmd) => self.spawn(&cmd),
            Effect::Quit => self.engine.state.running = false,
            Effect::Restart => self.restart(),
            Effect::PublishIpcState => self.publish_state(),
        }
        Ok(())
    }

    /// Re-exec the WM binary in place. `exec()` replaces the current process
    /// image without forking, so there's no race where two maverick instances
    /// fight over X11 grabs simultaneously.
    pub(super) fn restart(&mut self) {
        use std::os::unix::process::CommandExt;
        if let Ok(exe) = std::env::current_exe() {
            let err = std::process::Command::new(exe).exec();
            log::error!("restart exec failed: {err}");
        }
        self.engine.state.running = false;
    }

    pub(super) fn spawn(&self, cmd: &[String]) {
        if cmd.is_empty() {
            return;
        }
        let _ = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    pub(super) fn kill(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        if self.has_protocol(win, self.atoms.wm_delete_window)? {
            self.send_proto(win, self.atoms.wm_delete_window)?;
        } else {
            let _ = self.conn.kill_client(win);
        }
        Ok(())
    }

    /// Hand over the control socket server (kept so `cleanup` can tear it down).
    pub fn set_control(&mut self, server: maverick_sys::ControlServer) {
        self.control = Some(server);
    }

    /// Record the instance name (used for the identity ficha teardown).
    pub fn set_instance_name(&mut self, name: String) {
        self.instance_name = name;
    }

    /// Attach the control hub bridging the socket thread and the WM loop.
    pub fn set_hub(&mut self, hub: maverick_sys::ControlHub) {
        self.hub = Some(hub);
    }

    /// Drain any control commands the socket thread queued and act on them:
    /// dispatch actions through the Engine, or quit/restart/reload the WM.
    pub(super) fn drain_control(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let cmds = match &self.hub {
            Some(h) => h.drain_commands(),
            None => return Ok(()),
        };
        for cmd in cmds {
            match cmd {
                maverick_sys::ControlCommand::Quit => self.engine.state.running = false,
                maverick_sys::ControlCommand::Restart => self.restart(),
                maverick_sys::ControlCommand::Reload => {
                    // Config is compiled in; reload just re-grabs keys + re-arranges.
                    self.keymap = build_keymap(&self.engine.cfg);
                    self.grab_keys()?;
                    for mi in 0..self.engine.state.monitors.len() {
                        self.arrange(mi)?;
                    }
                }
                maverick_sys::ControlCommand::Dispatch(line) => {
                    if let Some(action) = parse_action(&line) {
                        self.do_action(action)?;
                    } else {
                        log::warn!("control: unknown dispatch action '{line}'");
                    }
                }
            }
        }
        Ok(())
    }

    /// Publish a fresh state snapshot to the hub (only when it changed), and emit
    /// granular focus/workspace events to `subscribe` clients on transitions.
    pub(super) fn publish_state(&mut self) {
        let hub = match &self.hub {
            Some(h) => h.clone(),
            None => return,
        };
        let json = state_json(&self.engine.state, &self.engine.cfg);
        if json != self.last_state_json {
            hub.publish_state(json.clone());
            self.last_state_json = json;
        }

        let sel_mon = self.engine.state.sel_mon;
        let focused = self
            .engine
            .state
            .monitors
            .get(sel_mon)
            .and_then(|m| m.focused);
        let active_ws = self
            .engine
            .state
            .monitors
            .get(sel_mon)
            .map_or(0, |m| m.active_ws);

        if focused != self.last_focus {
            hub.emit(format!("focus {}", focused.unwrap_or(0)));
            self.last_focus = focused;
        }
        if active_ws != self.last_active_ws || sel_mon != self.last_sel_mon {
            hub.emit(format!("workspace {active_ws} {sel_mon}"));
            self.last_active_ws = active_ws;
            self.last_sel_mon = sel_mon;
        }
    }
}
