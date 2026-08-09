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
use crate::core::layout::{fs_ctx, ideal_scroll, ribbon_geom, FsCtx};
use crate::types::{Column, Dir, LayoutKind, State, ViewportMode, WindowId, WinFlags};

#[derive(Debug, Clone, Copy)]
pub struct ToggleMaximize(pub Option<WindowId>);

/// Recenter the scroll camera of `mon_idx`/`ws_i` on its focused column. Every
/// mutator that adds/removes/splits columns (`MoveToWorkspace`, `ToggleFloat`,
/// …) must call this instead of leaving a stale `camera.target` that could land
/// past the new ribbon width (bug C8). Kept as a single helper so the invariant
/// "after any column change the camera follows the focus" lives in one place.
fn scroll_to_focused(state: &mut State, cfg: &Cfg, mi: usize, ws_i: usize) {
    if mi >= state.monitors.len() {
        return;
    }
    let wa = state.monitors[mi].workarea;
    let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs_of(state, mi, ws_i));
    state.monitors[mi].workspaces[ws_i].camera.target = scroll;
}

/// Derive the fullscreen-column descriptor (`FsCtx`) for monitor `mi` / workspace
/// `ws_i` straight from `State`, so every `ideal_scroll` call site can pass the
/// same view of where the fullscreen column lives without borrowing `State`
/// through the ribbon helpers (which only take `&Workspace`).
fn fs_of(state: &State, mi: usize, ws_i: usize) -> FsCtx {
    let mon = &state.monitors[mi];
    fs_ctx(&state.clients, &mon.workspaces[ws_i], mon.screen)
}

/// Pure float⇄tiled topology transition that accompanies entering/leaving a
/// *tiled* fullscreen.
///
/// A tiled fullscreen window is a column of the scrolling ribbon, so a float
/// asking for fullscreen has to join the tiling first — otherwise the layout
/// keeps deriving its rect from `client.geom` (it stays in `ws.floats`) and the
/// window never actually grows. `FS_WAS_FLOAT` remembers the promotion so
/// leaving fullscreen puts the window back where the user had it.
///
/// This is shared by two callers that must not disagree:
///   * `ToggleFullscreen` (the `Mod4+F` keyboard path), and
///   * the backend's `set_fullscreen` (the EWMH `_NET_WM_STATE_FULLSCREEN`
///     client-message path, which used to skip the promotion entirely and left
///     the float collapsed — bug C1/A1).
///
/// It is idempotent on purpose: entering only promotes an actual float, leaving
/// only demotes a window carrying `FS_WAS_FLOAT`, so running it twice for the
/// same transition is a no-op. Returns true when the topology actually changed.
pub fn apply_fullscreen_topology(
    state: &mut State,
    cfg: &Cfg,
    win: WindowId,
    entering: bool,
) -> bool {
    let Some(client) = state.clients.get(&win) else {
        return false;
    };
    let (mi, ws_i) = (client.monitor, client.workspace);
    if mi >= state.monitors.len() || ws_i >= state.monitors[mi].workspaces.len() {
        return false;
    }
    if entering {
        if !client.is_float() {
            return false;
        }
        state.monitors[mi].workspaces[ws_i].remove_window(win);
        state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
        if let Some(c) = state.clients.get_mut(&win) {
            // Snapshot the *float* rect here, while it is still the live
            // geometry. `arrange` overwrites `geom` with the tile rect as soon
            // as it runs, so saving it later (in the backend's `set_fullscreen`)
            // would remember the tile instead of where the user had the float.
            c.saved_geom = c.geom;
            c.flags.clear(WinFlags::FLOAT);
            c.flags.set(WinFlags::FS_WAS_FLOAT);
        }
    } else {
        if !client.flags.has(WinFlags::FS_WAS_FLOAT) {
            return false;
        }
        state.monitors[mi].workspaces[ws_i].remove_window(win);
        state.monitors[mi].workspaces[ws_i].floats.push(win);
        if let Some(c) = state.clients.get_mut(&win) {
            c.flags.set(WinFlags::FLOAT);
            c.flags.clear(WinFlags::FS_WAS_FLOAT);
        }
    }
    true
}

/// Minimum/maximum `page_zoom` factor. < 1.0 would be an Overview-style zoom-out
/// (handled by `ToggleOverview`), so the viewport zoom floor is exactly 1.0 — at
/// or below it the workspace returns to `ViewportMode::Normal`.
const VIEWPORT_ZOOM_MIN: f32 = 1.0;
const VIEWPORT_ZOOM_MAX: f32 = 4.0;

/// Zoom the workspace viewport in/out. Positive `0`th field zooms in, negative
/// zooms out; enters `ViewportMode::Zoomed` and animates the `page_zoom` spring
/// (Fase 9). Keeps the focused column centered while zooming by retargeting the
/// scroll camera, so the enlargement grows around what the user is looking at.
#[derive(Debug, Clone, Copy)]
pub struct ViewportZoom(pub f32);

impl Command for ViewportZoom {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];

        let factor = 1.0 + self.0;
        let new = (ws.page_zoom_target * factor).clamp(VIEWPORT_ZOOM_MIN, VIEWPORT_ZOOM_MAX);
        if new <= VIEWPORT_ZOOM_MIN + 1e-3 {
            // Back to normal: drop the viewport mode and let the spring ease the
            // factor (and the camera) back home.
            ws.viewport_mode = ViewportMode::Normal;
            ws.page_zoom_target = 1.0;
        } else {
            ws.viewport_mode = ViewportMode::Zoomed;
            ws.page_zoom_target = new;
        }
        // Keep the focused column centered under the new zoom (camera animates).
        if ws.layout == LayoutKind::Column {
            ws.camera.target = ideal_scroll(ws, cfg, wa, fs);
        }
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}

/// Scroll the camera by one screen-width (a "page" of the ribbon) in the given
/// direction. Purely visual — no focus change — and reuses `ribbon_geom` /
/// `ideal_scroll` math so the step matches exactly what is on screen. Mirrors
/// `OverviewNav` in accepting only left/right.
#[derive(Debug, Clone, Copy)]
pub struct PageSnap(pub Dir);

impl Command for PageSnap {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        if ws.layout != LayoutKind::Column || ws.columns.is_empty() {
            return CommandReport::new(cmds);
        }
        let g = ribbon_geom(ws, cfg, wa, true, fs);
        // One visible-page worth of world space at the current zoom.
        let step = (wa.w as f32) / g.alpha;
        let dir = match self.0 {
            Dir::Left => -1.0,
            Dir::Right => 1.0,
            _ => return CommandReport::new(cmds),
        };
        let max_scroll = (g.total_w - (wa.w as f32) / g.alpha).max(0.0);
        let new = (ws.camera.target + dir * step).clamp(0.0, max_scroll);
        ws.camera.target = new;
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}


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
                let ci = ws.focus.column_idx.min(n - 1);
                // P2: horizontal navigation keeps the row you were on. Vertical
                // navigation tracks the row by writing `col.focused`, but the
                // destination column carries its own stale `focused` (usually 0),
                // so without carrying the row over, `focus-right` jumps to an
                // unrelated window instead of the neighbouring one.
                let old_row = ws.columns[ci].focused;
                let new_ci = if self.0 == Dir::Left {
                    (ci + n - 1) % n
                } else {
                    (ci + 1) % n
                };
                let ws = &mut state.monitors[mi].workspaces[ws_i];
                ws.focus.column_idx = new_ci;
                let rows = ws.columns[new_ci].windows.len();
                if rows > 0 {
                    ws.columns[new_ci].focused = old_row.min(rows - 1);
                }
                let wa = state.monitors[mi].workarea;
                let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs_of(state, mi, ws_i));
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
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
                let wa = state.monitors[mi].workarea;
                let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs_of(state, mi, ws_i));
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
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
                let ci = state.monitors[mi].workspaces[ws_i].columns.iter().position(|c| c.windows.contains(&target));
                if let Some(ci) = ci {
                    state.monitors[mi].workspaces[ws_i].focus.column_idx = ci;
                }
                state.monitors[mi].focused = Some(target);
                let wa = state.monitors[mi].workarea;
                let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs_of(state, mi, ws_i));
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
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
        let wa = state.monitors[mi].workarea;
        let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs_of(state, mi, ws_i));
        state.monitors[mi].workspaces[ws_i].camera.target = scroll;
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
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
            if let Some(c) = state.clients.get_mut(&win) { c.flags.clear(WinFlags::FLOAT); }
        } else {
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].floats.push(win);
            if let Some(c) = state.clients.get_mut(&win) { c.flags.set(WinFlags::FLOAT); }
        }
        scroll_to_focused(state, cfg, mi, ws_i);
        cmds.push(Effect::MarkRestack(mi));
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::SyncWindowPrefs(win));
        CommandReport::with_event(cmds, Event::FloatToggled(win))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToggleFullscreen(pub Option<WindowId>);

impl Command for ToggleFullscreen {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        if let Some(win) = self.0.or(state.monitors.get(mi).and_then(|m| m.focused)) {
            // The `FULLSCREEN` flag itself is owned by the backend's
            // `set_fullscreen` effect handler, which early-returns when the flag
            // already matches (`fs == c.is_fullscreen()`) — so we must NOT
            // pre-set it here (bug C3). We only mutate the *topology* (floating
            // <-> tiled) here; the flag, border, saved geometry and EWMH state
            // are written by the `SetFullscreen` effect below.
            let is_fs = state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_fullscreen);
            // Entering: a floating window joins the tiling as a fresh column so
            // it can scroll as a normal ribbon participant. Leaving: it goes
            // back to being a float. `apply_fullscreen_topology` is the single
            // implementation, shared with the backend's EWMH path, and is
            // idempotent — so the effect handler re-running it changes nothing.
            apply_fullscreen_topology(state, cfg, win, !is_fs);
            // Recenter the camera and refresh stacking/layout/prefs.
            scroll_to_focused(state, cfg, mi, ws_i);
            cmds.push(Effect::MarkRestack(mi));
            cmds.push(Effect::ArrangeMonitor(mi));
            cmds.push(Effect::SyncWindowPrefs(win));
            cmds.push(Effect::SetFullscreen { win, on: !is_fs });
            return CommandReport::with_event(
                cmds,
                Event::FullscreenToggled { win, on: !is_fs },
            );
        }
        CommandReport::new(cmds)
    }
}

impl Command for ToggleMaximize {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if let Some(win) = self.0.or(state.monitors.get(mi).and_then(|m| m.focused)) {
            // Same rationale as `ToggleFullscreen` (bug C3): the flag is owned by
            // the `set_maximized` effect handler, so don't pre-set it here.
            // The keyboard/IPC "maximize" is the both-axes one — a half-maximize
            // would drive the axes separately, which the effect already allows.
            let on = state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_maximized);
            cmds.push(Effect::SetMaximized {
                win,
                vert: Some(!on),
                horiz: Some(!on),
            });
            return CommandReport::with_event(
                cmds,
                Event::MaximizeToggled { win, on: !on },
            );
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
        let wa = state.monitors[mi].workarea;
        let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_idx], cfg, wa, fs_of(state, mi, ws_idx));
        state.monitors[mi].workspaces[ws_idx].camera.snap(scroll);
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
        state.monitors[mi].workspaces[src_ws].remove_window(win);
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        if is_float {
            state.monitors[mi].workspaces[ws_idx].floats.push(win);
        } else {
            state.monitors[mi].workspaces[ws_idx].remove_window(win);
            state.monitors[mi].workspaces[ws_idx].add_tiled(win, cfg.column_width);
        }
        if let Some(c) = state.clients.get_mut(&win) { c.workspace = ws_idx; }
        // The source workspace just lost a column: recenter its camera so it
        // doesn't stay scrolled past the new (shorter) ribbon (bug C8).
        scroll_to_focused(state, cfg, mi, src_ws);
        cmds.push(Effect::SetWindowDesktop { win, ws: ws_idx });
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(state.best_focus(mi)));
        CommandReport::with_event(cmds, Event::WindowMoved(win))
    }
}
#[derive(Debug, Clone, Copy)]
pub struct GrowColumn(pub i32);

impl Command for GrowColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() { return CommandReport::new(cmds); }
        let ws_i = state.monitors[mi].active_ws;
        let workarea_w = state.monitors[mi].workarea.w;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];

        if ws.columns.is_empty() { return CommandReport::new(cmds); }
        let ci = ws.focus.column_idx.min(ws.columns.len().saturating_sub(1));

        // Scrolling layout: each column has an *independent* width (a fraction of
        // the workarea), and growing/shrinking one column never resizes its
        // neighbours — the ribbon just gets longer/shorter and the camera scrolls.
        // Stealing width from siblings (the old fit-to-screen behaviour) is wrong
        // here (bug C7).
        let col_count = ws.columns.len();
        let usable_w = if col_count > 1 {
            workarea_w as i32 - (col_count as i32 - 1) * cfg.gaps_inner as i32
        } else {
            workarea_w as i32
        };
        if usable_w <= 0 { return CommandReport::new(cmds); }

        let delta_weight = self.0 as f32 / usable_w as f32;
        let old_weight = ws.columns[ci].weight;
        // Upper bound must stay >= the lower bound (0.05), otherwise
        // `f32::clamp` panics (`min > max`) once there are enough columns that
        // `1.0 - 0.05*(n-1)` goes below 0.05 (21+ columns) — see bug C2.
        let max_w = (1.0 - 0.05 * (col_count - 1) as f32).max(0.05);
        ws.columns[ci].weight = (old_weight + delta_weight).clamp(0.05, max_w);

        let scroll = ideal_scroll(ws, cfg, wa, fs);
        ws.camera.target = scroll;
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
        
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        
        // Remove from current column
        ws.remove_window(win);
        
        // If removing the window emptied the column, the column is gone.
        // We need to determine the new column index and weight.
        let new_ci = ci.min(ws.columns.len().saturating_sub(1));
        let survivor_w = if new_ci < ws.columns.len() { 
            Some(ws.columns[new_ci].weight)
        } else if !ws.columns.is_empty() {
            Some(ws.columns.last().unwrap().weight)
        } else {
            None
        };
        
        // Unified "new column" policy (bug C14): the split-out window becomes a
        // sibling column at the configured `column_width` (a fraction of the
        // workarea), independent of how many columns already exist. The
        // surviving column keeps its own width — no stealing, no 70/30
        // fit-to-screen split. If pulling the window out emptied the only
        // column, the new column is the sole one and fills the whole workarea
        // (weight 1.0) instead of a sub-0.1 sliver of the default width (N3).
        let new_w = match survivor_w {
            Some(_) => cfg.column_width,
            None => 1.0,
        };

        let mut new_col = Column::new(new_w);
        new_col.windows.push(win);
        new_col.focused = 0;
        
        let ins_pos = (new_ci + 1).min(ws.columns.len());
        ws.columns.insert(ins_pos, new_col);
        ws.focus.column_idx = ins_pos;
        
        ws.rebalance_weights();
        
        let scroll = ideal_scroll(ws, cfg, wa, fs);
        ws.camera.target = scroll;
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
                // P1: the collapsed column's width must be absorbed by the
                // target, not discarded. `retain` below drops column `ci`, and
                // `rebalance_weights` only repairs weights <= 0 — it never
                // re-normalizes — so without this transfer the total column
                // weight drops by `columns[ci].weight` and the ribbon leaves a
                // permanent empty gap on the right of the workarea.
                // Capped at 1.0 (a full workarea width) so a merged column can
                // never end up wider than the visible area, mirroring the
                // focused-column clamp in `layout::ribbon_geom`.
                let absorbed = ws.columns[ci].weight.max(0.0);
                ws.columns[target].weight = (ws.columns[target].weight + absorbed).min(1.0);
                let wins: Vec<WindowId> = std::mem::take(&mut ws.columns[ci].windows);
                ws.columns[target].windows.extend(wins);
                ws.columns.retain(|c| !c.windows.is_empty());
                ws.focus.column_idx = target.min(ws.columns.len().saturating_sub(1));
                ws.rebalance_weights();
            }
            let scroll = ideal_scroll(
                &state.monitors[mi].workspaces[ws_i],
                cfg,
                state.monitors[mi].workarea,
                fs_of(state, mi, ws_i),
            );
            state.monitors[mi].workspaces[ws_i].camera.target = scroll;
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
        let client = state.clients.get(&win);
        let src_ws = client.map_or(0, |c| c.workspace);
        let is_float = client.is_some_and(crate::types::Client::is_float);
        // Use the real source workspace index for removal from origin monitor.
        let src_ws_real = src_ws.min(state.monitors[mi].workspaces.len().saturating_sub(1));
        // Use the clamped index only for insertion on the destination monitor.
        let dst_ws = src_ws.min(state.monitors[new_mi].workspaces.len().saturating_sub(1));
        state.monitors[mi].workspaces[src_ws_real].remove_window(win);
        if is_float {
            state.monitors[new_mi].workspaces[dst_ws].floats.push(win);
        } else {
            state.monitors[new_mi].workspaces[dst_ws].add_tiled(win, cfg.column_width);
        }
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        state.monitors[new_mi].focus_stack.push(win);
        if let Some(c) = state.clients.get_mut(&win) { c.monitor = new_mi; c.workspace = dst_ws; }
        // Recenter the scroll camera on both the origin (which just lost a window)
        // and the destination (which just gained one) so neither monitor is left
        // with a stale camera that hides the focused column.
        let src_wa = state.monitors[mi].workarea;
        state.monitors[mi].workspaces[src_ws_real].camera.target =
            ideal_scroll(&state.monitors[mi].workspaces[src_ws_real], cfg, src_wa, fs_of(state, mi, src_ws_real));
        let dst_wa = state.monitors[new_mi].workarea;
        state.monitors[new_mi].workspaces[dst_ws].camera.target =
            ideal_scroll(&state.monitors[new_mi].workspaces[dst_ws], cfg, dst_wa, fs_of(state, new_mi, dst_ws));
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

// ─── Overview (semantic-zoom film-strip) ──────────────────────────────────────

/// Toggle the Overview mode on the active workspace: zooms the whole ribbon out
/// (animated) so every column is visible, and back in.
#[derive(Debug, Clone, Copy)]
pub struct ToggleOverview;

impl Command for ToggleOverview {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        ws.overview = !ws.overview;
        ws.zoom_target = if ws.overview { cfg.overview_zoom_min } else { 1.0 };
        let scroll = if layout == LayoutKind::Column {
            ideal_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.target = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}

/// Navigate the column selection while in Overview (also enters Overview if not
/// already active). Only left/right are meaningful; up/down are ignored.
#[derive(Debug, Clone, Copy)]
pub struct OverviewNav(pub Dir);

impl Command for OverviewNav {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        let n = ws.columns.len();
        if n == 0 {
            return CommandReport::new(cmds);
        }
        let cur = ws.focus.column_idx.min(n - 1);
        let new = match self.0 {
            Dir::Left => cur.saturating_sub(1),
            Dir::Right => (cur + 1).min(n - 1),
            _ => cur,
        };
        ws.focus.column_idx = new;
        // Ensure we're in Overview so the strip is visible.
        ws.overview = true;
        ws.zoom_target = cfg.overview_zoom_min;
        let scroll = if layout == LayoutKind::Column {
            ideal_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.target = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        // Overview navigation must also move the real input focus to the window
        // we just selected, otherwise the keyboard keeps going to the previous
        // window and `ws.focus.column_idx` desyncs from `mon.focused` (bug C4).
        if let Some(w) = ws.columns.get(new).and_then(Column::focused_win) {
            cmds.push(Effect::FocusWindow(Some(w)));
        }
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}

/// Drop into the selected column: leave Overview and zoom back to 1.0, keeping
/// the current selection as the focused column.
#[derive(Debug, Clone, Copy)]
pub struct OverviewEnter;

impl Command for OverviewEnter {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        ws.overview = false;
        ws.zoom_target = 1.0;
        let scroll = if layout == LayoutKind::Column {
            ideal_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.target = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        // "Enter" drops into the selected column: move the real focus there too,
        // so the key window matches `ws.focus.column_idx` (bug C4).
        if let Some(w) = ws.columns.get(ws.focus.column_idx).and_then(Column::focused_win) {
            cmds.push(Effect::FocusWindow(Some(w)));
        }
        CommandReport::with_event(
            cmds,
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}
