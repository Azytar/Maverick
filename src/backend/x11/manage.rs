use super::*;

impl WindowManager {
    pub(super) fn scan_windows(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let tree = match self.conn.query_tree(self.root)?.reply() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to query window tree: {}", e);
                return Ok(());
            }
        };

        // P13: Pipeline window attributes — fire all requests, then collect replies.
        let mut cookies: Vec<(Window, _)> = Vec::with_capacity(tree.children.len());
        for &w in &tree.children {
            if let Ok(c) = self.conn.get_window_attributes(w) {
                cookies.push((w, c));
            }
        }
        let mut wins = Vec::with_capacity(cookies.len());
        for (w, c) in cookies {
            if let Ok(a) = c.reply() {
                wins.push((w, a));
            }
        }

        for (w, a) in wins {
            if !a.override_redirect && a.map_state == MapState::VIEWABLE {
                if let Err(e) = self.manage(w, &a) {
                    log::warn!("Failed to manage window {}: {}", w, e);
                }
            }
        }
        Ok(())
    }

    pub(super) fn manage(
        &mut self,
        win: Window,
        attrs: &GetWindowAttributesReply,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if attrs.override_redirect {
            let _ = self.conn.map_window(win);
            return Ok(());
        }
        if self.engine.state.clients.contains_key(&win) {
            return Ok(());
        }

        let geom_r = match self.conn.get_geometry(win)?.reply() {
            Ok(g) => g,
            Err(e) => {
                log::warn!("Failed to get geometry for window {}: {}", win, e);
                return Ok(());
            }
        };
        let geom = Rect::new(
            geom_r.x as i32,
            geom_r.y as i32,
            geom_r.width as u32,
            geom_r.height as u32,
        );

        let mon_idx = self.engine.state.sel_mon;
        let ws_idx = self.engine.state.monitors[mon_idx].active_ws;

        let mut client = Client::new(win, mon_idx, ws_idx);
        client.geom = geom;
        client.saved_geom = geom;
        client.border_w = self.engine.cfg.border_w;

        let mut unmanaged = false;

        // P1: Pipeline all property reads — fire all requests before any reply.
        // Scoped so Cookies (which borrow self.conn) are dropped before &mut self calls below.
        {
            let c_title_net = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_name,
                self.atoms.utf8_string,
                0,
                256,
            )?;
            let c_title_wm =
                self.conn
                    .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)?;
            let c_class =
                self.conn
                    .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?;
            let c_wtype = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_window_type,
                AtomEnum::ATOM,
                0,
                32,
            )?;
            let c_wstate = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_state,
                AtomEnum::ATOM,
                0,
                32,
            )?;
            let c_hints = self.conn.get_property(
                false,
                win,
                self.atoms.wm_hints,
                self.atoms.wm_hints,
                0,
                9,
            )?;
            let c_size = self.conn.get_property(
                false,
                win,
                AtomEnum::WM_NORMAL_HINTS,
                AtomEnum::WM_SIZE_HINTS,
                0,
                18,
            )?;

            // Process title (net_wm_name with WM_NAME fallback)
            if let Ok(ref prop) = c_title_net.reply() {
                if !prop.value.is_empty() {
                    client.name = String::from_utf8_lossy(&prop.value).into_owned();
                }
            }
            if client.name.is_empty() {
                if let Ok(ref prop) = c_title_wm.reply() {
                    client.name = String::from_utf8_lossy(&prop.value).into_owned();
                }
            }

            // Process class
            if let Ok(ref prop) = c_class.reply() {
                let s = String::from_utf8_lossy(&prop.value);
                let mut parts = s.split('\0');
                client.instance = parts.next().unwrap_or("").to_string();
                client.class = parts.next().unwrap_or("").to_string();
            }

            // Process window type
            if let Ok(ref prop) = c_wtype.reply() {
                if prop.type_ == u32::from(AtomEnum::ATOM) {
                    let atoms: Vec<u32> = prop.value32().map(std::iter::Iterator::collect).unwrap_or_default();
                    for a in atoms {
                        if a == self.atoms.net_wm_window_type_desktop
                            || a == self.atoms.net_wm_window_type_dock
                        {
                            client.is_unmanaged = true;
                        }
                        if a == self.atoms.net_wm_window_type_dialog
                            || a == self.atoms.net_wm_window_type_utility
                            || a == self.atoms.net_wm_window_type_menu
                            || a == self.atoms.net_wm_window_type_toolbar
                            || a == self.atoms.net_wm_window_type_splash
                        {
                            client.flags.set(WinFlags::FLOAT);
                            client.is_dialog = true;
                        }
                    }
                }
            }

            if client.is_unmanaged {
                if let Err(e) = self.conn.map_window(win) {
                    log::warn!("failed to map window {}: {}", win, e);
                }
                unmanaged = true;
            }

            // Process window state
            if let Ok(ref sp) = c_wstate.reply() {
                if sp.type_ == u32::from(AtomEnum::ATOM) {
                    let atoms: Vec<u32> = sp.value32().map(std::iter::Iterator::collect).unwrap_or_default();
                    for a in atoms {
                        if a == self.atoms.net_wm_state_fullscreen {
                            client.flags.set(WinFlags::FULLSCREEN);
                        }
                        if a == self.atoms.net_wm_state_maximized_vert
                            || a == self.atoms.net_wm_state_maximized_horiz
                        {
                            client.flags.set(WinFlags::MAXIMIZED);
                        }
                        if a == self.atoms.net_wm_state_modal {
                            client.flags.set(WinFlags::FLOAT);
                            client.is_dialog = true;
                        }
                    }
                }
            }

            // Process WM hints
            if let Ok(ref prop) = c_hints.reply() {
                if let Some(vals) = prop.value32() {
                    let v: Vec<u32> = vals.collect();
                    if !v.is_empty() {
                        if v[0] & 1 != 0 && v.len() > 1 {
                            if v[1] == 0 {
                                client.flags.set(WinFlags::NO_FOCUS);
                                client.wants_input = false;
                            } else {
                                client.wants_input = true;
                            }
                        }
                        if v[0] & 256 != 0 {
                            client.flags.set(WinFlags::URGENT);
                        }
                    }
                }
            }

            // Process size hints
            if let Ok(ref prop) = c_size.reply() {
                if let Some(vals) = prop.value32() {
                    let v: Vec<u32> = vals.collect();
                    if v.len() >= 18 {
                        let f = v[0];
                        let h = &mut client.hints;
                        if f & 16 != 0 {
                            h.min_w = v[9] as i32;
                            h.min_h = v[10] as i32;
                        }
                        if f & 32 != 0 {
                            h.max_w = v[11] as i32;
                            h.max_h = v[12] as i32;
                        }
                        if f & 64 != 0 {
                            h.inc_w = v[13] as i32;
                            h.inc_h = v[14] as i32;
                        }
                        if f & 128 != 0 {
                            let denom = v[16].max(1);
                            h.min_aspect = v[15] as f32 / denom as f32;
                            h.max_aspect = v[17] as f32 / denom as f32;
                        }
                        if f & 256 != 0 {
                            h.base_w = v[7] as i32;
                            h.base_h = v[8] as i32;
                        }
                        h.valid = true;
                        if h.max_w > 0 && h.max_h > 0 && h.max_w == h.min_w && h.max_h == h.min_h {
                            client.flags.set(WinFlags::FIXED);
                            client.flags.set(WinFlags::FLOAT);
                        }
                    }
                }
            }
        } // cookies dropped here

        if unmanaged {
            // Docks reserve screen space via _NET_WM_STRUT[_PARTIAL].
            self.apply_dock_strut(win)?;
            return Ok(());
        }

        // transient → inherit parent workspace
        if let Some(parent) = self.transient_for(win)? {
            if let Some(pc) = self.engine.state.clients.get(&parent) {
                client.workspace = pc.workspace;
                client.monitor = pc.monitor;
                client.flags.set(WinFlags::FLOAT);
            }
        }

        self.apply_rules(&mut client);
        self.detect_portal(&mut client);

        // configure border
        let _ = self.conn.configure_window(
            win,
            &ConfigureWindowAux::new().border_width(client.border_w),
        );
        let _ = self.conn.change_window_attributes(
            win,
            &ChangeWindowAttributesAux::new()
                .border_pixel(self.engine.cfg.col_normal)
                .event_mask(
                    EventMask::ENTER_WINDOW
                        | EventMask::FOCUS_CHANGE
                        | EventMask::PROPERTY_CHANGE
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        );

        self.grab_buttons(win, false)?;

        let bw = client.border_w;
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_frame_extents,
            AtomEnum::CARDINAL,
            &[bw, bw, bw, bw],
        );
        let _ = self.set_wm_state(win, 1);

        // place into workspace structure
        let ws_i = client.workspace;
        let mon_i = client.monitor;
        let is_fl = client.is_float();

        self.engine.state.add_client(client);

        if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
            if is_fl {
                self.engine.state.monitors[mon_i].workspaces[ws_i]
                    .floats
                    .push(win);
                self.stack_dirty = true;
            } else {
                let dw = self.engine.cfg.default_col_w;
                let workarea_w = self.engine.state.monitors[mon_i].workarea.w;
                self.engine.state.monitors[mon_i].workspaces[ws_i].add_tiled(win, dw, workarea_w);
            }
        }

        self.client_list_dirty = true;

        // Inform EWMH-aware taskbars (polybar, eww, etc.) which desktop this window is on.
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_wm_desktop,
            AtomEnum::CARDINAL,
            &[ws_i as u32],
        );

        let _ = self.conn.map_window(win);

        // scroll & arrange
        {
            let scroll = ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
            self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
        }
        self.arrange(mon_i)?;
        self.focus(Some(win))?;

        Ok(())
    }

    pub(super) fn unmanage(&mut self, win: Window, destroyed: bool) -> Result<(), Box<dyn std::error::Error>> {
        // 1. If already removed (e.g. double Unmap + Destroy event), exit silently.
        let client = match self.engine.state.remove_client(win) {
            Some(c) => c,
            None => return Ok(()),
        };

        if !destroyed {
            let _ = self.conn.configure_window(
                win,
                &ConfigureWindowAux::new().border_width(client.old_border_w),
            );
            let _ = self.set_wm_state(win, 0);
            let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        }

        self.client_list_dirty = true;
        let mon_i = client.monitor;

        // 2. Avoid panic if the monitor no longer exists after a hotplug.
        if mon_i < self.engine.state.monitors.len() {
            let scroll = ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
            let ws_i = client.workspace;
            if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
                self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
            }
            let _ = self.arrange(mon_i);
            let _ = self.focus_best(mon_i);
        }
        Ok(())
    }

    pub(super) fn apply_rules(&self, c: &mut Client) {
        for rule in &self.engine.cfg.rules {
            if rule.matches(&c.class, &c.name) {
                if rule.float {
                    c.flags.set(WinFlags::FLOAT);
                }
                if let Some(ws) = rule.ws {
                    let mi = c.monitor;
                    if mi < self.engine.state.monitors.len()
                        && ws < self.engine.state.monitors[mi].workspaces.len()
                    {
                        c.workspace = ws;
                    }
                }
            }
        }
    }

    pub(super) fn detect_portal(&self, c: &mut Client) {
        let float_classes = [
            "xdg-desktop-portal",
            "flameshot",
            "gpick",
            "pinentry",
            "screenkey",
        ];
        let float_titles = [
            "file upload",
            "open file",
            "save file",
            "file chooser",
            "qt file dialog",
            "choose file",
            "select file",
        ];
        let cl = c.class.to_lowercase();
        let ti = c.name.to_lowercase();
        if float_classes.iter().any(|fc| cl.contains(fc))
            || float_titles.iter().any(|ft| ti.contains(ft))
        {
            c.flags.set(WinFlags::FLOAT);
            if cl.contains("flameshot") {
                c.border_w = 0;
            }
        }
    }

    pub(super) fn transient_for(&self, win: Window) -> Result<Option<Window>, Box<dyn std::error::Error>> {
        let prop = self
            .conn
            .get_property(
                false,
                win,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;
        Ok(prop
            .value32()
            .and_then(|mut v| v.next())
            .filter(|&w| w != 0 && w != self.root))
    }

    pub(super) fn refresh_title(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        // Inline read without cloning the entire Client
        let name = read_title_value(&self.conn, win, &self.atoms)?;
        if let Some(name) = name {
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                c.name = name;
            }
        }
        Ok(())
    }

    pub(super) fn refresh_hints(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let hints = read_wm_hints_value(&self.conn, win)?;
        if let Some((no_focus, wants_input, urgent)) = hints {
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                if no_focus {
                    c.flags.set(WinFlags::NO_FOCUS);
                }
                c.wants_input = wants_input;
                if urgent {
                    c.flags.set(WinFlags::URGENT);
                }
            }
        }
        Ok(())
    }

    pub(super) fn find_client(&self, mut win: Window) -> Option<Window> {
        if self.engine.state.clients.contains_key(&win) {
            return Some(win);
        }
        loop {
            let tree = self.conn.query_tree(win).ok()?.reply().ok()?;
            let parent = tree.parent;
            if parent == self.root || parent == win || parent == x11rb::NONE {
                return None;
            }
            win = parent;
            if self.engine.state.clients.contains_key(&win) {
                return Some(win);
            }
        }
    }

    pub(super) fn set_fullscreen(&mut self, win: Window, fs: bool) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            if fs == c.is_fullscreen() {
                return Ok(());
            }
            if fs {
                c.flags.set(WinFlags::FULLSCREEN);
                c.saved_geom = c.geom;
                c.old_border_w = c.border_w;
                c.border_w = 0;
                // Force geom to a sentinel so apply_geom doesn't skip the X11 call
                c.geom = Rect::default();
            } else {
                c.flags.clear(WinFlags::FULLSCREEN);
                c.border_w = c.old_border_w;
                // Floats are laid out from `client.geom`, so restore the saved
                // pre-fullscreen rect (else they collapse to the zero sentinel).
                // Tiled windows are re-laid by arrange, so the sentinel is fine.
                if c.is_float() {
                    c.geom = c.saved_geom;
                } else {
                    c.geom = Rect::default();
                }
            }
            self.stack_dirty = true;
        }
        self.write_net_wm_state(win);
        let mi = self
            .engine
            .state
            .clients
            .get(&win)
            .map_or(0, |c| c.monitor);
        self.arrange(mi)?;
        // Raise fullscreen windows above everything else
        if self
            .engine
            .state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_fullscreen)
        {
            let _ = self
                .conn
                .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        Ok(())
    }

    /// Toggle the maximized presentation state of `win`. Mirrors
    /// `set_fullscreen`, but a maximized window fills the monitor's `workarea`
    /// (bar/docks respected) and keeps its border — the geometry itself is
    /// produced by `core::present`, so here we only manage flags + EWMH + raise.
    pub(super) fn set_maximized(
        &mut self,
        win: Window,
        max: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            if max == c.is_maximized() {
                return Ok(());
            }
            if max {
                c.flags.set(WinFlags::MAXIMIZED);
                c.saved_geom = c.geom;
                // Sentinel so apply_geom doesn't skip the X11 call.
                c.geom = Rect::default();
            } else {
                c.flags.clear(WinFlags::MAXIMIZED);
                if c.is_float() {
                    c.geom = c.saved_geom;
                } else {
                    c.geom = Rect::default();
                }
            }
            self.stack_dirty = true;
        }
        self.write_net_wm_state(win);
        let mi = self
            .engine
            .state
            .clients
            .get(&win)
            .map_or(0, |c| c.monitor);
        self.arrange(mi)?;
        if self
            .engine
            .state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_maximized)
        {
            let _ = self
                .conn
                .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        Ok(())
    }

    /// Rewrite `_NET_WM_STATE` for `win` from the client's current flags, so
    /// every active EWMH state (fullscreen, maximized, …) is advertised
    /// consistently. Setting a single state must not clobber the others.
    fn write_net_wm_state(&self, win: Window) {
        let Some(c) = self.engine.state.clients.get(&win) else {
            return;
        };
        let mut state_atoms = Vec::new();
        if c.is_fullscreen() {
            state_atoms.push(self.atoms.net_wm_state_fullscreen);
        }
        if c.is_maximized() {
            state_atoms.push(self.atoms.net_wm_state_maximized_vert);
            state_atoms.push(self.atoms.net_wm_state_maximized_horiz);
        }
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_wm_state,
            AtomEnum::ATOM,
            &state_atoms,
        );
    }
}
