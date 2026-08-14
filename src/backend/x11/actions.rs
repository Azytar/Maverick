use super::*;

use std::os::unix::io::AsRawFd;

/// Maximum time Maverick waits for clients to close cooperatively during a
/// graceful shutdown. After this elapses, any remaining clients are force-killed
/// (escape hatch) and Maverick terminates regardless. This is a global budget,
/// NOT a per-client wait.
const SHUTDOWN_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

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
            } => self.apply_geom(win, geom, border_w, true)?,
            Effect::KillWindow(win) => self.kill(win)?,
            Effect::SetFullscreen { win, on } => self.set_fullscreen(win, on)?,
            Effect::SetMaximized { win, vert, horiz } => self.set_maximized(win, vert, horiz)?,
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
            Effect::Quit => self.begin_shutdown(),
            Effect::Restart => self.restart(),
            Effect::SetWallpaper => {
                // Push the engine's current wallpaper spec into the compositor.
                // A decode/compile failure there logs once and leaves the
                // wallpaper disabled — it never takes the WM down (criterio #2).
                if let Some(comp) = self.compositor.as_mut() {
                    comp.set_wallpaper(&self.engine.state.wallpaper);
                }
            }
            Effect::PublishIpcState => self.publish_state(),
        }
        Ok(())
    }

    /// Re-exec the WM binary in place with the EXACT arguments it was launched
    /// with, so the new instance reuses the same `--config`, `--name` and
    /// `--replace` (a real hard restart that rebuilds all state from scratch).
    ///
    /// Before exec we explicitly tear down X11 (key grabs, SubstructureRedirect,
    /// EWMH root props, check window) and the IPC socket + identity ficha via
    /// `cleanup()`, and mark the X connection fd `FD_CLOEXEC` so it is closed on
    /// exec — we do NOT rely on the connection layer having set CLOEXEC. `exec`
    /// replaces the process image without forking, so there is no window where
    /// two maverick instances contend over X11 grabs.
    pub(super) fn restart(&mut self) {
        use std::os::unix::process::CommandExt;

        // Release X11 resources + IPC + ficha so the new instance starts clean
        // and can reclaim the screen.
        let _ = self.cleanup();

        // Close the X connection fd on exec (explicit, not assumed): the new
        // process must open its own connection, not inherit this one's identity.
        let fd = self.conn.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }

        if let Ok(exe) = std::env::current_exe() {
            // `launch_args` excludes argv[0] (the program name), which
            // `Command::new(exe)` already supplies, so the new argv matches the
            // original launch exactly.
            let err = std::process::Command::new(exe)
                .args(&self.launch_args)
                .exec();
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

    /// Begin a graceful shutdown. Asks every managed client to close: clients
    /// that advertise `WM_DELETE_WINDOW` get the cooperative delete request;
    /// clients without it cannot cooperate, so they are force-killed
    /// immediately (there is nothing to wait for). Then arms a single global
    /// deadline. Idempotent: a second call is a no-op.
    pub(super) fn begin_shutdown(&mut self) {
        if self.shutdown_deadline.is_some() {
            return;
        }
        for win in self
            .engine
            .state
            .clients
            .keys()
            .copied()
            .collect::<Vec<_>>()
        {
            let cooperate = self
                .has_protocol(win, self.atoms.wm_delete_window)
                .unwrap_or(false);
            if cooperate {
                let _ = self.send_proto(win, self.atoms.wm_delete_window, self.last_event_time);
            } else {
                let _ = self.conn.kill_client(win);
            }
        }
        self.shutdown_deadline = Some(std::time::Instant::now() + SHUTDOWN_BUDGET);
    }

    /// Escape hatch: force-kill (X KillClient) every still-managed client.
    /// Fire-and-forget — we do NOT wait for them to actually die; Maverick
    /// terminates regardless.
    pub(super) fn force_kill_remaining(&self) {
        for win in self
            .engine
            .state
            .clients
            .keys()
            .copied()
            .collect::<Vec<_>>()
        {
            let _ = self.conn.kill_client(win);
        }
    }

    /// Hand over the control socket server (kept so `cleanup` can tear it down).
    pub fn set_control(&mut self, server: maverick_sys::ControlServer) {
        self.control = Some(server);
    }

    /// Record the session id (used for the identity ficha teardown).
    pub fn set_session_id(&mut self, sid: String) {
        self.session_id = sid;
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
                maverick_sys::ControlCommand::Quit => self.begin_shutdown(),
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
                    let json =
                        crate::core::ipc::query_json(&self.engine.state, &self.engine.cfg, &topic);
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
        // Re-read the same file we booted from: the `--config` override (stored
        // on `self.config_path`) must survive a reload, not be replaced by the
        // XDG default. Fall back to the XDG path only when no override was set.
        let Some(path) = self
            .config_path
            .clone()
            .or_else(crate::userconfig::config_path)
        else {
            log::warn!("reload: no config path available; keeping current config");
            return Ok(());
        };
        let (cfg, diag) = crate::userconfig::load_from_path(&path);
        crate::userconfig::dump_diagnostics(&diag);

        let tags_changed =
            cfg.n_tags != self.engine.cfg.n_tags || cfg.tag_names != self.engine.cfg.tag_names;
        let mut clamped_wins = Vec::new();
        if tags_changed {
            for mon in &mut self.engine.state.monitors {
                mon.reconcile_workspaces(cfg.n_tags);
            }
            let n_tags = cfg.n_tags;
            for (&win, client) in &mut self.engine.state.clients {
                if client.workspace >= n_tags {
                    client.workspace = n_tags.saturating_sub(1);
                    clamped_wins.push(win);
                }
            }
        }

        self.engine.cfg = cfg;
        self.keymap = build_keymap(&self.engine.cfg);
        self.grab_keys()?;

        // Re-seed the native wallpaper from the freshly reloaded config. The
        // startup path does this too; without it `reload` would silently ignore
        // `[wallpaper]` changes (the wallpaper is only read from config here,
        // never from IPC state — IPC `wallpaper set` updates `state` directly).
        if let Some(comp) = self.compositor.as_mut() {
            if let Some(path) = self.engine.cfg.wallpaper.path.clone() {
                self.engine.state.wallpaper.source =
                    crate::core::wallpaper::WallpaperSource::from_path(path.into());
                self.engine.state.wallpaper.mode = self.engine.cfg.wallpaper.mode;
            } else {
                self.engine.state.wallpaper.source = crate::core::wallpaper::WallpaperSource::None;
            }
            comp.set_wallpaper(&self.engine.state.wallpaper);
        }

        // Republish EWMH desktop state for external bars/taskbars. Only the
        // count/names need a refresh here — `_NET_CURRENT_DESKTOP` must NOT be
        // reset (it would yank the active tag back to 0 on every reload).
        // Any client whose workspace was clamped also needs its `_NET_WM_DESKTOP`
        // re-emitted so the new desktop index is reflected.
        if tags_changed {
            self.update_ewmh_desktop_count()?;
            for win in clamped_wins {
                let ws = self
                    .engine
                    .state
                    .clients
                    .get(&win)
                    .map_or(0, |c| c.workspace);
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    win,
                    self.atoms.net_wm_desktop,
                    AtomEnum::CARDINAL,
                    &[ws as u32],
                );
            }
        }

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
