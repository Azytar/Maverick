//! EWMH property publishing on the root window.
//!
//! `_NET_SUPPORTED` advertises exactly the atoms Maverick
//! handles — no phantom atoms. The root properties are
//! updated on workspace/monitor changes, fullscreen
//! transitions, and client list/stack changes.
//!
//! # Protocol semantics
//!
//! - `_NET_WORKAREA` — the physical workarea bounds, once per live desktop.
//!   Written as a flat CARDINAL array, independent of monitor count.
//! - `_NET_DESKTOP_GEOMETRY` — the full screen rect.
//! - `_NET_NUMBER_OF_DESKTOPS` — the largest live View count across monitors.
//! - `_NET_DESKTOP_NAMES` — nul-separated UTF-8.
//! - `_NET_CURRENT_DESKTOP` — published by
//!   `Effect::SetCurrentDesktop`; only the initial startup
//!   publish resets it to 0.
//! - `_NET_CLIENT_LIST` / `_NET_CLIENT_LIST_STACKING`
//!   — tracked windows in the X server's bottom-to-top stack order.
//!   Updated lazily (deferred dirty coalesce).
//! - `_NET_WM_STATE` — `WM_STATE` normal (1) on manage;
//!   `write_net_wm_state` rewrites the atom list
//!   preserving urgent.
//! - `_NET_ACTIVE_WINDOW` — set on focus.
//!
//! # Invariants
//!
//! - `flush_client_list` is deferred (dirty coalesce) so
//!   multiple changes in one frame produce one write.
//! - `update_ewmh_desktops` resets `_NET_CURRENT_DESKTOP`
//!   to 0; `update_ewmh_desktop_count` never does, so a
//!   workspace reconcile cannot yank the active desktop.

use super::*;

fn desktop_count(monitors: &[Monitor]) -> usize {
    monitors
        .iter()
        .map(|mon| mon.workspaces.len())
        .max()
        .unwrap_or(0)
}

fn workarea_property(monitors: &[Monitor]) -> Vec<u32> {
    let Some(first) = monitors.first() else {
        return Vec::new();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (
        first.workarea.x as i64,
        first.workarea.y as i64,
        first.workarea.x as i64 + first.workarea.w as i64,
        first.workarea.y as i64 + first.workarea.h as i64,
    );
    for mon in &monitors[1..] {
        let wa = mon.workarea;
        x0 = x0.min(wa.x as i64);
        y0 = y0.min(wa.y as i64);
        x1 = x1.max(wa.x as i64 + wa.w as i64);
        y1 = y1.max(wa.y as i64 + wa.h as i64);
    }
    [
        x0 as u32,
        y0 as u32,
        (x1 - x0).clamp(1, u32::MAX as i64) as u32,
        (y1 - y0).clamp(1, u32::MAX as i64) as u32,
    ]
    .repeat(desktop_count(monitors))
}

impl WindowManager {
    pub(super) fn update_workarea(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        if self.engine.state.monitors.is_empty() {
            return Ok(());
        }
        let data = workarea_property(&self.engine.state.monitors);
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
        // `_NET_DESKTOP_GEOMETRY` is the size of the virtual desktop, not
        // one monitor: the bounding box of all screens (multi-monitor
        // aware). Saturating: hostile ±2G origins can never wrap the cast.
        let (mut x0, mut y0, mut x1, mut y1) = (
            first_mon.screen.x as i64,
            first_mon.screen.y as i64,
            first_mon.screen.x as i64 + first_mon.screen.w as i64,
            first_mon.screen.y as i64 + first_mon.screen.h as i64,
        );
        for mon in &self.engine.state.monitors[1..] {
            let s = &mon.screen;
            x0 = x0.min(s.x as i64);
            y0 = y0.min(s.y as i64);
            x1 = x1.max(s.x as i64 + s.w as i64);
            y1 = y1.max(s.y as i64 + s.h as i64);
        }
        let vw = (x1 - x0).clamp(1, u32::MAX as i64) as u32;
        let vh = (y1 - y0).clamp(1, u32::MAX as i64) as u32;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_desktop_geometry,
                AtomEnum::CARDINAL,
                &[vw, vh],
            )?
            .check()?;
        Ok(())
    }

    /// Rewrite `_NET_NUMBER_OF_DESKTOPS` and `_NET_DESKTOP_NAMES` to match the
    /// live Views. Unlike `update_ewmh_desktops`, this deliberately leaves
    /// `_NET_CURRENT_DESKTOP` untouched — callers that reconcile workspaces
    /// (e.g. `reload_config`) must not reset the active desktop to 0.
    pub(super) fn update_ewmh_desktop_count(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        let n = desktop_count(&self.engine.state.monitors) as u32;

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
        for i in 0..n as usize {
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
        // Sorted for stability: `HashMap` iteration order is random, and an
        // unstable list makes taskbars reorder on every manage/unmanage.
        let mut wins: Vec<u32> = self.engine.state.clients.keys().copied().collect();
        wins.sort_unstable();
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

    /// Publish the server's stack after pending raises, including tracked docks.
    pub(super) fn update_client_list_stacking(&self) -> Result<(), Box<dyn std::error::Error>> {
        let out: Vec<_> = self
            .conn
            .query_tree(self.root)?
            .reply()?
            .children
            .into_iter()
            .filter(|win| {
                self.engine.state.clients.contains_key(win) || self.docks.contains_key(win)
            })
            .collect();
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
    /// through IPC, so an external bar has a status source without having to
    /// parse `xsetroot` output.
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
    /// flushed at settled boundaries in `run_once`, before it can wait.
    /// A call with no dirty state emits no requests.
    pub(super) fn flush_client_list(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.client_list_dirty {
            self.client_list_dirty = false;
            self.update_client_list()?;
            self.stacking_dirty = true;
        }
        if self.stacking_dirty {
            self.stacking_dirty = false;
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

    /// Whether `win` lists `proto` in `WM_PROTOCOLS`.
    ///
    /// Served from `self.protocols`; only a window not seen yet (or whose
    /// property changed) costs a `GetProperty` round trip, and `manage` warms
    /// the cache so that read happens at map time, never on the input path. A
    /// failed read (the window is gone) is not cached.
    pub(super) fn has_protocol(
        &self,
        win: Window,
        proto: u32,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if let Some(list) = self.protocols.borrow().get(&win) {
            return Ok(list.contains(&proto));
        }
        let reply = self
            .conn
            .get_property(false, win, self.atoms.wm_protocols, AtomEnum::ATOM, 0, 32)?
            .reply();
        let Ok(prop) = reply else {
            return Ok(false);
        };
        let list: Vec<u32> = prop.value32().map(Iterator::collect).unwrap_or_default();
        let hit = list.contains(&proto);
        self.protocols.borrow_mut().insert(win, list);
        Ok(hit)
    }

    pub(super) fn send_proto(
        &self,
        win: Window,
        proto: u32,
        time: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // ICCCM forbids CurrentTime in WM_TAKE_FOCUS, even at startup.
        if proto == self.atoms.wm_take_focus && time == x11rb::CURRENT_TIME {
            return Err("WM_TAKE_FOCUS requires a server timestamp".into());
        }
        if proto == self.atoms.wm_take_focus {
            trace!("focus_offer", "window={win:#x} x_time_ms={time}");
        }
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

    pub(super) fn offer_take_focus(
        &mut self,
        win: Window,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cookie = self.conn.change_property8(
            PropMode::REPLACE,
            self.check_win,
            self.atoms.maverick_timestamp,
            AtomEnum::INTEGER,
            &[0],
        )?;
        // PropertyNotify is stamped after the preceding focus request. Match
        // the request sequence so a superseded offer cannot use an older reply.
        self.take_focus_pending = Some((win, cookie.sequence_number() as u16));
        trace!(
            "focus_offer_pending",
            "window={win:#x} sequence={}",
            cookie.sequence_number()
        );
        Ok(())
    }

    fn declared_focus_group(
        &self,
        win: Window,
    ) -> Result<Option<Window>, Box<dyn std::error::Error>> {
        let hints =
            self.conn
                .get_property(false, win, self.atoms.wm_hints, self.atoms.wm_hints, 0, 9)?;
        let leader = self.conn.get_property(
            false,
            win,
            self.atoms.wm_client_leader,
            AtomEnum::WINDOW,
            0,
            1,
        )?;
        let Ok(hints) = hints.reply() else {
            return Ok(None);
        };
        let words: Vec<u32> = hints.value32().map(Iterator::collect).unwrap_or_default();
        if words.first().is_some_and(|flags| flags & 64 != 0) {
            if let Some(&group) = words.get(8).filter(|&&group| group != x11rb::NONE) {
                return Ok(Some(group));
            }
        }
        Ok(leader
            .reply()
            .ok()
            .and_then(|reply| reply.value32().and_then(|mut values| values.next()))
            .filter(|&group| group != x11rb::NONE))
    }

    pub(super) fn shares_focus_group(
        &self,
        from: Window,
        to: Window,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // These ICCCM declarations describe an application family, not an
        // authenticated identity. PID and WM_CLASS are not focus authority.
        let from = self.declared_focus_group(from)?;
        Ok(from.is_some() && from == self.declared_focus_group(to)?)
    }
}

#[cfg(test)]
mod workarea_tests {
    use super::*;

    #[test]
    fn workareas_have_one_rectangle_per_live_desktop_not_per_output() {
        let mut left = Monitor::new(Rect::new(0, 0, 800, 600), 3);
        let mut right = Monitor::new(Rect::new(800, 0, 800, 600), 4);
        left.workarea = Rect::new(0, 24, 800, 576);
        right.workarea = Rect::new(800, 24, 800, 576);
        assert_eq!(
            workarea_property(&[left, right]),
            [0, 24, 1600, 576].repeat(4)
        );
        assert!(workarea_property(&[]).is_empty());
    }
}
