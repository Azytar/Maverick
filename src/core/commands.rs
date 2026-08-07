//! Sistema de comandos tipado para Maverick.
//!
//! Cada comando es una transformación **pura** sobre `State`/`Cfg` que:
//! - produce los `Effect` que el backend debe ejecutar, y
//! - declara el **evento de dominio** que representa (ver `core::event`).
//!
//! El comando conoce SU evento, no a sus consumidores. Quien quiera reaccionar
//! (renderer, IPC, barras, hooks, tests) se suscribe al `EventBus` del Engine.

use crate::config::Cfg;
use crate::core::effect::Effect;
use crate::core::event::{CommandReport, Event};
use crate::types::{Column, Dir, LayoutKind, State, WindowId, WinFlags};

// ─── Trait Command ───────────────────────────────────────────────────────────

pub trait Command {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport;
}

// ─── Focus ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct FocusWindow(pub Option<WindowId>);

impl Command for FocusWindow {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let before = state.sel_mon;
        let from = state
            .monitors
            .get(before)
            .and_then(|m| m.focused);
        if let Some(win) = self.0 {
            if before < state.monitors.len() {
                return CommandReport::with_event(
                    vec![Effect::FocusWindow(Some(win))],
                    Event::FocusChanged {
                        from,
                        to: Some(win),
                    },
                );
            }
        }
        CommandReport::new(vec![])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FocusDirection(pub Dir);

impl Command for FocusDirection {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let from = state.monitors[mi].focused;
        let ws_i = state.monitors[mi].active_ws;
        let target: Option<WindowId> = match self.0 {
            Dir::Left | Dir::Right => {
                let ws = &state.monitors[mi].workspaces[ws_i];
                let n = ws.columns.len();
                if n == 0 { return CommandReport::new(cmds); }
                let new_ci = if self.0 == Dir::Left {
                    (ws.focus.column_idx + n - 1) % n
                } else {
                    (ws.focus.column_idx + 1) % n
                };
                state.monitors[mi].workspaces[ws_i].focus.column_idx = new_ci;
                let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
                state.monitors[mi].workspaces[ws_i].scroll = scroll;
                state.monitors[mi].workspaces[ws_i].columns[new_ci].focused_win()
            }
            Dir::Up | Dir::Down => {
                let ws_i = state.monitors[mi].active_ws;
                let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
                if ci >= state.monitors[mi].workspaces[ws_i].columns.len() { return CommandReport::new(cmds); }
                let n = state.monitors[mi].workspaces[ws_i].columns[ci].windows.len();
                if n == 0 { return CommandReport::new(cmds); }
                let new_ri = if self.0 == Dir::Up {
                    (state.monitors[mi].workspaces[ws_i].columns[ci].focused + n - 1) % n
                } else {
                    (state.monitors[mi].workspaces[ws_i].columns[ci].focused + 1) % n
                };
                state.monitors[mi].workspaces[ws_i].columns[ci].focused = new_ri;
                let target = state.monitors[mi].workspaces[ws_i].columns[ci].windows[new_ri];
                let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
                state.monitors[mi].workspaces[ws_i].scroll = scroll;
                Some(target)
            }
            Dir::Next | Dir::Prev => {
                let ws_i = state.monitors[mi].active_ws;
                let focused = state.monitors[mi].focused;
                let stack = &state.monitors[mi].focus_stack;
                if stack.is_empty() { return CommandReport::new(cmds); }
                let stack: Vec<WindowId> = stack.iter().copied().filter(|&w| {
                    state.clients.get(&w).is_some_and(|c| c.workspace == ws_i)
                }).collect();
                if stack.is_empty() { return CommandReport::new(cmds); }
                let target = match focused {
                    Some(fw) => match stack.iter().position(|&w| w == fw) {
                        Some(pos) => {
                            let n = stack.len();
                            let ni = if self.0 == Dir::Next {
                                (pos + 1) % n
                            } else {
                                (pos + n - 1) % n
                            };
                            stack[ni]
                        }
                        None => stack[0],
                    },
                    None => stack[0],
                };
                state.monitors[mi].focused = Some(target);
                let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
                state.monitors[mi].workspaces[ws_i].scroll = scroll;
                Some(target)
            }
        };
        if let Some(w) = target {
            cmds.push(Effect::Unfocus(from.unwrap_or(0)));
            state.monitors[mi].focused = Some(w);
            cmds.push(Effect::ArrangeMonitor(mi));
            cmds.push(Effect::FocusWindow(Some(w)));
            return CommandReport::with_event(
                cmds,
                Event::FocusChanged { from, to: Some(w) },
            );
        }
        CommandReport::new(cmds)
    }
}

// ─── Move ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct MoveWindow(pub WindowId, pub Dir);

impl Command for MoveWindow {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        if !state.apply_move_dir(self.1) { return CommandReport::new(cmds); }
        let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
        state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(Some(self.0)));
        CommandReport::with_event(cmds, Event::WindowMoved(self.0))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KillWindow(pub WindowId);

impl Command for KillWindow {
    fn execute(&mut self, _state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        CommandReport::with_event(
            vec![Effect::KillWindow(self.0)],
            Event::WindowUnmapped(self.0),
        )
    }
}

// ─── Float / Fullscreen ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ToggleFloat;

impl Command for ToggleFloat {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        let win = match state.monitors[mi].focused { Some(w) => w, None => return CommandReport::new(cmds) };
        let is_float = state.clients.get(&win).is_some_and(crate::types::Client::is_float);
        if ws_i >= state.monitors[mi].workspaces.len() { return CommandReport::new(cmds); }
        if is_float {
            let dw = cfg.default_col_w;
            let workarea_w = state.monitors[mi].workarea.w;
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].add_tiled(win, dw, workarea_w);
            if let Some(c) = state.clients.get_mut(&win) { c.flags.clear(WinFlags::FLOAT); }
        } else {
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].floats.push(win);
            if let Some(c) = state.clients.get_mut(&win) { c.flags.set(WinFlags::FLOAT); }
        }
        cmds.push(Effect::MarkRestack(mi));
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::SyncWindowPrefs(win));
        CommandReport::with_event(cmds, Event::FloatToggled(win))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToggleFullscreen(pub Option<WindowId>);

impl Command for ToggleFullscreen {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if let Some(win) = self.0.or(state.monitors.get(mi).and_then(|m| m.focused)) {
            if let Some(c) = state.clients.get_mut(&win) {
                let on = !c.is_fullscreen();
                if on { c.flags.set(WinFlags::FULLSCREEN); }
                else { c.flags.clear(WinFlags::FULLSCREEN); }
                cmds.push(Effect::SetFullscreen { win, on });
                cmds.push(Effect::ArrangeMonitor(mi));
                return CommandReport::with_event(
                    cmds,
                    Event::FullscreenToggled { win, on },
                );
            }
        }
        CommandReport::new(cmds)
    }
}

// ─── Layout ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct CycleLayout;

impl Command for CycleLayout {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi < state.monitors.len() {
            let ws_i = state.monitors[mi].active_ws;
            state.monitors[mi].workspaces[ws_i].cycle_layout();
            cmds.push(Effect::ArrangeMonitor(mi));
            return CommandReport::with_event(
                cmds,
                Event::LayoutChanged {
                    monitor: mi,
                    workspace: ws_i,
                },
            );
        }
        CommandReport::new(cmds)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SetLayout(pub LayoutKind);

impl Command for SetLayout {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi < state.monitors.len() {
            let ws_i = state.monitors[mi].active_ws;
            state.monitors[mi].workspaces[ws_i].layout = self.0;
            cmds.push(Effect::ArrangeMonitor(mi));
            return CommandReport::with_event(
                cmds,
                Event::LayoutChanged {
                    monitor: mi,
                    workspace: ws_i,
                },
            );
        }
        CommandReport::new(cmds)
    }
}

// ─── Workspace ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ViewWorkspace(pub usize);

impl Command for ViewWorkspace {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        let mon = match state.monitors.get(mi) { Some(m) => m, None => return CommandReport::new(cmds) };
        let from = mon.active_ws;
        let ws_idx = self.0;
        if ws_idx >= mon.workspaces.len() || ws_idx == mon.active_ws { return CommandReport::new(cmds); }
        state.monitors[mi].active_ws = ws_idx;
        let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
        state.monitors[mi].workspaces[ws_idx].scroll = scroll;
        cmds.push(Effect::SetCurrentDesktop(ws_idx));
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(state.best_focus(mi)));
        CommandReport::with_event(
            cmds,
            Event::WorkspaceChanged {
                monitor: mi,
                from,
                to: ws_idx,
            },
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MoveToWorkspace(pub usize);

impl Command for MoveToWorkspace {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        let win = match state.monitors.get(mi).and_then(|m| m.focused) { Some(w) => w, None => return CommandReport::new(cmds) };
        let src_ws = match state.clients.get(&win) { Some(c) => c.workspace, None => return CommandReport::new(cmds) };
        let ws_idx = self.0;
        if src_ws == ws_idx || ws_idx >= state.monitors[mi].workspaces.len() { return CommandReport::new(cmds); }
        let is_float = state.clients.get(&win).is_some_and(crate::types::Client::is_float);
        let dw = cfg.default_col_w;
        state.monitors[mi].workspaces[src_ws].remove_window(win);
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        if is_float {
            state.monitors[mi].workspaces[ws_idx].floats.push(win);
        } else {
            let workarea_w = state.monitors[mi].workarea.w;
            state.monitors[mi].workspaces[ws_idx].add_tiled(win, dw, workarea_w);
        }
        if let Some(c) = state.clients.get_mut(&win) { c.workspace = ws_idx; }
        cmds.push(Effect::SetWindowDesktop { win, ws: ws_idx });
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(state.best_focus(mi)));
        CommandReport::with_event(cmds, Event::WindowMoved(win))
    }
}
// ─── Column ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct GrowColumn(pub i32);

impl Command for GrowColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        if state.monitors[mi].workspaces[ws_i].columns.is_empty() { return CommandReport::new(cmds); }
        let min_col = 100u32;
        let max_w = state.monitors[mi].workarea.w;
        if let Some(col) = state.monitors[mi].workspaces[ws_i].columns.get_mut(ci) {
            let old_w = col.width.min(i32::MAX as u32) as i32;
            let new_w = (old_w.saturating_add(self.0)).max(min_col as i32).min(max_w as i32) as u32;
            col.width = new_w;
        }
        let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
        state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::new(cmds)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NewColumn;

impl Command for NewColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        let win = match state.monitors[mi].focused { Some(w) => w, None => return CommandReport::new(cmds) };
        if state.clients.get(&win).is_none_or(crate::types::Client::is_float) { return CommandReport::new(cmds); }
        let workarea_w = state.monitors[mi].workarea.w;
        let dw = (workarea_w as f32 * 0.75) as u32;
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        state.monitors[mi].workspaces[ws_i].remove_window(win);
        let ins_pos = (ci + 1).min(state.monitors[mi].workspaces[ws_i].columns.len());
        let mut new_col = Column::new(dw);
        new_col.windows.push(win);
        state.monitors[mi].workspaces[ws_i].columns.insert(ins_pos, new_col);
        state.monitors[mi].workspaces[ws_i].focus.column_idx = ins_pos;
        let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
        state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(Some(win)));
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged { monitor: mi, workspace: ws_i },
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CollapseColumn;

impl Command for CollapseColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let n_cols = state.monitors[mi].workspaces[ws_i].columns.len();
        if n_cols < 2 || ci == 0 || ci >= n_cols { return CommandReport::new(cmds); }
        let target = ci - 1;
        {
            let ws = &mut state.monitors[mi].workspaces[ws_i];
            let wins: Vec<WindowId> = std::mem::take(&mut ws.columns[ci].windows);
            ws.columns[target].windows.extend(wins);
            ws.columns.retain(|c| !c.windows.is_empty());
            ws.focus.column_idx = target.min(ws.columns.len().saturating_sub(1));
        }
        let scroll = crate::core::layout::ideal_scroll(&state.monitors[mi], cfg);
        state.monitors[mi].workspaces[ws_i].scroll = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::new(cmds)
    }
}

// ─── Monitor ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct FocusMonitor(pub Dir);

impl Command for FocusMonitor {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let n = state.monitors.len();
        if n <= 1 { return CommandReport::new(cmds); }
        let cur = state.sel_mon;
        let from = state.monitors[cur].focused;
        let new = match self.0 {
            Dir::Left | Dir::Prev => (cur + n - 1) % n,
            _ => (cur + 1) % n,
        };
        if let Some(fw) = state.monitors[cur].focused {
            cmds.push(Effect::Unfocus(fw));
        }
        state.sel_mon = new;
        let to = state.best_focus(new);
        cmds.push(Effect::FocusWindow(to));
        CommandReport::with_event(cmds, Event::FocusChanged { from, to })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MoveWindowToMonitor(pub WindowId, pub Dir);

impl Command for MoveWindowToMonitor {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let n = state.monitors.len();
        if n <= 1 { return CommandReport::new(cmds); }
        let mi = state.sel_mon;
        let win = self.0;
        let new_mi = match self.1 {
            Dir::Left | Dir::Prev => (mi + n - 1) % n,
            _ => (mi + 1) % n,
        };
        let src_ws = state.clients.get(&win).map_or(0, |c| c.workspace);
        let src_ws = src_ws.min(state.monitors[new_mi].workspaces.len().saturating_sub(1));
        let is_float = state.clients.get(&win).is_some_and(crate::types::Client::is_float);
        let dw = cfg.default_col_w;
        state.monitors[mi].workspaces[src_ws].remove_window(win);
        if is_float {
            state.monitors[new_mi].workspaces[src_ws].floats.push(win);
        } else {
            let workarea_w = state.monitors[new_mi].workarea.w;
            state.monitors[new_mi].workspaces[src_ws].add_tiled(win, dw, workarea_w);
        }
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        state.monitors[new_mi].focus_stack.push(win);
        if let Some(c) = state.clients.get_mut(&win) { c.monitor = new_mi; c.workspace = src_ws; }
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::ArrangeMonitor(new_mi));
        state.sel_mon = new_mi;
        cmds.push(Effect::FocusWindow(Some(win)));
        CommandReport::with_event(cmds, Event::WindowMoved(win))
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Which set of gaps a `SetGaps` command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    Inner,
    Outer,
    Both,
}

#[derive(Debug, Clone, Copy)]
pub struct SetGaps(pub GapKind, pub u32);

impl Command for SetGaps {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        match self.0 {
            GapKind::Inner => cfg.gaps_inner = self.1,
            GapKind::Outer => cfg.gaps_outer = self.1,
            GapKind::Both => { cfg.gaps_inner = self.1; cfg.gaps_outer = self.1; }
        }
        let mi = state.sel_mon;
        if mi < state.monitors.len() { cmds.push(Effect::ArrangeMonitor(mi)); }
        CommandReport::with_event(cmds, Event::GapsChanged)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SetBorderWidth(pub u32);

impl Command for SetBorderWidth {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        cfg.border_w = self.0;
        let mi = state.sel_mon;
        let effects = if mi < state.monitors.len() { vec![Effect::ArrangeMonitor(mi)] } else { vec![] };
        CommandReport::with_event(effects, Event::BorderChanged)
    }
}

// ─── System ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Spawn(pub Vec<String>);

impl Command for Spawn {
    fn execute(&mut self, _state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        CommandReport::new(vec![Effect::Spawn(std::mem::take(&mut self.0))])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Quit;

impl Command for Quit {
    fn execute(&mut self, _state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        CommandReport::with_event(vec![Effect::Quit], Event::SessionQuit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Restart;

impl Command for Restart {
    fn execute(&mut self, _state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        CommandReport::with_event(vec![Effect::Restart], Event::SessionRestart)
    }
}
