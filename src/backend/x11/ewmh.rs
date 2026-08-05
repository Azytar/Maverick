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

    pub(super) fn update_ewmh_desktops(&self) -> Result<(), Box<dyn std::error::Error>> {
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

        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_current_desktop,
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: self.atoms.wm_protocols,
            data: ClientMessageData::from([proto, x11rb::CURRENT_TIME, 0, 0, 0]),
        };
        let _ = self.conn.send_event(false, win, EventMask::NO_EVENT, ev);
        Ok(())
    }
}
