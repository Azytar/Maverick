//! Engine→X11 effect bridge.
//!
//! `do_action` dispatches via `engine.dispatch`, captures
//! toggle diagnostics, and runs the effects. `execute` is
//! the single match arm for every `Effect` variant — a
//! future Wayland backend replaces only this module.
//!
//! # Effect semantics
//!
//! Each `Effect` variant is a semantic instruction from the
//! core. The backend translates it into X11 protocol calls:
//! - `ArrangeMonitor` → `render::arrange_full`
//! - `FocusWindow` → `set_input_focus` + `WM_TAKE_FOCUS`
//! - `ConfigureWindow` → `configure_window` + fake
//!   `ConfigureNotifyEvent`
//! - `SetFullscreen` → `_NET_WM_BYPASS_COMPOSITOR` +
//!   `_NET_WM_STATE` rewrite
//! - `KillWindow` → `WM_DELETE_WINDOW` or `KillClient`
//! - `Spawn` → `Command::spawn`
//! - `PublishIpcState` → `hub.publish_state`
//!
//! # Restart
//!
//! `restart()` cleans up, sets `FD_CLOEXEC` on the new
//! process, and `exec`s the current binary. The old
//! process is replaced in place — no fork.
//!
//! # Safety
//!
//! X11 FFI calls are safe because the connection is alive
//! and the WM thread owns the connection.

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
        if matches!(action, Action::ToggleOverview | Action::OverviewNav(_))
            && self
                .engine
                .state
                .monitors
                .get(self.engine.state.sel_mon)
                .is_some_and(|m| !m.ws().overview)
        {
            if let Err(error) = overview::Overview::check_extensions(&self.conn) {
                log::warn!("Overview unavailable: {error}");
                return Ok(());
            }
            self.flush_layout()?;
        }
        let _action_trace = super::trace::Span::new("action");
        super::trace::trace!("action_input", "action={action:?}");
        let state_trace = super::trace::Span::new("state_action");
        let effects = self.engine.dispatch(action);
        drop(state_trace);
        self.run_effects(effects)?;
        Ok(())
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
            Effect::ArrangeMonitor(mi) => self.pending.mark(mi, None),
            Effect::MarkRestack(_mi) => {
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
            Effect::RefreshDesktops => {
                // The live View count is authoritative for EWMH, so the count and
                // the names are rewritten from the *selected monitor's* View list
                // rather than from `cfg.n_tags` / `cfg.tag_names` — a
                // `CreateView`/`RemoveView` at runtime changes neither, and the
                // old pairing would report a stale desktop count to pagers and
                // taskbars. `_NET_CURRENT_DESKTOP` is deliberately untouched.
                let Some(mon) = self.engine.state.monitors.get(self.engine.state.sel_mon) else {
                    return Ok(());
                };
                let n = mon.workspaces.len() as u32;
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    self.root,
                    self.atoms.net_number_of_desktops,
                    AtomEnum::CARDINAL,
                    &[n],
                );
                let mut names = Vec::new();
                for i in 0..mon.workspaces.len() {
                    let name = self
                        .engine
                        .cfg
                        .tag_names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| (i + 1).to_string());
                    names.extend_from_slice(name.as_bytes());
                    names.push(0);
                }
                let _ = self.conn.change_property8(
                    PropMode::REPLACE,
                    self.root,
                    self.atoms.net_desktop_names,
                    self.atoms.utf8_string,
                    &names,
                );
            }
            Effect::Spawn(cmd) => self.spawn(&cmd),
            Effect::Quit => self.begin_shutdown(),
            Effect::Restart => self.restart(),
            Effect::PublishIpcState => self.publish_state(),
        }
        Ok(())
    }

    /// Re-exec the WM binary in place with the EXACT arguments it was launched
    /// with, so the new instance reuses the same `--config`, `--name` and
    /// `--replace` (a real hard restart that rebuilds all state from scratch).
    ///
    /// Before exec we explicitly tear down X11 (key grabs, `SubstructureRedirect`,
    /// EWMH root props, check window) and the IPC socket + identity ficha via
    /// `cleanup()`, and mark the X connection fd `FD_CLOEXEC` so it is closed on
    /// exec — we do NOT rely on the connection layer having set CLOEXEC. `exec`
    /// replaces the process image without forking, so there is no window where
    /// two maverick instances contend over X11 grabs.
    pub(super) fn restart(&mut self) {
        use std::os::unix::process::CommandExt;

        // Stop answering as the instance *before* the socket is unbound. The
        // window in which the socket simply does not exist lasts only as long as
        // the replacement takes to start, which can be shorter than a client
        // polling for it — so a client could otherwise see this process still
        // serving before the exec and after it never return at all, and have no
        // way to tell a completed restart from an instance that never left.
        if let Some(hub) = &self.hub {
            hub.begin_restart();
        }

        // Release X11 resources + IPC + ficha so the new instance starts clean
        // and can reclaim the screen.
        let _ = self.cleanup();

        // Close the X connection fd on exec (explicit, not assumed): the new
        // process must open its own connection, not inherit this one's identity.
        let fd = self.conn.as_raw_fd();
        // SAFETY: `fd` is the fd of the live `Rc<XConn>`'s `xcb_connection_t`
        // (still owned by `self.conn` at this point); `fcntl(F_GETFD/F_SETFD)`
        // is a pure fd-flag operation with no allocation and is async-signal-safe
        // to issue from the WM thread before `exec`. The connection itself is
        // closed on exec via `FD_CLOEXEC`, not here.
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
            let args = restart_args(&self.launch_args, &self.session_id);
            let err = std::process::Command::new(exe).args(&args).exec();
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

    /// Escape hatch: force-kill (X `KillClient`) every still-managed client.
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
                    // querying has to happen through this queue. `query_json`
                    // already routes `inspect` to `inspect_json`, so there is no
                    // topic to special-case here.
                    let json =
                        crate::core::ipc::query_json(&self.engine.state, &self.engine.cfg, &topic);
                    let _ = reply.send(json);
                }
            }
        }
        Ok(())
    }

    /// Re-read the user TOML (the same fail-safe loader used at startup) and
    /// swap it in. A file that cannot be read, or does not parse, is *not* the
    /// configuration this session is running: the loader answers with the
    /// compiled baseline, and adopting that would replace the user's keybinds,
    /// rules, theme and tag count — and reconcile/clamp their windows against the
    /// baseline's tag count. A missing file is treated the same way, even though
    /// it is diagnosed silently: the current config is kept and a warning names
    /// the path and the remedy (`restart` falls back to compiled defaults). A
    /// file that *did* parse is applied even when individual values were
    /// rejected, exactly as at startup — those land in `Diagnostics`, and are not
    /// a reason to refuse the file.
    ///
    /// The decision is taken before any state is touched, so a rejected reload
    /// leaves the running configuration, the workspace list, every client's
    /// workspace and the grabbed keymap exactly as they were. If the tag count
    /// did change, every monitor's workspace list is reconciled
    /// (grown/truncated) before the new keymap is grabbed and everything is
    /// re-arranged.
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
        // `load_from_path` is the BOOT view and drops this distinction, so read
        // the classified form: `Diagnostics` cannot decide it (a missing file is
        // clean, a rejected value is still applied), and applying a compiled
        // baseline over a live configuration is never what the user asked for.
        // The check sits above `reconcile_workspaces` on purpose — it must run
        // before anything is mutated for a rejected reload to leave the session
        // untouched.
        let (source, cfg, diag) = crate::userconfig::load_from_path_classified(&path);
        crate::userconfig::dump_diagnostics(&diag);
        if source != crate::userconfig::ConfigSource::UserFile {
            log::warn!(
                "reload: '{}' is missing, unreadable or not valid TOML; keeping current config (run `maverickctl restart` to fall back to compiled defaults)",
                path.display()
            );
            return Ok(());
        }

        let tags_changed =
            cfg.n_tags != self.engine.cfg.n_tags || cfg.tag_names != self.engine.cfg.tag_names;
        // (window, monitor) pairs re-homed by the shrink, so their
        // `_NET_WM_DESKTOP` can be re-emitted at the new position.
        let mut clamped_wins: Vec<(WindowId, usize)> = Vec::new();
        if tags_changed {
            // Shrink first (one View at a time, so each removal re-homes onto a
            // View that still exists), then grow.
            for mi in 0..self.engine.state.monitors.len() {
                while self.engine.state.monitors[mi].workspaces.len() > cfg.n_tags.max(1) {
                    let pos = self.engine.state.monitors[mi].workspaces.len() - 1;
                    let survivor = self.engine.state.monitors[mi]
                        .workspaces
                        .get(pos.saturating_sub(1))
                        .map(|ws| ws.id);
                    let Some(survivor) = survivor else { break };
                    let dropped = self.engine.state.monitors[mi].workspaces[pos].id;
                    for win in self.engine.state.rehome_clients(mi, dropped, survivor) {
                        clamped_wins.push((win, mi));
                    }
                    self.engine.state.monitors[mi].remove_view_at(pos);
                }
            }
            for mon in &mut self.engine.state.monitors {
                mon.reconcile_workspaces(cfg.n_tags);
            }
        }

        self.engine.cfg = cfg;
        self.keymap = build_keymap(&self.engine.cfg);
        self.grab_keys()?;

        // Republish EWMH desktop state for external bars/taskbars. Only the
        // count/names need a refresh here — `_NET_CURRENT_DESKTOP` must NOT be
        // reset (it would yank the active tag back to 0 on every reload).
        // Any client whose workspace was clamped also needs its `_NET_WM_DESKTOP`
        // re-emitted so the new desktop index is reflected.
        if tags_changed {
            self.update_ewmh_desktop_count()?;
            for (win, mi) in clamped_wins {
                // `_NET_WM_DESKTOP` is a *position*, so it is resolved from the
                // client's new `ViewId` rather than carried over.
                let Some(mon) = self.engine.state.monitors.get(mi) else {
                    continue;
                };
                let ws = self
                    .engine
                    .state
                    .clients
                    .get(&win)
                    .and_then(|c| mon.view_index(c.workspace))
                    .unwrap_or(0);
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
    /// Granular `focus`/`workspace` lines for `subscribe` clients do not come
    /// from here: they are produced by the typed `EventBus` via `HubEventSink`,
    /// so a single source of truth describes every transition.
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

/// Build the argv for a restart re-exec, preserving the session id.
///
/// The session id is the filesystem key for the runtime directory, the control
/// socket and the identity ficha, and a hand-started WM's argv carries no
/// `--session-id` — so a re-exec from `launch_args` alone mints a fresh random
/// id and relocates all three. Anything that resolved the instance before the
/// restart (`MAVERICK_INSTANCE`, exported to every child at startup) then names
/// paths that no longer exist. Re-passing the current id keeps the socket path
/// stable across restarts.
fn restart_args(launch_args: &[String], session_id: &str) -> Vec<String> {
    let mut args = launch_args.to_vec();
    if !args.iter().any(|a| a == "--session-id") {
        args.push("--session-id".to_string());
        args.push(session_id.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::restart_args;

    #[test]
    fn restart_preserves_a_random_session_id() {
        let launch = vec!["--debug".to_string()];
        let args = restart_args(&launch, "abc123-def456");
        assert_eq!(
            args,
            vec![
                "--debug".to_string(),
                "--session-id".to_string(),
                "abc123-def456".to_string()
            ]
        );
    }

    #[test]
    fn restart_leaves_an_explicit_session_id_untouched() {
        let launch = vec![
            "--session-id".to_string(),
            "debug".to_string(),
            "--debug".to_string(),
        ];
        let args = restart_args(&launch, "should-not-be-used");
        assert_eq!(
            args,
            vec![
                "--session-id".to_string(),
                "debug".to_string(),
                "--debug".to_string()
            ]
        );
    }

    #[test]
    fn restart_appends_to_empty_launch_args() {
        let args = restart_args(&[], "sid");
        assert_eq!(args, vec!["--session-id".to_string(), "sid".to_string()]);
    }
}
