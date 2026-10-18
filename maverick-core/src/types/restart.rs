//! One-shot logical layout handoff between two images of the same WM process.
//! Client properties and output reservations are read afresh by the backend.

use super::*;

/// Below the base X11 request limit, including the property request header.
pub const MAX_WORDS: usize = 60_000;

pub struct RestartState(State);

impl RestartState {
    pub fn encode(state: &State) -> Option<Vec<u32>> {
        let mut out = vec![1, state.sel_mon as u32, state.monitors.len() as u32];
        for mon in &state.monitors {
            put_rect(&mut out, mon.screen);
            out.extend([
                mon.carousel.current?.get(),
                mon.carousel.origin?.get(),
                mon.carousel.next_id,
                mon.focused.unwrap_or(0),
            ]);
            put_list(&mut out, &mon.focus_stack);
            out.push(mon.workspaces.len() as u32);
            for ws in &mon.workspaces {
                out.extend([
                    ws.id.get(),
                    ws.focus.column_idx as u32,
                    ws.camera.position.to_bits(),
                    u32::from(ws.viewport_mode == ViewportMode::Zoomed),
                    ws.page_zoom.to_bits(),
                    ws.presented_maximize.unwrap_or(0),
                    ws.columns.len() as u32,
                ]);
                for col in &ws.columns {
                    out.extend([col.weight.to_bits(), col.focused as u32]);
                    put_list(&mut out, &col.windows);
                }
                put_list(&mut out, &ws.floats);
                out.extend([
                    u32::from(ws.overview),
                    ws.overview_scale.to_bits(),
                    ws.overview_rects.len() as u32,
                ]);
                let mut rects: Vec<_> = ws.overview_rects.iter().collect();
                rects.sort_unstable_by_key(|(win, _)| **win);
                for (&win, &(rect, border)) in rects {
                    out.push(win);
                    put_rect(&mut out, rect);
                    out.push(border);
                }
            }
        }
        out.push(state.clients.len() as u32);
        // Stable ordering makes diagnostics and round trips comparable.
        let mut wins: Vec<_> = state.clients.keys().copied().collect();
        wins.sort_unstable();
        for win in wins {
            let c = &state.clients[&win];
            out.extend([
                win,
                c.monitor as u32,
                c.workspace.get(),
                c.flags.0 as u32,
                c.border_w,
                c.old_border_w,
                c.fullscreen_policy as u32,
                u32::from(c.float_client_authority),
                c.opacity.map(f32::to_bits).unwrap_or(u32::MAX),
            ]);
            put_rect(&mut out, c.geom);
            put_rect(&mut out, c.saved_geom);
            out.push(u32::from(c.fs_snapshot.is_some()));
            if let Some(fs) = c.fs_snapshot {
                out.extend([fs.prior as u32, fs.policy as u32]);
                put_rect(&mut out, fs.rect);
            }
        }
        (out.len() < MAX_WORDS).then_some(out)
    }

    pub fn decode(words: &[u32]) -> Option<Self> {
        if words.len() >= MAX_WORDS {
            return None;
        }
        let mut r = Reader(words);
        if r.word()? != 1 {
            return None;
        }
        let mut state = State::new();
        state.sel_mon = r.word()? as usize;
        for _ in 0..r.count()? {
            let mut mon = Monitor::new(r.rect()?, 1);
            mon.carousel = Carousel {
                current: Some(ViewId::new(r.word()?)),
                origin: Some(ViewId::new(r.word()?)),
                next_id: r.word()?,
            };
            mon.focused = r.window()?;
            mon.focus_stack = r.list()?;
            mon.workspaces.clear();
            for _ in 0..r.count()? {
                let mut ws = Workspace::new(ViewId::new(r.word()?));
                ws.focus.column_idx = r.word()? as usize;
                ws.camera = Camera::new(r.float()?);
                ws.viewport_mode = if r.boolean()? {
                    ViewportMode::Zoomed
                } else {
                    ViewportMode::Normal
                };
                ws.page_zoom = r.float()?;
                if ws.page_zoom < 1.0 {
                    return None;
                }
                ws.presented_maximize = r.window()?;
                for _ in 0..r.count()? {
                    ws.columns.push(Column {
                        weight: r.float()?,
                        focused: r.word()? as usize,
                        windows: r.list()?,
                    });
                }
                ws.floats = r.list()?;
                ws.overview = r.boolean()?;
                ws.overview_scale = r.float()?;
                if ws.overview_scale <= 0.0 || ws.overview_scale > 1.0 {
                    return None;
                }
                for _ in 0..r.count()? {
                    let win = r.word()?;
                    if ws
                        .overview_rects
                        .insert(win, (r.rect()?, r.word()?))
                        .is_some()
                    {
                        return None;
                    }
                }
                mon.workspaces.push(ws);
            }
            if mon.workspaces.is_empty()
                || mon
                    .workspaces
                    .iter()
                    .any(|ws| ws.id.get() >= mon.carousel.next_id)
            {
                return None;
            }
            state.monitors.push(mon);
        }
        if state.monitors.is_empty() || state.sel_mon >= state.monitors.len() {
            return None;
        }
        for _ in 0..r.count()? {
            let win = r.word()?;
            let mut c = Client::new(win, r.word()? as usize, ViewId::new(r.word()?));
            c.flags = WinFlags(u16::try_from(r.word()?).ok()?);
            c.border_w = r.word()?;
            c.old_border_w = r.word()?;
            c.fullscreen_policy = r.policy()?;
            c.float_client_authority = r.boolean()?;
            let opacity = r.word()?;
            if opacity != u32::MAX {
                let opacity = f32::from_bits(opacity);
                if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
                    return None;
                }
                c.opacity = Some(opacity);
            }
            c.geom = r.rect()?;
            c.saved_geom = r.rect()?;
            if r.boolean()? {
                let prior = match r.word()? {
                    0 => WindowMode::Float,
                    1 => WindowMode::Tiled,
                    2 => WindowMode::Maximized,
                    _ => return None,
                };
                c.fs_snapshot = Some(FullscreenSnapshot {
                    prior,
                    policy: r.policy()?,
                    rect: r.rect()?,
                });
            }
            if win == 0 || state.clients.insert(win, c).is_some() {
                return None;
            }
        }
        if !r.0.is_empty() || state.check_invariants().is_err() {
            return None;
        }
        Some(Self(state))
    }

    pub fn compatible(&self, monitors: &[Monitor]) -> bool {
        self.0.monitors.len() == monitors.len()
            && self
                .0
                .monitors
                .iter()
                .zip(monitors)
                .all(|(a, b)| a.screen == b.screen)
    }

    /// Restore only WM-owned fields; keep fresh hints, class, protocols and PID.
    pub fn restore_client(&self, client: &mut Client) -> bool {
        let Some(saved) = self.0.clients.get(&client.window) else {
            return false;
        };
        client.geom = saved.geom;
        client.saved_geom = saved.saved_geom;
        client.flags = saved.flags;
        client.border_w = saved.border_w;
        client.old_border_w = saved.old_border_w;
        client.fullscreen_policy = saved.fullscreen_policy;
        client.fs_snapshot = saved.fs_snapshot;
        client.float_client_authority = saved.float_client_authority;
        client.opacity = saved.opacity;
        true
    }

    pub fn contains(&self, win: WindowId) -> bool {
        self.0.clients.contains_key(&win)
    }

    /// Reconcile the handoff with clients which survived the exec. New clients
    /// join the selected View; clients which closed during restart are dropped.
    pub fn apply(mut self, state: &mut State, column_width: f32) -> bool {
        let changed = state.clients.len() != self.0.clients.len()
            || state.clients.keys().any(|win| !self.contains(*win));
        let mut placed = std::collections::HashSet::new();
        for (mi, mon) in self.0.monitors.iter_mut().enumerate() {
            // Docks may have changed while the process image was replaced.
            mon.reserved_regions = state.monitors[mi].reserved_regions.clone();
            mon.recalc_geometry();
            for ws in &mut mon.workspaces {
                let focused = ws.focused_win();
                for col in &mut ws.columns {
                    let selected = col.focused_win();
                    col.windows.retain(|w| state.clients.contains_key(w));
                    col.focused = selected
                        .and_then(|w| col.windows.iter().position(|&id| id == w))
                        .unwrap_or(0);
                }
                ws.columns.retain(|col| !col.windows.is_empty());
                ws.focus.column_idx = focused
                    .and_then(|w| ws.index_of_window(w).map(|(ci, _)| ci))
                    .unwrap_or_else(|| ws.focus.column_idx.min(ws.columns.len().saturating_sub(1)));
                ws.floats.retain(|w| state.clients.contains_key(w));
                ws.overview_rects
                    .retain(|w, _| state.clients.contains_key(w));
                ws.presented_maximize = ws
                    .presented_maximize
                    .filter(|w| state.clients.contains_key(w));
                for win in ws.columns.iter().flat_map(|c| &c.windows).chain(&ws.floats) {
                    placed.insert(*win);
                    let client = state.clients.get_mut(win).expect("retained client");
                    client.monitor = mi;
                    client.workspace = ws.id;
                }
            }
            mon.focus_stack.retain(|w| placed.contains(w));
            mon.focused = mon.focused.filter(|w| placed.contains(w));
            if mon.focused.is_none() {
                mon.focused = mon
                    .ws()
                    .focused_win()
                    .or_else(|| mon.ws().floats.last().copied());
            }
        }
        state.monitors = self.0.monitors;
        state.sel_mon = self.0.sel_mon;
        for client in state
            .clients
            .values_mut()
            .filter(|c| !placed.contains(&c.window))
        {
            client.monitor = state.sel_mon;
            let mon = &mut state.monitors[state.sel_mon];
            client.workspace = mon.ws().id;
            if client.is_float() {
                mon.ws_mut().floats.push(client.window);
            } else {
                mon.ws_mut().add_tiled(client.window, column_width);
            }
        }
        state.pending_focus = None;
        state.pending_transients.clear();
        for mi in 0..state.monitors.len() {
            let mon = &mut state.monitors[mi];
            if mon.focused.is_none() {
                mon.focused = mon
                    .ws()
                    .focused_win()
                    .or_else(|| mon.ws().floats.last().copied());
            }
            state.sync_presented_maximize(mi);
        }
        changed
    }
}

fn put_rect(out: &mut Vec<u32>, r: Rect) {
    out.extend([r.x as u32, r.y as u32, r.w, r.h]);
}
fn put_list(out: &mut Vec<u32>, items: &[u32]) {
    out.push(items.len() as u32);
    out.extend(items);
}

struct Reader<'a>(&'a [u32]);
impl Reader<'_> {
    fn word(&mut self) -> Option<u32> {
        let (&word, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(word)
    }
    fn count(&mut self) -> Option<usize> {
        let n = self.word()? as usize;
        (n <= self.0.len()).then_some(n)
    }
    fn list(&mut self) -> Option<Vec<u32>> {
        let n = self.count()?;
        let (list, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(list.to_vec())
    }
    fn window(&mut self) -> Option<Option<WindowId>> {
        Some(match self.word()? {
            0 => None,
            w => Some(w),
        })
    }
    fn boolean(&mut self) -> Option<bool> {
        match self.word()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn float(&mut self) -> Option<f32> {
        let f = f32::from_bits(self.word()?);
        f.is_finite().then_some(f)
    }
    fn rect(&mut self) -> Option<Rect> {
        let r = Rect::new(
            self.word()? as i32,
            self.word()? as i32,
            self.word()?,
            self.word()?,
        );
        (r.w > 0 && r.h > 0).then_some(r)
    }
    fn policy(&mut self) -> Option<FullscreenPolicy> {
        match self.word()? {
            0 => Some(FullscreenPolicy::Normal),
            1 => Some(FullscreenPolicy::Deny),
            2 => Some(FullscreenPolicy::True),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> State {
        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 3);
        mon.remove_view_at(1);
        mon.goto_view(mon.workspaces[1].id);
        for win in 1..=3 {
            let view = mon.workspaces[usize::from(win == 3)].id;
            let mut client = Client::new(win, 0, view);
            client.geom = Rect::new(120, 40, 320, 240);
            client.saved_geom = client.geom;
            if win == 3 {
                client.flags.set(WinFlags::FLOAT | WinFlags::MAXIMIZED);
                client.opacity = Some(0.8);
                mon.workspaces[1].floats.push(win);
                mon.workspaces[1].presented_maximize = Some(win);
            } else {
                client.flags.set(WinFlags::FULLSCREEN);
                client.fullscreen_policy = FullscreenPolicy::True;
                client.fs_snapshot = Some(FullscreenSnapshot {
                    prior: if win == 1 {
                        WindowMode::Maximized
                    } else {
                        WindowMode::Tiled
                    },
                    rect: client.geom,
                    policy: FullscreenPolicy::Normal,
                });
                let mut column = Column::new(0.3 + win as f32 * 0.1);
                column.windows.push(win);
                mon.workspaces[0].columns.push(column);
            }
            state.clients.insert(win, client);
        }
        mon.workspaces[0].camera.snap(73.25);
        mon.focused = Some(3);
        mon.focus_stack = vec![1, 2, 3];
        state.monitors.push(mon);
        state
            .monitors
            .push(Monitor::new(Rect::new(800, 0, 800, 600), 2));
        let mon = &mut state.monitors[1];
        let mut column = Column::new(0.77);
        column.windows = vec![4, 5];
        column.focused = 1;
        mon.ws_mut().columns.push(column);
        mon.focused = Some(5);
        mon.focus_stack = vec![4, 5];
        for win in [4, 5] {
            let mut client = Client::new(win, 1, mon.ws().id);
            client.geom = Rect::new(800, 0, 300, 300);
            client.saved_geom = client.geom;
            state.clients.insert(win, client);
        }
        mon.ws_mut().overview = true;
        mon.ws_mut().overview_scale = 0.76;
        for win in [4, 5] {
            mon.ws_mut()
                .overview_rects
                .insert(win, (state.clients[&win].geom, 2));
        }
        state.check_invariants().unwrap();
        state
    }

    fn adopted(snapshot: &RestartState, original: &State) -> State {
        let mut fresh = State::new();
        fresh.monitors = original
            .monitors
            .iter()
            .map(|m| Monitor::new(m.screen, 9))
            .collect();
        for &win in original.clients.keys() {
            let mut client = Client::new(win, 0, fresh.monitors[0].ws().id);
            client.name = "fresh X11 title".into();
            client.pid = Some(42);
            snapshot.restore_client(&mut client);
            fresh.clients.insert(win, client);
        }
        fresh
    }

    #[test]
    fn handoff_preserves_layout_and_transition_state_without_stale_client_metadata() {
        let original = layout();
        let words = RestartState::encode(&original).unwrap();
        let snapshot = RestartState::decode(&words).unwrap();
        let mut fresh = adopted(&snapshot, &original);
        assert!(snapshot.compatible(&fresh.monitors));
        assert!(!snapshot.apply(&mut fresh, 0.6));
        fresh.check_invariants().unwrap();
        assert_eq!(RestartState::encode(&fresh).unwrap(), words);
        assert_eq!(
            fresh.clients[&1].fs_snapshot,
            original.clients[&1].fs_snapshot
        );
        assert_eq!(fresh.clients[&3].name, "fresh X11 title");
        assert_eq!(fresh.clients[&3].pid, Some(42));
        assert_eq!(fresh.monitors[0].create_view().get(), 3);
    }

    #[test]
    fn handoff_reconciles_clients_that_close_or_open_during_exec() {
        let original = layout();
        let snapshot = RestartState::decode(&RestartState::encode(&original).unwrap()).unwrap();
        let mut fresh = adopted(&snapshot, &original);
        fresh.clients.remove(&3);
        fresh.clients.remove(&1);
        let mut new = Client::new(10, 0, fresh.monitors[0].ws().id);
        new.geom = Rect::new(0, 0, 200, 200);
        new.saved_geom = new.geom;
        fresh.clients.insert(10, new);
        assert!(snapshot.apply(&mut fresh, 0.6));
        fresh.check_invariants().unwrap();
        assert_eq!(fresh.monitors[0].workspaces[0].columns[0].windows, [2]);
        assert_eq!(fresh.monitors[0].ws().columns[0].windows, [10]);
        assert_eq!(fresh.monitors[0].focused, Some(10));
        assert_eq!(fresh.clients[&10].workspace, fresh.monitors[0].ws().id);
    }

    #[test]
    fn malformed_handoffs_are_rejected_before_state_is_applied() {
        let mut original = layout();
        let words = RestartState::encode(&original).unwrap();
        for length in 0..words.len() {
            assert!(RestartState::decode(&words[..length]).is_none());
        }
        let mut trailing = words.clone();
        trailing.push(0);
        assert!(RestartState::decode(&trailing).is_none());
        assert!(RestartState::decode(&vec![0; MAX_WORDS]).is_none());
        original.monitors[0].workspaces[0].camera.position = f32::NAN;
        assert!(RestartState::decode(&RestartState::encode(&original).unwrap()).is_none());
        original.monitors[0].workspaces[0].camera.snap(0.0);
        original.monitors[0].workspaces[0].columns[0]
            .windows
            .push(2);
        assert!(RestartState::decode(&RestartState::encode(&original).unwrap()).is_none());
    }
}
