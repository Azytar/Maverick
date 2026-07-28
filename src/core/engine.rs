use crate::config::Cfg;
use crate::core::effect::Effect;
use crate::types::*;

pub struct Engine {
    pub state: State,
    pub cfg: Cfg,
}

impl Engine {
    pub fn new(cfg: Cfg) -> Self {
        Self {
            state: State::new(),
            cfg,
        }
    }

    /// The single owner of domain actions. Mutates `State` and returns the
    /// semantic `Effect`s the backend must carry out. This is the live path:
    /// the backend's `do_action` delegates every keybind/IPC action here.
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let mut cmds = Vec::new();
        match action {
            Action::ToggleBar => {
                let mi = self.state.sel_mon;
                if mi < self.state.monitors.len() {
                    self.state.monitors[mi].show_bar ^= true;
                    let bh = self.cfg.bar_height;
                    self.state.monitors[mi].recalc_workarea(bh);
                    cmds.push(Effect::SyncBarVisibility(mi));
                    cmds.push(Effect::ArrangeMonitor(mi));
                    cmds.push(Effect::UpdateBar(mi));
                }
            }
            Action::CycleLayout => {
                let mi = self.state.sel_mon;
                if mi < self.state.monitors.len() {
                    let ws_i = self.state.monitors[mi].active_ws;
                    self.state.monitors[mi].workspaces[ws_i].cycle_layout();
                    cmds.push(Effect::ArrangeMonitor(mi));
                    cmds.push(Effect::UpdateBar(mi));
                }
            }
            Action::SetLayout(lk) => {
                let mi = self.state.sel_mon;
                if mi < self.state.monitors.len() {
                    let ws_i = self.state.monitors[mi].active_ws;
                    self.state.monitors[mi].workspaces[ws_i].layout = lk;
                    cmds.push(Effect::ArrangeMonitor(mi));
                    cmds.push(Effect::UpdateBar(mi));
                }
            }
            Action::FocusDir(dir) => self.focus_dir(dir, &mut cmds),
            Action::MoveDir(dir) => self.move_dir(dir, &mut cmds),
            Action::View(ws_idx) => self.view_ws(ws_idx, &mut cmds),
            Action::MoveToWs(ws_idx) => self.move_to_ws(ws_idx, &mut cmds),
            Action::GrowCol(px) => self.grow_col(px, &mut cmds),
            Action::NewColumn => self.new_column(&mut cmds),
            Action::CollapseColumn => self.collapse_col(&mut cmds),
            Action::FocusMon(dir) => self.focus_mon(dir, &mut cmds),
            Action::MoveMon(dir) => self.move_mon(dir, &mut cmds),
            Action::Kill => {
                let mi = self.state.sel_mon;
                if let Some(w) = self.state.monitors.get(mi).and_then(|m| m.focused) {
                    cmds.push(Effect::KillWindow(w));
                }
            }
            Action::Spawn(cmd) => cmds.push(Effect::Spawn(cmd)),
            Action::Quit => cmds.push(Effect::Quit),
            Action::Restart => cmds.push(Effect::Restart),
            Action::ToggleFloat => self.toggle_float(&mut cmds),
            Action::ToggleFullscreen => {
                let mi = self.state.sel_mon;
                if let Some(win) = self.state.monitors.get(mi).and_then(|m| m.focused) {
                    let on = !self
                        .state
                        .clients
                        .get(&win)
                        .is_some_and(super::super::types::Client::is_fullscreen);
                    cmds.push(Effect::SetFullscreen { win, on });
                }
            }
        }
        // Always publish the state snapshot after any mutation so IPC
        // subscribers (e.g. bars, maverickctl state) stay in sync.
        if !cmds.is_empty() {
            cmds.push(Effect::PublishIpcState);
        }
        cmds
    }

    /// Move the focused window within its column/columns (niri-style).
    fn move_dir(&mut self, dir: Dir, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;
        let focused = match self.state.monitors[mi].focused {
            Some(w) => w,
            None => return,
        };
        if !self.state.apply_move_dir(dir) {
            return; // float, boundary no-op, etc.
        }
        let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
        self.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(Some(focused)));
    }

    /// Grow/shrink the focused column by `px` (niri-style: only this column).
    fn grow_col(&mut self, px: i32, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;
        let ci = self.state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let workarea_w = self.state.monitors[mi].workarea.w;
        if self.state.monitors[mi].workspaces[ws_i].columns.is_empty() {
            return;
        }
        let min_col = 100u32;
        let max_w = workarea_w;
        if let Some(col) = self.state.monitors[mi].workspaces[ws_i].columns.get_mut(ci) {
            let old_w = col.width.min(i32::MAX as u32) as i32;
            let new_w = (old_w.saturating_add(px))
                .max(min_col as i32)
                .min(max_w as i32) as u32;
            col.width = new_w;
        }
        let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
        self.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
    }

    /// Extract the focused window into a new column to its right.
    fn new_column(&mut self, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;
        let win = match self.state.monitors[mi].focused {
            Some(w) => w,
            None => return,
        };
        if self.state.clients.get(&win).is_none_or(Client::is_float) {
            return;
        }
        let workarea_w = self.state.monitors[mi].workarea.w;
        let dw = (workarea_w as f32 * 0.75) as u32;
        let ci = self.state.monitors[mi].workspaces[ws_i].focus.column_idx;
        self.state.monitors[mi].workspaces[ws_i].remove_window(win);
        let ins_pos = (ci + 1).min(self.state.monitors[mi].workspaces[ws_i].columns.len());
        let mut new_col = Column::new(dw);
        new_col.windows.push(win);
        self.state.monitors[mi].workspaces[ws_i]
            .columns
            .insert(ins_pos, new_col);
        self.state.monitors[mi].workspaces[ws_i].focus.column_idx = ins_pos;
        let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
        self.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(Some(win)));
    }

    /// Merge the focused column into the previous one.
    fn collapse_col(&mut self, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;
        let ci = self.state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let n_cols = self.state.monitors[mi].workspaces[ws_i].columns.len();
        if n_cols < 2 || ci == 0 || ci >= n_cols {
            return;
        }
        let target = ci - 1;
        {
            let ws = &mut self.state.monitors[mi].workspaces[ws_i];
            let wins: Vec<WindowId> = std::mem::take(&mut ws.columns[ci].windows);
            ws.columns[target].windows.extend(wins);
            ws.columns.retain(|c| !c.windows.is_empty());
            ws.focus.column_idx = target.min(ws.columns.len().saturating_sub(1));
            if let Some(col) = ws.columns.get(ws.focus.column_idx) {
                ws.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
            }
        }
        // Compute ideal scroll AFTER collapsing (not before) so it reflects
        // the new column count and positions.
        let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
        self.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
    }

    /// Move focus to the next/previous/left/right monitor.
    fn focus_mon(&mut self, dir: Dir, cmds: &mut Vec<Effect>) {
        let n = self.state.monitors.len();
        if n <= 1 {
            return;
        }
        let cur = self.state.sel_mon;
        let new = match dir {
            Dir::Left | Dir::Prev => (cur + n - 1) % n,
            _ => (cur + 1) % n,
        };
        if let Some(fw) = self.state.monitors[cur].focused {
            cmds.push(Effect::Unfocus(fw));
        }
        self.state.sel_mon = new;
        cmds.push(Effect::FocusWindow(self.state.best_focus(new)));
    }

    /// Move the focused window to the next/previous/left/right monitor.
    fn move_mon(&mut self, dir: Dir, cmds: &mut Vec<Effect>) {
        let n = self.state.monitors.len();
        if n <= 1 {
            return;
        }
        let mi = self.state.sel_mon;
        let win = match self.state.monitors.get(mi).and_then(|m| m.focused) {
            Some(w) => w,
            None => return,
        };
        let new_mi = match dir {
            Dir::Left | Dir::Prev => (mi + n - 1) % n,
            _ => (mi + 1) % n,
        };
        let src_ws = self.state.clients.get(&win).map_or(0, |c| c.workspace);
        let is_float = self
            .state
            .clients
            .get(&win)
            .is_some_and(super::super::types::Client::is_float);
        let dw = self.cfg.default_col_w;

        self.state.monitors[mi].workspaces[src_ws].remove_window(win);
        if is_float {
            self.state.monitors[new_mi].workspaces[src_ws]
                .floats
                .push(win);
        } else {
            let workarea_w = self.state.monitors[new_mi].workarea.w;
            self.state.monitors[new_mi].workspaces[src_ws].add_tiled(win, dw, workarea_w);
        }
        self.state.monitors[mi].focus_stack.retain(|&w| w != win);
        if self.state.monitors[mi].focused == Some(win) {
            self.state.monitors[mi].focused = self.state.monitors[mi].focus_stack.last().copied();
        }
        self.state.monitors[new_mi].focus_stack.push(win);
        if let Some(c) = self.state.clients.get_mut(&win) {
            c.monitor = new_mi;
            c.workspace = src_ws;
        }
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::ArrangeMonitor(new_mi));
        self.state.sel_mon = new_mi;
        cmds.push(Effect::FocusWindow(Some(win)));
    }

    /// Switch the selected monitor to workspace `ws_idx`.
    fn view_ws(&mut self, ws_idx: usize, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        let mon = match self.state.monitors.get(mi) {
            Some(m) => m,
            None => return,
        };
        if ws_idx >= mon.workspaces.len() || ws_idx == mon.active_ws {
            return;
        }
        self.state.monitors[mi].active_ws = ws_idx;
        let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
        self.state.monitors[mi].workspaces[ws_idx].scroll = scroll;

        cmds.push(Effect::SetCurrentDesktop(ws_idx));
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(self.state.best_focus(mi)));
        cmds.push(Effect::UpdateBar(mi));
    }

    /// Move the focused window to workspace `ws_idx` (same monitor).
    fn move_to_ws(&mut self, ws_idx: usize, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        let win = match self.state.monitors.get(mi).and_then(|m| m.focused) {
            Some(w) => w,
            None => return,
        };
        let src_ws = match self.state.clients.get(&win) {
            Some(c) => c.workspace,
            None => return,
        };
        if src_ws == ws_idx || ws_idx >= self.state.monitors[mi].workspaces.len() {
            return;
        }
        let is_float = self
            .state
            .clients
            .get(&win)
            .is_some_and(super::super::types::Client::is_float);
        let dw = self.cfg.default_col_w;

        self.state.monitors[mi].workspaces[src_ws].remove_window(win);
        self.state.monitors[mi].focus_stack.retain(|&w| w != win);
        if self.state.monitors[mi].focused == Some(win) {
            self.state.monitors[mi].focused = self.state.monitors[mi].focus_stack.last().copied();
        }

        if is_float {
            self.state.monitors[mi].workspaces[ws_idx].floats.push(win);
        } else {
            let workarea_w = self.state.monitors[mi].workarea.w;
            self.state.monitors[mi].workspaces[ws_idx].add_tiled(win, dw, workarea_w);
        }
        if let Some(c) = self.state.clients.get_mut(&win) {
            c.workspace = ws_idx;
        }

        cmds.push(Effect::SetWindowDesktop { win, ws: ws_idx });
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(self.state.best_focus(mi)));
        cmds.push(Effect::UpdateBar(mi));
    }

    /// Toggle floating for the focused window. Pure state move between the
    /// tiled column structure and the workspace's float list.
    fn toggle_float(&mut self, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;
        let win = match self.state.monitors[mi].focused {
            Some(w) => w,
            None => return,
        };
        let is_float = self
            .state
            .clients
            .get(&win)
            .is_some_and(super::super::types::Client::is_float);

        if ws_i >= self.state.monitors[mi].workspaces.len() {
            return;
        }

        if is_float {
            let dw = self.cfg.default_col_w;
            let workarea_w = self.state.monitors[mi].workarea.w;
            self.state.monitors[mi].workspaces[ws_i].remove_window(win);
            self.state.monitors[mi].workspaces[ws_i].add_tiled(win, dw, workarea_w);
            if let Some(c) = self.state.clients.get_mut(&win) {
                c.flags.clear(WinFlags::FLOAT);
            }
        } else {
            self.state.monitors[mi].workspaces[ws_i].remove_window(win);
            self.state.monitors[mi].workspaces[ws_i].floats.push(win);
            if let Some(c) = self.state.clients.get_mut(&win) {
                c.flags.set(WinFlags::FLOAT);
            }
        }

        cmds.push(Effect::MarkRestack(mi));
        cmds.push(Effect::ArrangeMonitor(mi));
    }

    /// Move focus in `dir` within the selected monitor. Pure domain decision:
    /// updates focus indices + scroll, then emits arrange + focus effects. The
    /// backend performs the actual X11 focus plumbing via `Effect::FocusWindow`.
    fn focus_dir(&mut self, dir: Dir, cmds: &mut Vec<Effect>) {
        let mi = self.state.sel_mon;
        if mi >= self.state.monitors.len() {
            return;
        }
        let ws_i = self.state.monitors[mi].active_ws;

        let target: Option<WindowId> = match dir {
            Dir::Left | Dir::Right => {
                let ws = &self.state.monitors[mi].workspaces[ws_i];
                let n = ws.columns.len();
                if n == 0 {
                    return;
                }
                let new_ci = if dir == Dir::Left {
                    (ws.focus.column_idx + n - 1) % n
                } else {
                    (ws.focus.column_idx + 1) % n
                };
                self.state.monitors[mi].workspaces[ws_i].focus.column_idx = new_ci;
                let win = self.state.monitors[mi].workspaces[ws_i].columns[new_ci].focused_win();
                let scroll = crate::core::layout::ideal_scroll(&self.state.monitors[mi], &self.cfg);
                self.state.monitors[mi].workspaces[ws_i].scroll = scroll;
                win
            }
            Dir::Up | Dir::Down => {
                let ws = &self.state.monitors[mi].workspaces[ws_i];
                let ci = ws.focus.column_idx;
                if ci >= ws.columns.len() {
                    return;
                }
                let col = &ws.columns[ci];
                let n = col.windows.len();
                if n == 0 {
                    return;
                }
                let new_ri = if dir == Dir::Up {
                    (col.focused + n - 1) % n
                } else {
                    (col.focused + 1) % n
                };
                self.state.monitors[mi].workspaces[ws_i].columns[ci].focused = new_ri;
                Some(self.state.monitors[mi].workspaces[ws_i].columns[ci].windows[new_ri])
            }
            Dir::Next | Dir::Prev => {
                let focused = self.state.monitors[mi].focused;
                let stack = &self.state.monitors[mi].focus_stack;
                if stack.is_empty() {
                    return;
                }
                match focused {
                    Some(fw) => match stack.iter().position(|&w| w == fw) {
                        Some(pos) => {
                            let n = stack.len();
                            let ni = if dir == Dir::Next {
                                (pos + 1) % n
                            } else {
                                (pos + n - 1) % n
                            };
                            Some(stack[ni])
                        }
                        None => return, // focused not in stack -> stale
                    },
                    None => Some(stack[0]),
                }
            }
        };

        // Next/Prev only cycle the focus stack (no layout change); the others
        // also re-arrange because scroll / focus indices moved.
        if !matches!(dir, Dir::Next | Dir::Prev) {
            cmds.push(Effect::ArrangeMonitor(mi));
        }
        cmds.push(Effect::FocusWindow(target));
    }
}
