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
            Effect::MarkRestack(_mi) => {
                self.stack_dirty = true;
                // Focus-driven raises reorder the stack: refresh the
                // `_NET_CLIENT_LIST_STACKING` property in the same flush.
                self.client_list_dirty = true;
            }
            Effect::FocusWindow(win) => self.focus(win)?,
            Effect::Unfocus(win) => self.unfocus(win)?,
            Effect::ConfigureWindow {
                win,
                geom,
                border_w,
            } => self.apply_geom(win, geom, border_w)?,
            Effect::KillWindow(win) => self.kill(win)?,
            Effect::SetFullscreen { win, on } => self.set_fullscreen(win, on)?,
            Effect::SyncWindowPrefs(win) => self.sync_window_prefs(win),
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
        // Tear down the control socket and identity ficha before exec
        // so the new process starts with a clean slate.
        if !self.instance_name.is_empty() {
            maverick_sys::identity::cleanup_meta(&self.instance_name);
        }
        drop(self.control.take());
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
            self.send_proto(win, self.atoms.wm_delete_window, self.last_event_time)?;
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

    /// Attach the control hub bridging the socket thread and the WM loop, and
    /// subscribe the `HubEventSink` to the typed `EventBus` so domain events
    /// render onto the `subscribe` wire protocol.
    pub fn set_hub(&mut self, hub: maverick_sys::ControlHub) {
        self.engine
            .subscribe(Box::new(super::hubevents::HubEventSink::new(hub.clone())));
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
                maverick_sys::ControlCommand::Reload => self.reload_config()?,
                maverick_sys::ControlCommand::Dispatch(line) => {
                    if let Some(action) = parse_action(&line) {
                        self.do_action(action)?;
                    } else {
                        log::warn!("control: unknown dispatch action '{line}'");
                    }
                }
                maverick_sys::ControlCommand::Query { topic, reply } => {
                    // Answer structured queries from live state; the client is
                    // blocked on the channel until the reply lands. State is
                    // only touched here (the WM thread), which is exactly why
                    // querying has to happen through this queue.
                    let json = crate::core::ipc::query_json(
                        &self.engine.state,
                        &self.engine.cfg,
                        &topic,
                    );
                    let _ = reply.send(json);
                }
            }
        }
        Ok(())
    }

    /// Re-read the user TOML (same fail-safe path used at startup) and swap it
    /// in. A config that can't be read, parsed, or applied leaves the current
    /// config untouched and only logs a warning — reload can never crash or
    /// blank the WM. If the tag count changed, every monitor's workspace list
    /// is reconciled (grown/truncated) before the new keymap is grabbed and
    /// everything is re-arranged.
    pub(super) fn reload_config(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = crate::userconfig::config_path() else {
            log::warn!("reload: no XDG config path available; keeping current config");
            return Ok(());
        };
        let cfg = crate::userconfig::load_from_path(&path);

        let tags_changed =
            cfg.n_tags != self.engine.cfg.n_tags || cfg.tag_names != self.engine.cfg.tag_names;
        if tags_changed {
            for mon in &mut self.engine.state.monitors {
                mon.reconcile_workspaces(cfg.n_tags);
            }
            let n_tags = cfg.n_tags;
            for client in self.engine.state.clients.values_mut() {
                if client.workspace >= n_tags {
                    client.workspace = n_tags.saturating_sub(1);
                }
            }
        }

        self.engine.cfg = cfg;
        self.keymap = build_keymap(&self.engine.cfg);
        self.grab_keys()?;
        for mi in 0..self.engine.state.monitors.len() {
            self.arrange(mi)?;
        }
        log::info!(
            "reload: {} tags, {} keybinds, {} rules, {} autostart",
            self.engine.cfg.tag_names.len(),
            self.engine.cfg.keybinds.len(),
            self.engine.cfg.rules.len(),
            self.engine.cfg.autostart.len(),
        );
        Ok(())
    }

    /// Publish a fresh JSON state snapshot to the hub, but only when it changed.
    ///
    /// Granular `focus`/`workspace` lines for `subscribe` clients are no longer
    /// derived here: they come from the typed `EventBus` via `HubEventSink`, so a
    /// single source of truth describes every transition.
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
    }
}
