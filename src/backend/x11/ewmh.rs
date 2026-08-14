use super::*;

impl WindowManager {
    pub(super) fn update_workarea(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        let n = self.engine.cfg.n_tags;
        if self.engine.state.monitors.is_empty() {
            return Ok(());
        }
        let mut data = Vec::with_capacity(self.engine.state.monitors.len() * n * 4);
        for mon in &self.engine.state.monitors {
            for _ in 0..n {
                data.push(mon.workarea.x as u32);
                data.push(mon.workarea.y as u32);
                data.push(mon.workarea.w);
                data.push(mon.workarea.h);
            }
        }
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_workarea,
                AtomEnum::CARDINAL,
                &data,
            )?
            .check()?;

        let first_mon = &self.engine.state.monitors[0];
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_desktop_geometry,
                AtomEnum::CARDINAL,
                &[first_mon.workarea.w, first_mon.workarea.h],
            )?
            .check()?;
        Ok(())
    }

    /// Rewrite `_NET_NUMBER_OF_DESKTOPS` and `_NET_DESKTOP_NAMES` to match the
    /// current config. Unlike `update_ewmh_desktops`, this deliberately leaves
    /// `_NET_CURRENT_DESKTOP` untouched — callers that reconcile workspaces
    /// (e.g. `reload_config`) must not reset the active desktop to 0.
    pub(super) fn update_ewmh_desktop_count(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        let n = self.engine.cfg.n_tags as u32;

        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_number_of_desktops,
                AtomEnum::CARDINAL,
                &[n],
            )?
            .check()?;

        let mut names = Vec::new();
        for name in &self.engine.cfg.tag_names {
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        self.conn
            .change_property8(
                PropMode::REPLACE,
                self.root,
                a.net_desktop_names,
                a.utf8_string,
                &names,
            )?
            .check()?;
        Ok(())
    }

    pub(super) fn update_ewmh_desktops(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.update_ewmh_desktop_count()?;

        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_current_desktop,
                AtomEnum::CARDINAL,
                &[0u32],
            )?
            .check()?;
        Ok(())
    }

    pub(super) fn update_client_list(&self) -> Result<(), Box<dyn std::error::Error>> {
        let wins: Vec<u32> = self.engine.state.clients.keys().copied().collect();
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_client_list,
                AtomEnum::WINDOW,
                &wins,
            )?
            .check()?;
        Ok(())
    }

    /// Rewrite `_NET_CLIENT_LIST_STACKING` — the client list in bottom-to-top
    /// stack order, consumed by taskbars, Alt+Tab switchers (rofi -windowdmenu,
    /// i3lock-style UIs) and EWMH clients that `XmuClientWindow`-walk the stack.
    /// Not perfectly the raw X Z-order (Maverick re-stacks programmatically in
    /// `stack_overlay`), but a faithful, deterministic model of it: tiled then
    /// floats per workspace, then anything left over in most-recently-focused
    /// order on top.
    pub(super) fn update_client_list_stacking(&self) -> Result<(), Box<dyn std::error::Error>> {
        let state = &self.engine.state;
        let mut out: Vec<u32> = Vec::with_capacity(state.clients.len());
        let mut seen = std::collections::HashSet::with_capacity(state.clients.len());
        for mon in &state.monitors {
            for ws in &mon.workspaces {
                for col in &ws.columns {
                    for &w in &col.windows {
                        if seen.insert(w) {
                            out.push(w);
                        }
                    }
                }
                for &w in &ws.floats {
                    if seen.insert(w) {
                        out.push(w);
                    }
                }
            }
        }
        // Any client not represented in the tiling tree (hidden/inactive-wo
        // state, hotplug leftovers) goes on top in focus-recency order.
        for mon in &state.monitors {
            for &w in mon.focus_stack.iter().rev() {
                if seen.insert(w) {
                    out.push(w);
                }
            }
        }
        for &w in state.clients.keys() {
            if seen.insert(w) {
                out.push(w);
            }
        }
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_client_list_stacking,
                AtomEnum::WINDOW,
                &out,
            )?
            .check()?;
        Ok(())
    }

    /// Read the root window's `WM_NAME` into `state.status`. External bars
    /// (polybar, waybar, …) and `maverickctl state`/`subscribe` consume this
    /// through IPC; the WM no longer renders it itself. Kept when the internal
    /// bar was removed so an external bar still has a status source without
    /// having to parse `xsetroot` output.
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
        Ok(())
    }

    /// Drain the deferred `_NET_CLIENT_LIST` update. Set on manage/unmanage and
    /// flushed once per event-loop iteration (in `run_once`) so a burst of
    /// window changes rewrites the property at most once. This used to live in
    /// the bar module's `flush_bars`; it was preserved when the internal bar
    /// was removed because it is EWMH bookkeeping, not bar drawing.
    pub(super) fn flush_client_list(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.client_list_dirty {
            self.client_list_dirty = false;
            self.update_client_list()?;
            self.update_client_list_stacking()?;
        }
        Ok(())
    }

    pub(super) fn set_wm_state(
        &self,
        win: Window,
        state: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.conn
            .change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.wm_state,
                self.atoms.wm_state,
                &[state, x11rb::NONE],
            )?
            .check()?;
        Ok(())
    }

    pub(super) fn has_protocol(
        &self,
        win: Window,
        proto: u32,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let prop = self
            .conn
            .get_property(false, win, self.atoms.wm_protocols, AtomEnum::ATOM, 0, 32)?
            .reply();
        Ok(prop
            .ok()
            .and_then(|p| p.value32().map(|mut v| v.any(|x| x == proto)))
            .unwrap_or(false))
    }

    pub(super) fn send_proto(
        &self,
        win: Window,
        proto: u32,
        time: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // ICCCM 4.1.4: `WM_TAKE_FOCUS` must carry a real server timestamp
        // (the latest key/button event) rather than `CurrentTime`; some strict
        // toolkits (Java Swing, some Emacs builds) discard CurrentTime-based
        // focus messages. Fall back to `CurrentTime` only when no input event
        // has been recorded yet.
        let time = if time != 0 { time } else { x11rb::CURRENT_TIME };
        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: self.atoms.wm_protocols,
            data: ClientMessageData::from([proto, time, 0, 0, 0]),
        };
        let _ = self.conn.send_event(false, win, EventMask::NO_EVENT, ev);
        Ok(())
    }
}
