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
use crate::core::wallpaper::WallpaperSource;

use crate::types::{
    Column, Dir, FullscreenSnapshot, LayoutKind, Rect, State, ViewportMode, WallpaperCmd, WinFlags,
    WindowId, WindowMode,
};

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
    let scroll = ideal_scroll(
        &state.monitors[mi].workspaces[ws_i],
        cfg,
        wa,
        fs_of(state, mi, ws_i),
    );
    state.monitors[mi].workspaces[ws_i].camera.target = scroll;
}

/// Point the workspace focus (column + row) at `win` and retarget its camera so
/// the column `win` lives in comes to rest in view — the *logical* half of a
/// pointer/EWMH focus change.
///
/// This is the pure extraction of what the backend's `Backend::focus()` used to
/// do inline: it moves `camera.target` (the settled rest viewport) and nothing
/// else. It deliberately does **not** re-project: `client.geom` — the rect X11
/// hit-tests clicks against and the pointer warp reads — is only refreshed by a
/// settled `arrange` of the returned monitor. That is why the return value is
/// `#[must_use]`: the `Some(mi)` is the monitor whose settled projection the
/// caller still owes, and dropping it silently reintroduces the "camera moved
/// but geometry didn't" bug (a click then lands on the neighbour's stale rect).
///
/// Returns `None` when `win` is unknown or its monitor/workspace indices are
/// stale (e.g. after a hotplug), in which case nothing was mutated.
#[must_use = "retargeting the camera without re-projecting leaves client.geom stale"]
pub fn retarget_focus_to_window(state: &mut State, cfg: &Cfg, win: WindowId) -> Option<usize> {
    let (mi, ws_i) = {
        let c = state.clients.get(&win)?;
        (c.monitor, c.workspace)
    };
    if mi >= state.monitors.len() || ws_i >= state.monitors[mi].workspaces.len() {
        return None;
    }
    let screen = state.monitors[mi].screen;
    let wa = state.monitors[mi].workarea;
    // Disjoint field borrows (same trick as `struts::retarget_cameras`): read
    // `clients` for the fullscreen descriptor while mutating the workspace.
    let State {
        clients, monitors, ..
    } = state;
    let ws = &mut monitors[mi].workspaces[ws_i];
    if let Some(ci) = ws.columns.iter().position(|col| col.windows.contains(&win)) {
        ws.focus.column_idx = ci;
        if let Some(ri) = ws.columns[ci].windows.iter().position(|&x| x == win) {
            ws.columns[ci].focused = ri;
        }
        let fs = fs_ctx(clients, ws, screen);
        ws.camera.target = ideal_scroll(ws, cfg, wa, fs);
    }
    Some(mi)
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
/// This is called once by `ToggleFullscreen`, which is the single funnel for
/// both the `Mod4+F` keyboard path and the EWMH `_NET_WM_STATE_FULLSCREEN`
/// client-message path. The backend's `set_fullscreen` no longer calls it
/// (it is now X11-only), so there is no longer a second caller to disagree
/// with — which is exactly why the float-collapse bug C1/A1 is gone.
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
            // Capture the fullscreen snapshot (prior mode + exact rect) so that
            // leaving fullscreen restores the float verbatim — robust against an
            // intervening maximize (which would otherwise clobber `saved_geom`).
            c.fs_snapshot = Some(crate::types::FullscreenSnapshot {
                prior: crate::types::WindowMode::Float,
                rect: c.geom,
            });
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

/// Restore a window's geometry from its `FullscreenSnapshot` after leaving
/// fullscreen. Pure (no X11) — `ToggleFullscreen` calls this on leave, and the
/// unit tests call it to simulate the Command's geometry restore exactly.
///
/// This captures the *exact* pre-fullscreen rect, so an intervening
/// maximize/border change (which mutates the shared `saved_geom`) can no longer
/// corrupt the restore (plan 1786564084575, Fase 3). Idempotent: returns the
/// prior `WindowMode` when a snapshot was applied, `None` when there was
/// nothing to restore.
pub fn apply_fullscreen_geom_restore(
    state: &mut State,
    win: WindowId,
) -> Option<crate::types::WindowMode> {
    let Some(c) = state.clients.get_mut(&win) else {
        return None;
    };
    let snap = c.fs_snapshot.take()?;
    c.geom = snap.rect;
    Some(snap.prior)
}

/// Consume `State::pending_focus` when the overlay that deferred it is gone.
/// Reads the single GLOBAL slot and only acts when it is keyed for `(mi, ws_i)`;
/// a deferral bound to a different monitor/workspace stays untouched (so it is
/// never orphaned). `dismissed` is the overlay being torn down, if known: a live
/// overlay that is NOT the one that created this deferral must not swallow a
/// deferral it did not own. Moves the deferred window to logical focus + stack and
/// returns it so the caller can emit `Effect::FocusWindow(Some(p))`. No-op if
/// absent/invalid.
pub(crate) fn consume_pending_focus(
    state: &mut State,
    mi: usize,
    ws_i: usize,
    dismissed: Option<WindowId>,
) -> Option<WindowId> {
    if mi >= state.monitors.len() || ws_i >= state.monitors[mi].workspaces.len() {
        return None;
    }
    let pf = state.pending_focus?;
    // Wrong monitor/workspace: this deferral belongs elsewhere. Leave it in place.
    if pf.monitor != mi || pf.workspace != ws_i {
        return None;
    }
    // A live overlay `o` (not the one that created this deferral) is dismissing:
    // it must not consume a deferral owned by a different overlay. Leave it.
    if let Some(o) = dismissed {
        let o_presented = state.presented_overlay_owner(mi) == Some(o)
            && state
                .monitors
                .get(mi)
                .and_then(|m| m.workspaces.get(ws_i))
                .map_or(false, |_ws| {
                    state.clients.get(&o).is_some_and(|c| c.workspace == ws_i)
                });
        if o_presented && pf.owner != o {
            return None;
        }
    }
    if !state.clients.contains_key(&pf.window) {
        // The deferred target is gone — drop the deferral, nothing to focus.
        state.pending_focus = None;
        return None;
    }
    state.pending_focus = None;
    focus_logical_on(state, mi, pf.window)
}

/// After ANY transition, if `pending_focus` exists but its owner is no longer a
/// presented overlay (per the EXACT `#8c` condition, shared via
/// `State::pending_focus_owner_presented`), resolve it immediately: if the
/// deferred window is still alive, focus it (mirrors `consume_pending_focus`
/// without needing a dismissing overlay); otherwise drop the deferral. This is a
/// centralized safety net called from `Engine::execute`/`execute_batch` right
/// before `assert_invariants`, guaranteeing invariant `#8c` holds right after
/// every `Command::execute()`. Because it uses the identical `#8c` condition as
/// the invariant and the existing `consume_pending_focus` calls, it can never
/// double-resolve (those paths only fire when the owner was already dismissed).
pub(crate) fn reconcile_pending_focus_after_transition(state: &mut State) -> Option<WindowId> {
    let pf = match state.pending_focus {
        Some(p) => p,
        None => return None,
    };
    if state.pending_focus_owner_presented() {
        return None;
    }
    state.pending_focus = None;
    if state.clients.contains_key(&pf.window) {
        focus_logical_on(state, pf.monitor, pf.window)
    } else {
        None
    }
}

/// Apply a *logical* focus (update `mon.focused` + `focus_stack` +
/// `presented_maximize`) without touching the real X input focus. Backend
/// handlers that must update the focus model for a non-selected monitor — or for
/// any path that is not the single real-X sink `Backend::focus()` — use this so
/// that `mon.focused`/`focus_stack`/`x11_input_focus` are only ever written by
/// `focus()` (and this logical helper, which mirrors `focus()`'s state mutation
/// minus the X call). Returns `Some(win)` on success.
pub fn focus_logical_on(state: &mut State, mi: usize, win: WindowId) -> Option<WindowId> {
    if mi >= state.monitors.len() || !state.clients.contains_key(&win) {
        return None;
    }
    let mon = &mut state.monitors[mi];
    mon.focused = Some(win);
    mon.focus_stack.retain(|&x| x != win);
    mon.focus_stack.push(win);
    state.sync_presented_maximize(mi);
    Some(win)
}

/// Pure decision for how `manage` should handle focus when a new window maps.
/// An overlay (fullscreen/maximize owner) that is *not* the new window's own
/// transient parent defers focus to the overlay; a window that belongs to the
/// overlay (or no overlay) takes the focus directly. The backend only *applies*
/// the result; the policy lives here in core.
pub enum ManageFocusIntent {
    Defer {
        owner: WindowId,
        monitor: usize,
        workspace: usize,
    },
    Focus(WindowId),
}

/// Decide, purely, whether a newly-managed `win` should be deferred behind the
/// current presented overlay or focused immediately. See `ManageFocusIntent`.
pub fn decide_manage_focus(state: &State, win: WindowId) -> ManageFocusIntent {
    let mi = state.sel_mon;
    if let Some(owner) = state.presented_overlay_owner(mi) {
        let ws_i = state.monitors[mi].active_ws;
        let owned_dialog = state
            .clients
            .get(&win)
            .and_then(|c| c.transient_parent)
            .is_some_and(|p| owner == p);
        if !owned_dialog {
            return ManageFocusIntent::Defer {
                owner,
                monitor: mi,
                workspace: ws_i,
            };
        }
    }
    ManageFocusIntent::Focus(win)
}

/// Pure EWMH `_NET_ACTIVE_WINDOW` policy decision. Returns whether an
/// app/pager requesting focus for `win` should be honored. The request is
/// refused when honoring it would steal focus from a presented overlay
/// (fullscreen/overlay owner) on `win`'s own (monitor, workspace) and `win`
/// is neither that overlay owner nor an owned dialog of it. We never switch
/// the user's selected monitor or active workspace here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveWindowIntent {
    Focus(WindowId),
    Ignore,
}

pub(crate) fn decide_active_window(state: &State, win: WindowId) -> ActiveWindowIntent {
    let Some(c) = state.clients.get(&win) else {
        return ActiveWindowIntent::Ignore;
    };
    let mi = c.monitor;
    let ws = c.workspace;
    if let Some(owner) = state.presented_overlay_owner_in(mi, ws) {
        if owner != win {
            let owned_dialog = c.transient_parent.is_some_and(|p| p == owner);
            if !owned_dialog {
                return ActiveWindowIntent::Ignore;
            }
        }
    }
    ActiveWindowIntent::Focus(win)
}

/// The single logical owner of maximize state (mirrors `apply_fullscreen_topology`).
/// Decides and mutates MAXIMIZED_V/H, saved_geom, geom, geometry_dirty,
/// and presented_maximize. The backend `set_maximized` is now X11-only.
pub fn apply_maximize(state: &mut State, win: WindowId, vert: Option<bool>, horiz: Option<bool>) {
    if let Some(c) = state.clients.get_mut(&win) {
        let was_max = c.is_maximized();
        let want_v = vert.unwrap_or_else(|| c.is_maximized_v());
        let want_h = horiz.unwrap_or_else(|| c.is_maximized_h());
        if want_v == c.is_maximized_v() && want_h == c.is_maximized_h() {
            return;
        }
        if want_v {
            c.flags.set(WinFlags::MAXIMIZED_V);
        } else {
            c.flags.clear(WinFlags::MAXIMIZED_V);
        }
        if want_h {
            c.flags.set(WinFlags::MAXIMIZED_H);
        } else {
            c.flags.clear(WinFlags::MAXIMIZED_H);
        }
        if !was_max {
            if !c.is_fullscreen() {
                c.saved_geom = c.geom;
            }
        } else if !c.is_maximized() && c.is_float() {
            c.geom = c.saved_geom;
        }
        c.geometry_dirty = true;
    }
    let mi = state.clients.get(&win).map_or(0, |c| c.monitor);
    state.sync_presented_maximize(mi);
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
            // Mutually exclusive with Overview (bug B1): leaving viewport zoom
            // must also clear any Overview state, otherwise the live `zoom`
            // spring would keep easing toward `overview_zoom_min` and surface as
            // a phantom zoom-out once we hand `alpha` back to `zoom`.
            ws.overview = false;
            ws.zoom_target = 1.0;
        } else {
            ws.viewport_mode = ViewportMode::Zoomed;
            ws.page_zoom_target = new;
            // Mutually exclusive with Overview (bug B1): entering viewport zoom
            // clears Overview so its zoom-out factor can't corrupt the `zoom`
            // spring while `alpha` is driven by `page_zoom` instead.
            ws.overview = false;
            ws.zoom_target = 1.0;
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
        let g = ribbon_geom(ws, cfg, wa, true, &fs);
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
        let from = state.monitors.get(before).and_then(|m| m.focused);
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
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let from = state.monitors[mi].focused;
        let ws_i = state.monitors[mi].active_ws;
        // In the `Grid` layout, left/right/up/down navigate by *geometry*, not
        // by the (meaningless, one-window-per-column) column model. Next/Prev
        // keep using the focus stack, handled below.
        if state.monitors[mi].workspaces[ws_i].layout == LayoutKind::Grid
            && matches!(self.0, Dir::Left | Dir::Right | Dir::Up | Dir::Down)
        {
            return focus_grid(state, cfg, mi, ws_i, self.0, from);
        }
        let target: Option<WindowId> = match self.0 {
            Dir::Left | Dir::Right => {
                let ws = &state.monitors[mi].workspaces[ws_i];
                let n = ws.columns.len();
                if n == 0 {
                    return CommandReport::new(cmds);
                }
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
                let scroll = ideal_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
                state.monitors[mi].workspaces[ws_i].columns[new_ci].focused_win()
            }
            Dir::Up | Dir::Down => {
                let ws_i = state.monitors[mi].active_ws;
                let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
                if ci >= state.monitors[mi].workspaces[ws_i].columns.len() {
                    return CommandReport::new(cmds);
                }
                let n = state.monitors[mi].workspaces[ws_i].columns[ci]
                    .windows
                    .len();
                if n == 0 {
                    return CommandReport::new(cmds);
                }
                let new_ri = if self.0 == Dir::Up {
                    (state.monitors[mi].workspaces[ws_i].columns[ci].focused + n - 1) % n
                } else {
                    (state.monitors[mi].workspaces[ws_i].columns[ci].focused + 1) % n
                };
                state.monitors[mi].workspaces[ws_i].columns[ci].focused = new_ri;
                let target = state.monitors[mi].workspaces[ws_i].columns[ci].windows[new_ri];
                let wa = state.monitors[mi].workarea;
                let scroll = ideal_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
                Some(target)
            }
            Dir::Next | Dir::Prev => {
                let ws_i = state.monitors[mi].active_ws;
                let focused = state.monitors[mi].focused;
                let stack = &state.monitors[mi].focus_stack;
                if stack.is_empty() {
                    return CommandReport::new(cmds);
                }
                let stack: Vec<WindowId> = stack
                    .iter()
                    .copied()
                    .filter(|&w| state.clients.get(&w).is_some_and(|c| c.workspace == ws_i))
                    .collect();
                if stack.is_empty() {
                    return CommandReport::new(cmds);
                }
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
                let ci = state.monitors[mi].workspaces[ws_i]
                    .columns
                    .iter()
                    .position(|c| c.windows.contains(&target));
                if let Some(ci) = ci {
                    let ws = &mut state.monitors[mi].workspaces[ws_i];
                    ws.focus.column_idx = ci;
                    // Keep the column's focused row in sync with `target`
                    // (bug B2). Up/Down and MoveWindow read `columns[ci].focused`,
                    // so leaving it stale (usually 0) would later move focus from
                    // the wrong row and desync `ws.focus` from `mon.focused`.
                    if let Some(ri) = ws.columns[ci].windows.iter().position(|&w| w == target) {
                        ws.columns[ci].focused = ri;
                    }
                }
                state.monitors[mi].focused = Some(target);
                let wa = state.monitors[mi].workarea;
                let scroll = ideal_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.target = scroll;
                Some(target)
            }
        };
        if let Some(w) = target {
            cmds.push(Effect::Unfocus(from.unwrap_or(0)));
            state.monitors[mi].focused = Some(w);
            cmds.push(Effect::ArrangeMonitor(mi));
            cmds.push(Effect::FocusWindow(Some(w)));
            return CommandReport::with_event(cmds, Event::FocusChanged { from, to: Some(w) });
        }
        CommandReport::new(cmds)
    }
}

/// Spatial focus navigation for the `Grid` layout. Resolves the neighbour of
/// the focused window by its on-screen geometry (so `h`/`j`/`k`/`l` move in the
/// direction you expect, unlike the column model where every grid window is its
/// own 1-window column). Pure — no camera, no `ideal_scroll` (the grid has none).
fn focus_grid(
    state: &mut State,
    _cfg: &Cfg,
    mi: usize,
    ws_i: usize,
    dir: Dir,
    from: Option<WindowId>,
) -> CommandReport {
    let mut cmds = Vec::new();
    let focused = {
        let mon = &state.monitors[mi];
        let ws = &mon.workspaces[ws_i];
        mon.focused.or_else(|| ws.focused_win())
    };
    let Some(focused) = focused else {
        return CommandReport::new(cmds);
    };

    // Base geometry comes from the snapshot kept fresh by the render path; if it
    // is missing (no arrange yet) fall back to a gap-free arrangement from the
    // raw workarea — direction is scale-invariant, so the neighbour is still
    // correct.
    let placements: Vec<(WindowId, Rect)> = {
        let mon = &state.monitors[mi];
        let ws = &mon.workspaces[ws_i];
        if let Some(s) = &ws.grid_snapshot {
            s.placements.iter().map(|p| (p.win, p.rect)).collect()
        } else {
            let wins: Vec<WindowId> = ws
                .columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .collect();
            let area = mon.workarea;
            crate::core::grid::arrange(&wins, area, 0, 0, None).0
        }
    };
    let Some(target) = crate::core::grid::neighbor(&placements, focused, dir) else {
        return CommandReport::new(cmds);
    };

    {
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        if let Some(ci) = ws.columns.iter().position(|c| c.windows.contains(&target)) {
            ws.focus.column_idx = ci;
            ws.columns[ci].focused = 0;
        }
    }
    state.monitors[mi].focused = Some(target);
    cmds.push(Effect::Unfocus(from.unwrap_or(0)));
    cmds.push(Effect::ArrangeMonitor(mi));
    cmds.push(Effect::FocusWindow(Some(target)));
    CommandReport::with_event(
        cmds,
        Event::FocusChanged {
            from,
            to: Some(target),
        },
    )
}

// ─── Move ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct MoveWindow(pub WindowId, pub Dir);

impl Command for MoveWindow {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        if !state.apply_move_dir(self.1) {
            return CommandReport::new(cmds);
        }
        let wa = state.monitors[mi].workarea;
        let scroll = ideal_scroll(
            &state.monitors[mi].workspaces[ws_i],
            cfg,
            wa,
            fs_of(state, mi, ws_i),
        );
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
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let win = match state.monitors[mi].focused {
            Some(w) => w,
            None => return CommandReport::new(cmds),
        };
        // Operate on the focused window's *own* (home) workspace, not the
        // monitor's `active_ws`: after `ViewWorkspace` the focus can sit on a
        // window tiled in a different workspace than the one currently shown, and
        // toggling float against `active_ws` would remove from (and push to) the
        // wrong tree — leaving the window tiled in its home workspace *and*
        // floating in the active one (cross-workspace duplication, caught by the
        // Fase 5 property harness). `NewColumn` already applies the same guard.
        let ws_i = state
            .clients
            .get(&win)
            .map(|c| c.workspace)
            .unwrap_or_else(|| state.monitors[mi].active_ws);
        let is_float = state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_float);
        if ws_i >= state.monitors[mi].workspaces.len() {
            return CommandReport::new(cmds);
        }
        if is_float {
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
            if let Some(c) = state.clients.get_mut(&win) {
                c.flags.clear(WinFlags::FLOAT);
            }
        } else {
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].floats.push(win);
            if let Some(c) = state.clients.get_mut(&win) {
                c.flags.set(WinFlags::FLOAT);
            }
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
            // The Command owns ALL fullscreen logical state. The backend's
            // `SetFullscreen` effect handler is reduced to the X11-only half
            // (EWMH atom + compositor bypass hint) and must not decide topology,
            // border, snapshot, flags or camera — those belong to the core.
            let on = !state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_fullscreen);

            // 1) Topology (float<->tiled membership + `FS_WAS_FLOAT`). Single
            //    shared pure helper; idempotent, so the old double-call from the
            //    effect handler is gone.
            apply_fullscreen_topology(state, cfg, win, on);

            // 2) Border + 3) `FULLSCREEN` flag + force-reconfigure mark. A
            //    fullscreen tile has no border; the flag is owned here now (the
            //    effect handler no longer sets it).
            if let Some(c) = state.clients.get_mut(&win) {
                if on {
                    c.old_border_w = c.border_w;
                    c.border_w = 0;
                } else {
                    c.border_w = c.old_border_w;
                }
                if on {
                    c.flags.set(WinFlags::FULLSCREEN);
                } else {
                    c.flags.clear(WinFlags::FULLSCREEN);
                }
                // Force a reconfigure even when the rect is unchanged (border /
                // state changed without moving — exactly what the old
                // `geometry_dirty` sentinel was for).
                c.geometry_dirty = true;
            }

            // 4) Snapshot the pre-fullscreen rect on enter (tiled/maximized case;
            //    the float case was already snapshotted by
            //    `apply_fullscreen_topology`), or restore it on leave.
            if on {
                if let Some(c) = state.clients.get_mut(&win) {
                    if !c.flags.has(WinFlags::FS_WAS_FLOAT) {
                        c.fs_snapshot = Some(FullscreenSnapshot {
                            prior: if c.is_maximized() {
                                WindowMode::Maximized
                            } else {
                                WindowMode::Tiled
                            },
                            rect: c.geom,
                        });
                    }
                }
            } else {
                apply_fullscreen_geom_restore(state, win);
            }

            // 5) Camera recenter (logical state). `scroll_to_focused` folds the
            //    newly-set `FULLSCREEN` flag through `fs_ctx`, so it reproduces
            //    exactly the `ideal_scroll` the old backend recenter computed.
            scroll_to_focused(state, cfg, mi, ws_i);

            if !on {
                if let Some(p) = consume_pending_focus(state, mi, ws_i, Some(win)) {
                    cmds.push(Effect::FocusWindow(Some(p)));
                }
            }
            cmds.push(Effect::MarkRestack(mi));
            cmds.push(Effect::ArrangeMonitor(mi));
            cmds.push(Effect::SyncWindowPrefs(win));
            cmds.push(Effect::SetFullscreen { win, on });
            return CommandReport::with_event(cmds, Event::FullscreenToggled { win, on });
        }
        CommandReport::new(cmds)
    }
}

impl Command for ToggleMaximize {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        let ws_i = state.monitors[mi].active_ws;
        if let Some(win) = self.0.or(state.monitors.get(mi).and_then(|m| m.focused)) {
            let on = state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_maximized);
            apply_maximize(state, win, Some(!on), Some(!on));
            cmds.push(Effect::MarkRestack(mi));
            cmds.push(Effect::ArrangeMonitor(mi));
            cmds.push(Effect::SyncWindowPrefs(win));
            cmds.push(Effect::SetMaximized {
                win,
                vert: Some(!on),
                horiz: Some(!on),
            });
            if on {
                if let Some(p) = consume_pending_focus(state, mi, ws_i, Some(win)) {
                    cmds.push(Effect::FocusWindow(Some(p)));
                }
            }
            return CommandReport::with_event(cmds, Event::MaximizeToggled { win, on: !on });
        }
        CommandReport::new(cmds)
    }
}

/// Drag/resize a window to an explicit rectangle.
///
/// The backend's pointer path computes the target rect from pointer motion +
/// `WM_SIZE_HINTS` and hands it here, so ALL float-geometry state mutation
/// stays inside the `Command` funnel (Fase: close the secondary mutation
/// paths). It emits a single `Effect::ConfigureWindow`, which the backend
/// carries out through the reconciler's `apply_geom` — so `configure_window`
/// keeps exactly one owner, and the window's logical `geom` is updated in the
/// same place as every other state transition.
#[derive(Debug, Clone, Copy)]
pub struct MoveResize(pub WindowId, pub Rect);

impl Command for MoveResize {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let win = self.0;
        let rect = self.1;
        if let Some(c) = state.clients.get_mut(&win) {
            c.geom = rect;
            c.flags.set(WinFlags::FLOAT);
            cmds.push(Effect::ConfigureWindow {
                win,
                geom: rect,
                border_w: c.border_w,
            });
        }
        CommandReport::new(cmds)
    }
}

// ─── Layout ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct CycleLayout;

impl Command for CycleLayout {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi < state.monitors.len() {
            let ws_i = state.monitors[mi].active_ws;
            state.monitors[mi].workspaces[ws_i].cycle_layout();
            // A layout switch must leave the scroll in a deterministic position:
            // re-center the camera on the focused column so the focused window is
            // on-screen at rest regardless of where the camera was before the
            // switch (e.g. camera != 0 when going Grid→Column). Without this the
            // focused column could stay off-screen until the next focus change.
            let layout = state.monitors[mi].workspaces[ws_i].layout;
            if layout == LayoutKind::Column {
                let wa = state.monitors[mi].workarea;
                let fs = fs_of(state, mi, ws_i);
                state.monitors[mi].workspaces[ws_i].camera.target =
                    ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs);
            }
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
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi < state.monitors.len() {
            let ws_i = state.monitors[mi].active_ws;
            state.monitors[mi].workspaces[ws_i].layout = self.0;
            // Deterministic scroll after a layout switch (scenario 11): re-center
            // the camera on the focused column in the new layout so the focused
            // window is visible at rest even when the camera was displaced.
            if self.0 == LayoutKind::Column {
                let wa = state.monitors[mi].workarea;
                let fs = fs_of(state, mi, ws_i);
                state.monitors[mi].workspaces[ws_i].camera.target =
                    ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs);
            }
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
        let mon = match state.monitors.get(mi) {
            Some(m) => m,
            None => return CommandReport::new(cmds),
        };
        let from = mon.active_ws;
        let ws_idx = self.0;
        if ws_idx >= mon.workspaces.len() || ws_idx == mon.active_ws {
            return CommandReport::new(cmds);
        }
        state.monitors[mi].active_ws = ws_idx;
        let wa = state.monitors[mi].workarea;
        let scroll = ideal_scroll(
            &state.monitors[mi].workspaces[ws_idx],
            cfg,
            wa,
            fs_of(state, mi, ws_idx),
        );
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
        let win = match state.monitors.get(mi).and_then(|m| m.focused) {
            Some(w) => w,
            None => return CommandReport::new(cmds),
        };
        let src_ws = match state.clients.get(&win) {
            Some(c) => c.workspace,
            None => return CommandReport::new(cmds),
        };
        let ws_idx = self.0;
        if src_ws == ws_idx || ws_idx >= state.monitors[mi].workspaces.len() {
            return CommandReport::new(cmds);
        }
        let is_float = state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_float);
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
        if let Some(c) = state.clients.get_mut(&win) {
            c.workspace = ws_idx;
        }
        // The source workspace just lost a column: recenter its camera so it
        // doesn't stay scrolled past the new (shorter) ribbon (bug C8).
        scroll_to_focused(state, cfg, mi, src_ws);
        // The moved window may have owned the source workspace's maximize
        // overlay; clear any now-dangling `presented_maximize` (invariant #9).
        if state.monitors[mi].workspaces[src_ws].presented_maximize == Some(win) {
            state.monitors[mi].workspaces[src_ws].presented_maximize = None;
        }
        // Resolve the post-move focus the same way the `FocusWindow` effect will
        // (and `run_cmd` applies it to `mon.focused`). `sync_presented_maximize`
        // reads `mon.focused`, so we must point it at the *final* focus BEFORE
        // syncing — otherwise the maximize-overlay owner is computed against the
        // stale post-`retain` focus and desyncs from `presented_overlay_owner`
        // (invariant #9b).
        let new_focus = state.best_focus(mi);
        state.monitors[mi].focused = new_focus;
        state.sync_presented_maximize(mi);
        cmds.push(Effect::SetWindowDesktop { win, ws: ws_idx });
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::FocusWindow(new_focus));
        CommandReport::with_event(cmds, Event::WindowMoved(win))
    }
}
#[derive(Debug, Clone, Copy)]
pub struct GrowColumn(pub i32);

impl Command for GrowColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let workarea_w = state.monitors[mi].workarea.w;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];

        if ws.columns.is_empty() {
            return CommandReport::new(cmds);
        }
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
        if usable_w <= 0 {
            return CommandReport::new(cmds);
        }

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
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let win = match state.monitors[mi].focused {
            Some(w) => w,
            None => return CommandReport::new(cmds),
        };
        // The focused window may live on a workspace other than `active_ws`
        // (left pointed there by `ViewWorkspace`/`MoveToWorkspace`), so operate
        // on its own workspace — otherwise we would splice it into the wrong
        // tree while it is still tiled on its own (cross-workspace duplication,
        // caught by the Fase 5 property harness).
        let ws_i = state.clients.get(&win).map(|c| c.workspace).unwrap_or(ws_i);
        if state
            .clients
            .get(&win)
            .is_none_or(crate::types::Client::is_float)
        {
            return CommandReport::new(cmds);
        }

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
            Event::LayoutChanged {
                monitor: mi,
                workspace: ws_i,
            },
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CollapseColumn;

impl Command for CollapseColumn {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_ws;
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let n_cols = state.monitors[mi].workspaces[ws_i].columns.len();
        if n_cols < 2 || ci == 0 || ci >= n_cols {
            return CommandReport::new(cmds);
        }
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
        if n <= 1 {
            return CommandReport::new(cmds);
        }
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
        if n <= 1 {
            return CommandReport::new(cmds);
        }
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
        // The window may have owned the source workspace's maximize overlay;
        // clear any now-dangling `presented_maximize` (invariant #9). This must
        // cover the source workspace even when it is *not* the monitor's active
        // one, since the stale entry would otherwise trip the invariant later
        // when that workspace becomes active.
        if state.monitors[mi].workspaces[src_ws_real].presented_maximize == Some(win) {
            state.monitors[mi].workspaces[src_ws_real].presented_maximize = None;
        }
        state.monitors[new_mi].focus_stack.push(win);
        if let Some(c) = state.clients.get_mut(&win) {
            c.monitor = new_mi;
            c.workspace = dst_ws;
        }
        // Refresh the maximize-overlay owner on both the source (which just lost
        // the window) and destination (which just gained it) monitors so neither
        // keeps a stale `presented_maximize` reference (invariant #9).
        state.sync_presented_maximize(mi);
        state.sync_presented_maximize(new_mi);
        // Recenter the scroll camera on both the origin (which just lost a window)
        // and the destination (which just gained one) so neither monitor is left
        // with a stale camera that hides the focused column.
        let src_wa = state.monitors[mi].workarea;
        state.monitors[mi].workspaces[src_ws_real].camera.target = ideal_scroll(
            &state.monitors[mi].workspaces[src_ws_real],
            cfg,
            src_wa,
            fs_of(state, mi, src_ws_real),
        );
        let dst_wa = state.monitors[new_mi].workarea;
        state.monitors[new_mi].workspaces[dst_ws].camera.target = ideal_scroll(
            &state.monitors[new_mi].workspaces[dst_ws],
            cfg,
            dst_wa,
            fs_of(state, new_mi, dst_ws),
        );
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
            GapKind::Both => {
                cfg.gaps_inner = self.1;
                cfg.gaps_outer = self.1;
            }
        }
        let mi = state.sel_mon;
        if mi < state.monitors.len() {
            cmds.push(Effect::ArrangeMonitor(mi));
        }
        CommandReport::with_event(cmds, Event::GapsChanged)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SetBorderWidth(pub u32);

impl Command for SetBorderWidth {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        cfg.border_w = self.0;
        let mi = state.sel_mon;
        let effects = if mi < state.monitors.len() {
            vec![Effect::ArrangeMonitor(mi)]
        } else {
            vec![]
        };
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

// ─── Wallpaper ───────────────────────────────────────────────────────────────

/// Apply a `WallpaperCmd` to `state.wallpaper` (pure State mutation) and emit
/// `Effect::SetWallpaper` so the backend uploads/draws the GPU texture, plus a
/// `WallpaperChanged` domain event. Bumps `state.wallpaper_rev` on any change so
/// the compositor can detect a new source without re-decoding every frame.
#[derive(Debug, Clone)]
pub struct SetWallpaper(pub WallpaperCmd);

impl Command for SetWallpaper {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let changed = match &self.0 {
            WallpaperCmd::Set(path) => {
                let src = WallpaperSource::from_path(path.clone());
                if state.wallpaper.source == src {
                    false
                } else {
                    state.wallpaper.source = src;
                    true
                }
            }
            WallpaperCmd::Clear => {
                if state.wallpaper.source == WallpaperSource::None {
                    false
                } else {
                    state.wallpaper.source = WallpaperSource::None;
                    true
                }
            }
            WallpaperCmd::Mode(mode) => {
                if state.wallpaper.mode == *mode {
                    false
                } else {
                    state.wallpaper.mode = *mode;
                    true
                }
            }
        };
        if changed {
            state.wallpaper_rev += 1;
            CommandReport::with_event(vec![Effect::SetWallpaper], Event::WallpaperChanged)
        } else {
            CommandReport::new(vec![])
        }
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
        ws.zoom_target = if ws.overview {
            cfg.overview_zoom_min
        } else {
            1.0
        };
        // Mutually exclusive with Viewport Zoom (bug B1): toggling Overview must
        // reset the page-zoom state, or a lingering `Zoomed` mode would keep
        // `alpha` on `page_zoom` (and leave `overview` ignored) — making Overview
        // a silent no-op or corrupting the live `zoom` spring.
        ws.viewport_mode = ViewportMode::Normal;
        ws.page_zoom_target = 1.0;
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
        // Mutually exclusive with Viewport Zoom (bug B1): entering Overview must
        // reset the page-zoom state or the zoom-out won't take effect.
        ws.viewport_mode = ViewportMode::Normal;
        ws.page_zoom_target = 1.0;
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
        // Mutually exclusive with Viewport Zoom (bug B1): leaving Overview must
        // also drop any pending viewport zoom so the state stays consistent.
        ws.viewport_mode = ViewportMode::Normal;
        ws.page_zoom_target = 1.0;
        let scroll = if layout == LayoutKind::Column {
            ideal_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.target = scroll;
        cmds.push(Effect::ArrangeMonitor(mi));
        // "Enter" drops into the selected column: move the real focus there too,
        // so the key window matches `ws.focus.column_idx` (bug C4).
        if let Some(w) = ws
            .columns
            .get(ws.focus.column_idx)
            .and_then(Column::focused_win)
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::effect::Effect;
    use crate::core::event::Event;
    use crate::core::wallpaper::WallpaperSpec;
    use crate::core::wallpaper::{WallpaperMode, WallpaperSource};
    use crate::types::WallpaperCmd;

    #[test]
    fn set_wallpaper_mutates_state_and_bumps_rev() {
        let mut s = State::new();
        let mut c = Cfg::default();
        let before = s.wallpaper_rev;
        let rep = SetWallpaper(WallpaperCmd::Set("/tmp/wp.png".into())).execute(&mut s, &mut c);
        assert_eq!(
            s.wallpaper.source,
            WallpaperSource::Image("/tmp/wp.png".into())
        );
        assert_eq!(s.wallpaper.mode, WallpaperSpec::default().mode);
        assert_eq!(s.wallpaper_rev, before + 1);
        assert!(rep
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SetWallpaper)));
        assert_eq!(rep.event, Some(Event::WallpaperChanged));
    }

    #[test]
    fn wallpaper_clear_and_mode() {
        let mut s = State::new();
        let mut c = Cfg::default();
        s.wallpaper.source = WallpaperSource::Image("/tmp/a.png".into());
        let rep = SetWallpaper(WallpaperCmd::Clear).execute(&mut s, &mut c);
        assert_eq!(s.wallpaper.source, WallpaperSource::None);
        assert!(rep
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SetWallpaper)));

        let rev = s.wallpaper_rev;
        let rep = SetWallpaper(WallpaperCmd::Mode(WallpaperMode::Fit)).execute(&mut s, &mut c);
        assert_eq!(s.wallpaper.mode, WallpaperMode::Fit);
        assert_eq!(s.wallpaper_rev, rev + 1);
    }

    #[test]
    fn wallpaper_noop_does_not_bump_rev() {
        let mut s = State::new();
        let mut c = Cfg::default();
        s.wallpaper.source = WallpaperSource::Image("/tmp/a.png".into());
        let rev = s.wallpaper_rev;
        let rep = SetWallpaper(WallpaperCmd::Set("/tmp/a.png".into())).execute(&mut s, &mut c);
        assert_eq!(s.wallpaper_rev, rev);
        assert!(rep.effects.is_empty());
        assert!(rep.event.is_none());
    }
}
