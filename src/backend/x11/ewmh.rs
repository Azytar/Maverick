//! EWMH property publishing on the root window.
//!
//! `_NET_SUPPORTED` advertises exactly the atoms Maverick
//! handles — no phantom atoms. The root properties are
//! updated on workspace/monitor changes, fullscreen
//! transitions, and client list/stack changes.
//!
//! # Protocol semantics
//!
//! - `_NET_WORKAREA` — `mon.workarea` × `n_tags`.
//!   The workarea is the screen minus reserved regions
//!   (docks). Written as a flat CARDINAL array.
//! - `_NET_DESKTOP_GEOMETRY` — the full screen rect.
//! - `_NET_NUMBER_OF_DESKTOPS` — `n_tags`.
//! - `_NET_DESKTOP_NAMES` — nul-separated UTF-8.
//! - `_NET_CURRENT_DESKTOP` — published by
//!   `Effect::SetCurrentDesktop`; only the initial startup
//!   publish resets it to 0.
//! - `_NET_CLIENT_LIST` / `_NET_CLIENT_LIST_STACKING`
//!   — bottom-to-top: tiled → floats → `focus_stack` →
//!   remaining. Updated lazily (deferred dirty coalesce).
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

    /// Clients that no other rule placed, in a stable order.
    ///
    /// This is the only part of the stacking list that came straight out of a
    /// `HashMap`, and `HashMap` iteration order is randomised per process. The
    /// result is the bottom-to-top Z order published in
    /// `_NET_CLIENT_LIST_STACKING` and re-published on every restack, so the
    /// leftover windows reshuffled between runs and between frames — visible as a
    /// taskbar reordering its entries as focus moved. Both other sources in this
    /// function already sort, and the comment above the dock loop says so; this
    /// one had been missed.
    ///
    /// Sorted rather than insertion-ordered on purpose: an ordered map would key
    /// the list on *manage* history, which changes across a restart, so it would
    /// trade one non-reproducible order for a different one.
    fn leftovers(clients: &std::collections::HashMap<WindowId, Client>) -> Vec<u32> {
        let mut out: Vec<u32> = clients.keys().copied().collect();
        out.sort_unstable();
        out
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
        for w in Self::leftovers(&state.clients) {
            if seen.insert(w) {
                out.push(w);
            }
        }
        // Docks (override-redirect panels) are not `clients`, but they ARE
        // mapped windows above the tiling tree — pagers and `rofi
        // -windowdmenu` must see them. Sorted for stability, stacked topmost.
        let mut docks: Vec<u32> = self.docks.keys().copied().collect();
        docks.sort_unstable();
        for w in docks {
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
    /// flushed once per event-loop iteration (in `run_once`) so a burst of
    /// window changes rewrites the property at most once.
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

#[cfg(test)]
mod stacking_order_tests {
    use super::*;
    use maverick_core::types::Client;
    use std::collections::HashMap;

    fn clients(ids: &[u32]) -> HashMap<WindowId, Client> {
        ids.iter().map(|id| (*id, Client::new(*id, 0, 0))).collect()
    }

    /// The published stack order must not depend on `HashMap` iteration order.
    ///
    /// This is not a cosmetic property. `_NET_CLIENT_LIST_STACKING` is the
    /// bottom-to-top Z order, consumed by taskbars and Alt-Tab switchers, and
    /// it is rewritten on every restack — so an unstable order made those
    /// consumers reshuffle as focus moved, and differently again on the next
    /// run of the same session.
    #[test]
    fn the_leftover_order_is_stable_across_insertion_orders() {
        let ids: [u32; 4] = [0x31, 0x7, 0x2ab, 0x1001];
        let expected: Vec<u32> = {
            let mut e = ids.to_vec();
            e.sort_unstable();
            e
        };
        // Insert in several orders into freshly built maps. `HashMap` seeds
        // its hasher per map, so iteration order genuinely differs between
        // these; the result must not.
        let orders: [Vec<u32>; 2] = [ids.to_vec(), ids.iter().rev().copied().collect()];
        for order in orders {
            let map = clients(&order);
            assert_eq!(WindowManager::leftovers(&map), expected);
        }
    }

    #[test]
    fn an_empty_client_map_yields_no_leftovers() {
        assert!(WindowManager::leftovers(&HashMap::new()).is_empty());
    }

    #[test]
    fn a_single_client_is_its_own_only_leftover() {
        assert_eq!(WindowManager::leftovers(&clients(&[0x42])), vec![0x42]);
    }
}
