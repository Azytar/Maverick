//! Typed command surface for Maverick.
//!
//! What owns: the `Command` trait (`&mut State, &mut Cfg` → `CommandReport`)
//! plus every typed command (`FocusDirection`, `MoveWindow`, `ToggleFullscreen`,
//! `ToggleMaximize`, `GrowColumn`, `ViewportZoom`, `PageSnap`, …) and the pure
//! helpers they share (`apply_fullscreen_topology`, `apply_maximize`,
//! `decide_manage_focus`, `reconcile_pending_focus_after_transition`).
//!
//! Exposes: `Command`, the single mutation entry point for `Engine::execute`,
//! and the shared focus/overlay helpers used by both the keyboard and the EWMH
//! paths.
//!
//! Leaves to others: X11/GL execution (the backend drains the returned
//! `Effect`s in order), `layout::arrange` geometry, `present::present_into`
//! overlays, and `EventBus` dispatch.
//!
//! # The purity contract
//!
//! Every command is a pure transformation over `State`/`Cfg`:
//!
//! - It performs no I/O, makes no X11 round trip, and never reads the wall
//!   clock. Anything that must happen outside the process is *returned* as an
//!   ordered `Vec<Effect>` for the backend to apply, never performed inline.
//! - It returns at most one `Event` naming the domain fact it represents (see
//!   `core::event`). A command knows its own event, never its consumers; the
//!   backend, IPC hub, bars, hooks and tests all subscribe to the `Engine`'s
//!   `EventBus`.
//!
//! This is what makes the reducer testable at all: tests assert on the returned
//! `Effect` list with no X server and no timing. It also means
//! the returned order *is* the contract — the backend applies effects in
//! sequence, so `MarkRestack` before `ArrangeMonitor` when stacking changed and
//! `SetFullscreen`/`SetMaximized` after the arrange that the new presentation
//! state implies.
//!
//! The `pending_focus` safety net (`reconcile_pending_focus_after_transition`)
//! is the centralized post-condition check that `Engine::execute` and
//! `execute_batch` run after every command, before the debug-only
//! `assert_invariants`.

use crate::config::Cfg;
use crate::core::effect::Effect;
use crate::core::event::{CommandReport, Event};
use crate::core::layout::{fs_ctx, ideal_scroll, overview_scroll, ribbon_geom, FsCtx};

use crate::types::{
    Column, Dir, FullscreenPolicy, FullscreenSnapshot, LayoutKind, Rect, State, ViewportMode,
    WinFlags, WindowId, WindowMode, Workspace,
};

/// Ceiling on Views per monitor, for `CreateView`.
///
/// The same limit config parsing already enforces on `n_tags` (1..=9), reused so
/// `CreateView` cannot grow a desktop layout the rest of the system refuses to
/// describe: EWMH `_NET_DESKTOP_NAMES`, the `tag_names` config list and the
/// generated `Mod4+N` binds are all sized by it. Config reload re-reconciles to
/// `n_tags`, so a runtime-created View is not permanent across a reload — the
/// same relationship the pre-existing `n_tags` shrink already has.
const MAX_VIEWS: usize = 9;

#[derive(Debug, Clone, Copy)]
pub struct ToggleMaximize(pub Option<WindowId>);

/// Recenter the scroll camera of `mon_idx`/`ws_i` on its focused column. Every
/// mutator that adds/removes/splits columns (`MoveToWorkspace`, `ToggleFloat`,
/// …) must call this: a camera left over from a longer ribbon can land
/// past the new one, stranding the workspace scrolled off its own content. Kept
/// as a single helper so the invariant "after any column change the camera
/// follows the focus" lives in one place.
///
/// In Overview this keeps the camera where it is when the focused column is
/// already visible ([`overview_scroll`]): a structural change must adjust
/// *visibility*, never the stored entry scale, and a re-centering on every
/// change would scroll the viewport under a selection that had not moved.
fn scroll_to_focused(state: &mut State, cfg: &Cfg, mi: usize, ws_i: usize) {
    let Some(mon) = state.monitors.get(mi) else {
        return;
    };
    let Some(ws) = mon.workspaces.get(ws_i) else {
        return;
    };
    let wa = mon.workarea;
    let scroll = follow_scroll(ws, cfg, wa, fs_of(state, mi, ws_i));
    // Re-borrow after the shared reads above.
    if let Some(mon) = state.monitors.get_mut(mi) {
        if let Some(ws) = mon.workspaces.get_mut(ws_i) {
            ws.camera.retarget(scroll);
        }
    }
}

/// The scroll target after a focus or content change on one View: the
/// Overview pan policy while Overview is on, the centering target otherwise.
///
/// Split out from the normal path on purpose — Overview navigation pans an
/// existing viewport instead of re-centering it — so the two policies cannot
/// drift: every command that moves focus or content on the *same* View goes
/// through here, while a View *switch* still snaps to `ideal_scroll` (the
/// camera of the View switched to may be stale, and there is no viewport to
/// preserve yet).
fn follow_scroll(ws: &Workspace, cfg: &Cfg, wa: Rect, fs: FsCtx) -> f32 {
    if ws.overview {
        overview_scroll(ws, cfg, wa, fs)
    } else {
        ideal_scroll(ws, cfg, wa, fs)
    }
}

/// Point the workspace focus (column + row) at `win` and retarget its camera so
/// the column `win` lives in comes to rest in view — the *logical* half of a
/// pointer/EWMH focus change.
///
/// In Overview the camera only moves when the newly selected column is not
/// already visible: a click or hover on a visible tile must select it without
/// shoving the viewport, and the scale is never touched — it was fixed on
/// entry. Only the camera moves; nothing is re-projected. `client.geom` — the rect
/// X11 hit-tests clicks against and the pointer warp reads — is refreshed only
/// by a settled `arrange` of the returned monitor. That is why the return value
/// is `#[must_use]`: the `Some(mi)` names the monitor whose settled projection
/// the caller still owes, and dropping it silently reintroduces "camera moved
/// but geometry didn't" (a click then lands on the neighbour's stale rect).
///
/// Returns `None` when `win` is unknown or its monitor/workspace indices are
/// stale (e.g. after a hotplug), in which case nothing was mutated.
#[must_use = "retargeting the camera without re-projecting leaves client.geom stale"]
pub fn retarget_focus_to_window(state: &mut State, cfg: &Cfg, win: WindowId) -> Option<usize> {
    // Resolved through the client's own `ViewId`, never through the monitor's
    // active View: a focus slot can name a window placed on another View.
    let mi = state.clients.get(&win)?.monitor;
    let ws_i = state
        .monitors
        .get(mi)?
        .view_index(state.clients[&win].workspace)?;
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
        ws.camera.retarget(follow_scroll(ws, cfg, wa, fs));
    }
    Some(mi)
}

/// Derive the fullscreen-column descriptor (`FsCtx`) for monitor `mi` / workspace
/// `ws_i` straight from `State`, so every `ideal_scroll` call site can pass the
/// same view of where the fullscreen column lives without borrowing `State`
/// through the ribbon helpers (which only take `&Workspace`).
fn fs_of(state: &State, mi: usize, ws_i: usize) -> FsCtx {
    // Defensive: stale indices after hotplug must yield "no fullscreen",
    // never a panic. Callers already guard, this is the last line of defense.
    let (Some(mon), Some(ws)) = (
        state.monitors.get(mi),
        state.monitors.get(mi).and_then(|m| m.workspaces.get(ws_i)),
    ) else {
        return FsCtx::default();
    };
    fs_ctx(&state.clients, ws, mon.screen)
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
/// `ToggleFullscreen` is the single funnel for this transition, covering both
/// the `Mod4+F` keyboard path and the EWMH `_NET_WM_STATE_FULLSCREEN` client
/// message, so the two channels can never disagree about the topology.
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
    let mi = client.monitor;
    let Some(ws_i) = state
        .monitors
        .get(mi)
        .and_then(|m| m.view_index(client.workspace))
    else {
        return false;
    };
    if entering {
        if !client.is_float() {
            return false;
        }
        state.monitors[mi].workspaces[ws_i].remove_window(win);
        state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
        if let Some(c) = state.clients.get_mut(&win) {
            // Snapshot the *float* rect here, while it is still the live
            // geometry. `arrange` overwrites `geom` with the tile rect as soon
            // as it runs, so saving it any later would remember the tile instead
            // of where the user had the float.
            c.saved_geom = c.geom;
            // Capture the fullscreen snapshot (prior mode + exact rect) so that
            // leaving fullscreen restores the float verbatim — robust against an
            // intervening maximize (which would otherwise clobber `saved_geom`).
            c.fs_snapshot = Some(crate::types::FullscreenSnapshot {
                prior: crate::types::WindowMode::Float,
                rect: c.geom,
                policy: c.fullscreen_policy,
            });
            c.flags.clear(WinFlags::FLOAT);
            // A column is not a float, so it cannot be a sticky float: leaving
            // `STICKY` set would exempt the window from parking while the ribbon
            // never projects it, stranding it on screen over another workspace.
            c.flags.clear(WinFlags::STICKY);
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
        // Settle against the workarea that shaped the float's pre-promotion
        // rect: if struts or gaps changed meanwhile, re-seating once here
        // avoids a visible jump when the first `arrange` corrects it.
        // Single "new context" helper, see `layout::settle_float_in_workarea`.
        crate::core::layout::settle_float_in_workarea(state, mi, win);
    }
    true
}

/// Restore a window's geometry from its `FullscreenSnapshot` after leaving
/// fullscreen. Pure (no X11) — `ToggleFullscreen` calls this on leave, and the
/// unit tests call it to reproduce the command's geometry restore exactly.
///
/// Captures the *exact* pre-fullscreen rect, so an intervening maximize or
/// border change (which mutates the shared `saved_geom`) can no longer corrupt
/// the restore. The pre-fullscreen `FullscreenPolicy` is restored too (entering
/// promotes to `True`; without this one toggle cycle would clobber a
/// `Deny`/`True` rule).
///
/// Idempotent: returns the prior `WindowMode` when a snapshot was applied,
/// `None` when there was nothing to restore.
pub fn apply_fullscreen_geom_restore(
    state: &mut State,
    win: WindowId,
) -> Option<crate::types::WindowMode> {
    let c = state.clients.get_mut(&win)?;
    let snap = c.fs_snapshot.take()?;
    c.geom = snap.rect;
    c.fullscreen_policy = snap.policy;
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
    let mon = state.monitors.get(mi)?;
    let ws = mon.workspaces.get(ws_i)?;
    // The deferral is keyed on a `ViewId`, so the comparison is by identity: a
    // deferral recorded against a View that still exists at a *different*
    // position after a removal is still the right deferral.
    let view = ws.id;
    let pf = state.pending_focus?;
    // Wrong monitor/view: this deferral belongs elsewhere. Leave it in place.
    if pf.monitor != mi || pf.workspace != view {
        return None;
    }
    // A live overlay `o` (not the one that created this deferral) is dismissing:
    // it must not consume a deferral owned by a different overlay. Leave it.
    if let Some(o) = dismissed {
        let o_presented = state.presented_overlay_owner(mi) == Some(o)
            && state.clients.get(&o).is_some_and(|c| c.workspace == view);
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

/// After ANY transition, resolve a `pending_focus` whose owner is no longer a
/// presented overlay (exactly the `State::pending_focus_owner_presented` test
/// that `consume_pending_focus` and the invariant check share): if the deferred
/// window is still alive, focus it; otherwise drop the deferral.
///
/// Centralized safety net called from `Engine::execute`/`execute_batch` right
/// before `assert_invariants`, so the `pending_focus` invariant holds right after
/// every `Command::execute()`. Sharing the exact predicate with
/// `consume_pending_focus` is what makes double resolution impossible: those
/// paths only fire when the owner was already dismissed, and this one only fires
/// when it is not presented.
pub(crate) fn reconcile_pending_focus_after_transition(state: &mut State) -> Option<WindowId> {
    let pf = state.pending_focus?;
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

/// Resolve a deferral that a *requested* focus move is about to orphan.
///
/// A command that moves the logical focus itself (`FocusDirection`,
/// `ViewWorkspace`, …) needs nothing: `Engine::execute` runs
/// [`reconcile_pending_focus_after_transition`] afterwards and sees the new
/// focus. A command that only emits `Effect::FocusWindow` and lets the X sink
/// apply it breaks that premise — the safety net runs while the old focus is
/// still in place, still sees the overlay as presented, and leaves the deferral
/// queued for an overlay the pending effect is about to unfocus. The input focus
/// would then land on a window nobody can see behind a dismissed overlay.
///
/// Only the focus-driven presentation is at stake: a *maximize* overlay is
/// presented exactly while it holds the focus (that is what
/// `State::pending_focus_owner_presented` tests), so a request that takes the
/// focus off the deferral's owner has to resolve the deferral now. A fullscreen
/// owner stays presented regardless of the focus and keeps its queue.
///
/// An explicit focus request supersedes the queued one (the same reasoning as
/// `FocusDirection` yielding an overlay), so the deferral is dropped rather than
/// redirected: the requested window is what the user asked to be looking at.
fn drop_deferral_yielded_by_focus_move(state: &mut State, to: Option<WindowId>) {
    let Some(pf) = state.pending_focus else {
        return;
    };
    if Some(pf.owner) == to {
        return;
    }
    let owner_holds_focus = state
        .monitors
        .get(pf.monitor)
        .and_then(|m| m.focused)
        .is_some_and(|f| f == pf.owner);
    if owner_holds_focus {
        state.pending_focus = None;
    }
}

/// Apply a *logical* focus (update `mon.focused` + `focus_stack` +
/// `presented_maximize`) without touching the real X input focus.
///
/// The single-writer rule: `mon.focused`/`focus_stack`/`x11_input_focus` are
/// written by the real-X sink `Backend::focus()` and by this helper, which
/// mirrors that sink's state mutation minus the X call. Backend handlers that
/// must update the focus model for a non-selected monitor use it instead.
///
/// Returns `Some(win)` on success.
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
    let ws_i = state.monitors[mi].active_index();
    if let Some(owner) = state.presented_overlay_owner(mi) {
        // The owner has to be presented *in the context the deferral is keyed on*.
        // `presented_overlay_owner` is monitor-scoped: it reads that monitor's
        // focus stack, which can legitimately name a window placed elsewhere (a
        // focus slot is a logical pointer — see `overlay_presented_in`), and the
        // newly mapped window is placed on `sel_mon`, so the only overlay that can
        // be stealing *its* focus is one presented right here. Deferring behind an
        // overlay that is not on this monitor would create a deferral its own
        // lifetime test (`State::pending_focus_owner_presented`, which
        // `check_invariants` #8c shares) rejects the instant it is recorded: the
        // slot is keyed to a context the owner does not live in, so the next
        // command's safety net would resolve it again immediately and the new
        // window would never actually be held back.
        if state.monitors[mi]
            .workspaces
            .get(ws_i)
            .is_some_and(|ws| state.overlay_presented_in(mi, ws.id, owner))
        {
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

/// The single logical owner of maximize state (mirrors `apply_fullscreen_topology`):
/// decides and mutates `MAXIMIZED_V/H`, `saved_geom`, `geom`, `geometry_dirty`
/// and `presented_maximize`. The backend's `SetMaximized` handler carries only
/// the X11 half.
///
/// Also the reconciliation point for a deferral this transition orphans, because
/// the EWMH per-axis path (`_NET_WM_STATE_MAXIMIZED_VERT` / `..._HORZ` in
/// `src/backend/x11/events.rs`) calls this *directly* instead of going through
/// `Engine::execute`, so it never reaches the engine's post-command safety net.
/// A maximize overlay is presented exactly while it holds the focus, so
/// un-maximizing the owner — or the client asking for a single axis to be
/// dropped — takes the overlay down, and a deferral left queued behind it is
/// exactly the orphan `check_invariants` #8c rejects. Resolving it here is the
/// same reconciliation `manage()` performs after focusing a window off an
/// overlay. The focus reaches X through the ordinary path: this writes the
/// logical focus, and the backend's `reconcile_focus` re-asserts a logical focus
/// that diverges from the server's on the next focus event, which is the
/// mechanism every state-only focus write already relies on.
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
    sync_presented_maximize_everywhere(state, win);
    // A no-op request returns above without touching presentation, so it must not
    // resolve anything either: an early return here is the difference between
    // "the EWMH path heals the deferral it orphans" and "the EWMH path orphans a
    // deferral on every repeat of the same client message".
    let _ = reconcile_pending_focus_after_transition(state);
}

/// Re-derive the maximize presentation on every monitor that can be showing `win`.
///
/// `presented_maximize` is a function of the *focus*, not of the client: a
/// monitor's owner is the window its focus slot names on its active workspace. A
/// focus slot is written on the monitor the user is looking at, which need not be
/// the window's own monitor, so one window can be the presented overlay owner of
/// several monitors. Every transition that changes what the derivation reads —
/// this window's maximize flags or the workspace it lives on — must therefore
/// refresh all of them, not just `c.monitor`: a monitor left behind would keep
/// naming a window that is no longer maximized, or one that no longer lives on
/// the workspace the name is attached to, which is what the `presented_maximize`
/// invariant rejects.
fn sync_presented_maximize_everywhere(state: &mut State, win: WindowId) {
    for mi in 0..state.monitors.len() {
        let names_win = state.monitors[mi].focused == Some(win);
        if names_win || state.clients.get(&win).is_some_and(|c| c.monitor == mi) {
            state.sync_presented_maximize(mi);
        }
    }
}

/// Minimum/maximum `page_zoom` factor. < 1.0 would be an Overview-style zoom-out
/// (handled by `ToggleOverview`), so the viewport zoom floor is exactly 1.0 — at
/// or below it the workspace returns to `ViewportMode::Normal`.
const VIEWPORT_ZOOM_MIN: f32 = 1.0;
const VIEWPORT_ZOOM_MAX: f32 = 4.0;

/// Zoom the workspace viewport in/out: a positive field zooms in, a negative one
/// zooms out. Enters `ViewportMode::Zoomed` and rescales the ribbon immediately.
/// Keeps the focused column centered by retargeting the scroll camera, so the
/// enlargement grows around what the user is looking at.
#[derive(Debug, Clone, Copy)]
pub struct ViewportZoom(pub f32);

impl Command for ViewportZoom {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        let Some(mon) = state.monitors.get(mi) else {
            return CommandReport::new(cmds);
        };
        let ws_i = mon.active_index();
        let Some(_) = mon.workspaces.get(ws_i) else {
            return CommandReport::new(cmds);
        };
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];

        // `core::action` refuses a non-finite delta, but this command is also
        // reachable directly (`Engine::dispatch` takes a typed `Action`, and a
        // caller can build one), so the rejection is made here too. It must be
        // a rejection rather than a substitution: silently reading a non-finite
        // request as "no zoom change" would still fall through to the branch
        // below that clears Overview and reports a layout change, turning
        // malformed input into an apparently successful operation.
        if !self.0.is_finite() || self.0.abs() > 10.0 {
            return CommandReport::new(cmds);
        }
        let delta = self.0;
        let factor = 1.0 + delta;
        let new = (ws.page_zoom * factor).clamp(VIEWPORT_ZOOM_MIN, VIEWPORT_ZOOM_MAX);
        // `clamp` still yields NaN if the stored factor was already NaN (a
        // session restored from a poisoned file): repair to 1.0 rather than
        // persisting the poison.
        let new = if new.is_finite() { new } else { 1.0 };
        if new <= VIEWPORT_ZOOM_MIN + 1e-3 {
            // Back to normal: drop the viewport mode and put the factor back.
            ws.viewport_mode = ViewportMode::Normal;
            ws.page_zoom = 1.0;
            // Mutually exclusive with Overview: leaving viewport zoom must also
            // clear any Overview state, otherwise a workspace left in Overview
            // would project at the stored entry scale rather than the settled
            // view.
            ws.overview = false;
            ws.overview_scale = 1.0;
        } else {
            ws.viewport_mode = ViewportMode::Zoomed;
            ws.page_zoom = new;
            // Same mutual exclusion on the way in: Overview is cleared so its
            // stored entry scale cannot fight the enlargement this command just
            // asked for.
            ws.overview = false;
            ws.overview_scale = 1.0;
        }
        // Keep the focused column centered under the new zoom.
        if ws.layout == LayoutKind::Column {
            ws.camera.retarget(ideal_scroll(ws, cfg, wa, fs));
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
        let ws_i = state.monitors[mi].active_index();
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        if ws.layout != LayoutKind::Column || ws.columns.is_empty() {
            return CommandReport::new(cmds);
        }
        let g = ribbon_geom(ws, cfg, wa, &fs);
        // One visible-page worth of world space at the settled zoom. The
        // geometry subtracts `gaps_outer` into `g.wa`, and the screen projection
        // scales that already-inset width; using the outer workarea here would
        // overshoot the real page and compute a different max scroll.
        let visible_w = (g.wa.w as f32 / g.alpha).max(1.0);
        let step = visible_w;
        let dir = match self.0 {
            Dir::Left => -1.0,
            Dir::Right => 1.0,
            _ => return CommandReport::new(cmds),
        };
        let cam_min = g.cx / g.alpha;
        let cam_max = g.total_w - (g.wa.w as f32 - g.cx) / g.alpha;
        let (lo, hi) = if cam_max <= cam_min {
            let center = (g.total_w - g.wa.w as f32) / 2.0;
            (center, center)
        } else {
            (cam_min, cam_max)
        };
        let new = (ws.camera.position + dir * step).clamp(lo, hi);
        ws.camera.retarget(new);
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

/// Pure mutation of `State`/`Cfg` into `Effect`s and an optional `Event`.
///
/// The single entry point `Engine::execute`/`execute_batch` call. Implementors
/// never touch X11/GL — they decide *what* (`Effect` vocabulary) and *which
/// domain fact* (`Event`), leaving *how* to the backend. Every `Engine` mutation
/// funnels through this trait so effects and events stay auditable.
pub trait Command {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport;
}

#[derive(Debug, Clone, Copy)]
pub struct FocusWindow(pub Option<WindowId>);

impl Command for FocusWindow {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let before = state.sel_mon;
        let from = state.monitors.get(before).and_then(|m| m.focused);
        if let Some(win) = self.0 {
            // The named window must be managed: `Event::FocusChanged` reports
            // where focus moved, and the backend's `focus` sink treats a window
            // that is not a client as `None` — i.e. it would clear the focus it
            // was asked to move. Announcing `to: Some(win)` for an unmanaged id
            // published a transition the sink never performed.
            if before < state.monitors.len() && state.clients.contains_key(&win) {
                // The sink applies this focus, i.e. after `Engine::execute` ran
                // its deferral safety net; see the helper.
                drop_deferral_yielded_by_focus_move(state, Some(win));
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
        // The monitor this command retargets before navigating, which may differ
        // from `mi` when the focus slot names a window placed on another monitor.
        let mut retargeted = None;
        let from = state.monitors[mi].focused;
        let ws_i = state.monitors[mi].active_index();
        // A window mapped behind an overlay can insert a column and move the
        // workspace cursor without receiving focus. Navigate from the actual
        // focused window, not that insertion cursor.
        if matches!(self.0, Dir::Left | Dir::Right) {
            if let Some(win) = from {
                // The monitor whose camera this moves is not necessarily the one
                // this command navigates: the focus slot is a logical pointer, so
                // it can name a window placed on another monitor (the same
                // divergence `ToggleFullscreen` documents before resolving its
                // target's own monitor). That retarget moves a camera without
                // re-projecting, so `client.geom` on that monitor stays where it
                // was while the hit-test extents follow the camera — a click then
                // lands on the neighbour's stale rect. The arrange owed for it is
                // emitted with the rest, below.
                retargeted = retarget_focus_to_window(state, cfg, win);
            }
        }
        let target: Option<WindowId> = match self.0 {
            Dir::Left | Dir::Right => {
                let ws = &state.monitors[mi].workspaces[ws_i];
                let n = ws.columns.len();
                if n == 0 {
                    return CommandReport::new(cmds);
                }
                let ci = ws.focus.column_idx.min(n - 1);
                // Horizontal navigation carries the current row over. Vertical
                // navigation tracks it by writing `col.focused`, but the
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
                let scroll = follow_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
                state.monitors[mi].workspaces[ws_i].columns[new_ci].focused_win()
            }
            Dir::Up | Dir::Down => {
                let ws_i = state.monitors[mi].active_index();
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
                let scroll = follow_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
                Some(target)
            }
            Dir::Next | Dir::Prev => {
                let ws_i = state.monitors[mi].active_index();
                let focused = state.monitors[mi].focused;
                let stack = &state.monitors[mi].focus_stack;
                if stack.is_empty() {
                    return CommandReport::new(cmds);
                }
                let stack: Vec<WindowId> = stack
                    .iter()
                    .copied()
                    .filter(|&w| {
                        state
                            .clients
                            .get(&w)
                            .is_some_and(|c| c.workspace == state.monitors[mi].ws().id)
                    })
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
                    // Keep the column's focused row in sync with `target`.
                    // Up/Down and MoveWindow read `columns[ci].focused`, so
                    // leaving it stale (usually 0) would later move focus from
                    // the wrong row and desync `ws.focus` from `mon.focused`.
                    if let Some(ri) = ws.columns[ci].windows.iter().position(|&w| w == target) {
                        ws.columns[ci].focused = ri;
                    }
                }
                state.monitors[mi].focused = Some(target);
                let wa = state.monitors[mi].workarea;
                let scroll = follow_scroll(
                    &state.monitors[mi].workspaces[ws_i],
                    cfg,
                    wa,
                    fs_of(state, mi, ws_i),
                );
                state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
                Some(target)
            }
        };
        if let Some(w) = target {
            // Explicit horizontal navigation yields exclusive presentation to
            // the ribbon. Keep FULLSCREEN, borders and the restore snapshot:
            // returning to that column still fills the screen, but it no longer
            // follows the camera over its neighbours. Passive focus changes and
            // navigation with no other column must not dismiss an overlay.
            if matches!(self.0, Dir::Left | Dir::Right) && from != Some(w) {
                let windows: Vec<_> = state.monitors[mi].workspaces[ws_i]
                    .columns
                    .iter()
                    .flat_map(|col| col.windows.iter().copied())
                    .collect();
                for owner in windows {
                    if owner == w {
                        continue;
                    }
                    if let Some(c) = state.clients.get_mut(&owner) {
                        if c.is_fullscreen_overlay() {
                            c.fullscreen_policy = FullscreenPolicy::Normal;
                            c.geometry_dirty = true;
                            // This explicit navigation supersedes a deferred
                            // map-time focus owned by the overlay being yielded.
                            if state.pending_focus.is_some_and(|pf| pf.owner == owner) {
                                state.pending_focus = None;
                            }
                            cmds.push(Effect::MarkRestack(mi));
                        }
                    }
                }
                scroll_to_focused(state, cfg, mi, ws_i);
            }
            // `from == None` means "nothing was focused": omit Unfocus
            // instead of emitting `Unfocus(0)` (0 is never a valid XID and
            // produces a spurious X11 error).
            if let Some(f) = from {
                cmds.push(Effect::Unfocus(f));
            }
            state.monitors[mi].focused = Some(w);
            // The maximize overlay is presented exactly while its window holds the
            // focus, so the owner has to follow this write. Every direction
            // funnels through here, including `Next`/`Prev` (whose own write
            // above lands on the same value before any reader runs), so one call
            // covers them all. Without it the state handed to `Engine::execute`
            // still names the previously focused window, and `core::present`,
            // `render::stack_overlay` and `presented_overlay_owner` (hence
            // `best_focus`) keep presenting and stacking a window the user just
            // navigated away from.
            state.sync_presented_maximize(mi);
            cmds.push(Effect::ArrangeMonitor(mi));
            // The camera this command moved before navigating, if it was not this
            // monitor's, still owes a settled projection. Without it that monitor
            // keeps its old `client.geom` against the camera the retarget just
            // moved, and its clicks land on the wrong column.
            if let Some(other) = retargeted.filter(|&other| other != mi) {
                cmds.push(Effect::ArrangeMonitor(other));
            }
            cmds.push(Effect::FocusWindow(Some(w)));
            return CommandReport::with_event(cmds, Event::FocusChanged { from, to: Some(w) });
        }
        CommandReport::new(cmds)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MoveWindow(pub WindowId, pub Dir);

impl Command for MoveWindow {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        // The window's *own* monitor and workspace, not `sel_mon` +
        // `active_ws`: a named window can be tiled somewhere else entirely, and
        // a move applied to the selected monitor's active workspace would
        // reorder a tree this window is not in.
        let Some(client) = state.clients.get(&self.0) else {
            return CommandReport::new(cmds);
        };
        let mi = client.monitor;
        let Some(ws_i) = state
            .monitors
            .get(mi)
            .and_then(|m| m.view_index(client.workspace))
        else {
            return CommandReport::new(cmds);
        };
        if !state.apply_move_dir_for(self.0, self.1) {
            return CommandReport::new(cmds);
        }
        let wa = state.monitors[mi].workarea;
        let scroll = follow_scroll(
            &state.monitors[mi].workspaces[ws_i],
            cfg,
            wa,
            fs_of(state, mi, ws_i),
        );
        state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
        cmds.push(Effect::ArrangeMonitor(mi));
        // This focus is requested, not applied, so a deferral owned by the
        // overlay it takes the focus from is resolved here; see the helper.
        drop_deferral_yielded_by_focus_move(state, Some(self.0));
        cmds.push(Effect::FocusWindow(Some(self.0)));
        CommandReport::with_event(cmds, Event::WindowMoved(self.0))
    }
}

/// Ask a managed window to close.
///
/// The target must be a client. Every other window-targeting command resolves
/// its target through `state.clients` and absorbs when it is not there
/// (`MoveWindow`, `ToggleFloat`, `MoveResize`, `MoveWindowToMonitor`), and a
/// window the WM does not manage is not this command's to close: the backend's
/// `kill` would send `WM_DELETE_WINDOW`, or `XKillClient` on a client that
/// advertises no protocol, to an arbitrary X11 window — including one owned by
/// another client.
///
/// `Event::WindowUnmapped` says a window *left the managed set*. A window that
/// was never in it has not left it, so the event was false as well as the
/// effect.
#[derive(Debug, Clone, Copy)]
pub struct KillWindow(pub WindowId);

impl Command for KillWindow {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        if !state.clients.contains_key(&self.0) {
            return CommandReport::new(cmds);
        }
        cmds.push(Effect::KillWindow(self.0));
        CommandReport::with_event(cmds, Event::WindowUnmapped(self.0))
    }
}

/// Undo the Overview entry scale on a tiled window's `Client::geom` before it
/// becomes a float's persistent geometry.
///
/// A tiled window's `geom` is the *projected* rect: the backend writes the wire
/// rect back, so under Overview it carries the stored entry scale. Floating
/// that rect as-is would launder a temporary view scale into the float's own
/// geometry — a window shrunk by the viewport it was selected in, which no
/// later arrange would ever grow back. The fix divides the scale back out,
/// keeping the tile's center so the float appears where the tile was, and lets
/// the settle that follows clamp the full-size rect into the workarea.
///
/// Bounded on purpose rather than a logical/geometry split: the only writer
/// that can observe a projected rect is this transition, so correcting the
/// rect at the transition keeps the single `Client::geom` field honest without
/// a second geometry store. No-op outside Overview and at a `1.0` entry scale.
fn unscale_overview_tile_for_float(state: &mut State, mi: usize, ws_i: usize, win: WindowId) {
    let scale = state
        .monitors
        .get(mi)
        .and_then(|m| m.workspaces.get(ws_i))
        .map(|ws| ws.overview_scale)
        .unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 || scale >= 1.0 {
        return;
    }
    let Some(c) = state.clients.get_mut(&win) else {
        return;
    };
    // The projected tile rect is border-exclusive (`inner_w = col_w * alpha -
    // 2 * bw`), so dividing the scale back out of the bare width would come up
    // short by the frame the projection reserved: re-add it before unscaling
    // and reserve it again after, or every float born in Overview would lose
    // `2 * bw * (1/alpha - 1)` px it never gets back (the settle below only
    // ever shrinks).
    let frame = 2.0 * c.border_w as f32;
    let g = c.geom;
    let nw = (((g.w as f32 + frame) / scale - frame)
        .round()
        .clamp(1.0, 16_384.0)) as u32;
    let nh = (((g.h as f32 + frame) / scale - frame)
        .round()
        .clamp(1.0, 16_384.0)) as u32;
    let cx = g.x as f32 + (g.w as f32 + frame) / 2.0;
    let cy = g.y as f32 + (g.h as f32 + frame) / 2.0;
    let nx = ((cx - (nw as f32 + frame) / 2.0)
        .round()
        .clamp(-16_384.0, 16_384.0)) as i32;
    let ny = ((cy - (nh as f32 + frame) / 2.0)
        .round()
        .clamp(-16_384.0, 16_384.0)) as i32;
    c.geom = Rect::new(nx, ny, nw, nh);
}

/// Toggle floating for `Some(win)`, or for the selected monitor's focused
/// window when `None`.
///
/// The single funnel for the `Mod4+F` keybinding, the IPC `togglefloat`
/// action and `maverickctl window float <id>`, so the three channels cannot
/// drift apart in topology, border, snapshot, policy or camera handling.
///
/// The targeted form resolves the window's *own* monitor instead of
/// `sel_mon`, and skips the cross-monitor guard below: that guard exists
/// because a stale focus slot can name a window the focus-repair paths have not
/// settled yet, which is an ambiguity of *focus*. An explicit request carries
/// no such ambiguity — the client that asked for this window cannot be
/// mistaken about which one it means.
#[derive(Debug, Clone, Copy)]
pub struct ToggleFloat(pub Option<WindowId>);

impl Command for ToggleFloat {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = match self.0 {
            Some(win) => match state.clients.get(&win) {
                Some(c) => c.monitor,
                None => return CommandReport::new(cmds),
            },
            None => state.sel_mon,
        };
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let win = match self.0 {
            Some(win) => win,
            None => match state.monitors[mi].focused {
                Some(w) => w,
                None => return CommandReport::new(cmds),
            },
        };
        // Operate on the focused window's *own* (home) workspace, not the
        // monitor's `active_ws`: after `ViewWorkspace` the focus can sit on a
        // window tiled in a different workspace than the one currently shown, and
        // toggling float against `active_ws` would remove from (and push to) the
        // wrong tree — leaving the window tiled in its home workspace *and*
        // floating in the active one (cross-workspace duplication).
        // `NewColumn` applies the same guard.
        //
        // A focus slot can also name a window that is not a client at all: the
        // teardown path purges the focus bookkeeping of the monitor the window
        // was *placed* on, so a slot on another monitor keeps naming it until
        // the next focus settles there. Both branches below write into a
        // placement index, and a placement index must only ever name live
        // clients, so a stale slot absorbs the toggle instead of pushing a
        // phantom entry into `floats`.
        let is_float = match state.clients.get(&win) {
            Some(c) => c.is_float(),
            None => return CommandReport::new(cmds),
        };
        // Guard against cross-monitor focus corruption — only for the
        // *unfocused* form, where `mi` came from `sel_mon` and the focused
        // window may genuinely belong elsewhere.
        if self.0.is_none() && state.clients.get(&win).is_some_and(|c| c.monitor != mi) {
            return CommandReport::new(cmds);
        }
        // The View the window is *actually* placed in, resolved from its own
        // `ViewId`. A window named by a focus slot can sit on another View of the
        // same monitor, and toggling against the active one would splice it into
        // a tree that does not contain it.
        let ws_i = {
            let c = &state.clients[&win];
            match state
                .monitors
                .get(c.monitor)
                .and_then(|m| m.view_index(c.workspace))
            {
                Some(i) => i,
                None => return CommandReport::new(cmds),
            }
        };
        // A fullscreen window may only be a float if it is an *exclusive*
        // overlay. `FullscreenPolicy::True` is one wherever it lives —
        // `present_into` rewrites its entry to `mon.screen` regardless of the
        // list it is in — but `FullscreenPolicy::Normal` means "a ribbon column
        // that happens to fill the screen", and both places that know how to
        // present that look at `ws.columns` only. Floating one would therefore
        // present it as an ordinary workarea float while
        // `_NET_WM_STATE_FULLSCREEN` still tells the client it fills the screen.
        //
        // The state is reachable — fullscreen, navigate to a sibling column
        // (which yields exclusive presentation and demotes the policy to
        // Normal, keeping the FULLSCREEN flag), then float it — so the toggle is
        // refused here rather than the state being made presentable. Leaving
        // fullscreen first is the user's to ask for; a keybinding that silently
        // dropped a fullscreen would be the more surprising half.
        if state
            .clients
            .get(&win)
            .is_some_and(|c| c.is_fullscreen() && !c.is_fullscreen_overlay())
        {
            return CommandReport::new(cmds);
        }
        if is_float {
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
            if let Some(c) = state.clients.get_mut(&win) {
                c.flags.clear(WinFlags::FLOAT);
                // `STICKY` modifies a float, not a window: a sticky float rides
                // above every workspace of its monitor, and a sticky *tile* is a
                // state the layout has no meaning for. `hide_offscreen` exempts
                // sticky windows from parking, so a sticky tile is never hidden,
                // and `arrange` only projects the active workspace, so it is
                // never drawn either — it just stays on screen over whichever
                // workspace the user moved to. Tearing a window off is the user
                // overriding the rule that made it float, so it overrides the
                // stickiness that rule attached too.
                c.flags.clear(WinFlags::STICKY);
            }
        } else {
            // A tile floated while Overview is on carries the entry scale in
            // its `Client::geom` (the backend wrote the projected rect back).
            // Undo it before the float list adopts the rect, or the viewport
            // leaks into persistent geometry.
            unscale_overview_tile_for_float(state, mi, ws_i, win);
            state.monitors[mi].workspaces[ws_i].remove_window(win);
            state.monitors[mi].workspaces[ws_i].floats.push(win);
            // The FLOAT flag must move with the window: `settle_float_in_workarea`
            // (below) is a no-op for a non-float by design, and every
            // `is_float()`-gated policy (drag authority, borders, prefs sync,
            // MoveResize) reads the flag, not the `floats` membership.
            if let Some(c) = state.clients.get_mut(&win) {
                c.flags.set(WinFlags::FLOAT);
            }
            // The projected tile rect can fall outside grid/hints, so a window
            // re-settled (idempotently) against its workarea now does not get
            // displaced by the first arrange. Single "new context" helper, see
            // `layout::settle_float_in_workarea`.
            crate::core::layout::settle_float_in_workarea(state, mi, win);
        }
        scroll_to_focused(state, cfg, mi, ws_i);
        cmds.push(Effect::MarkRestack(mi));
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::SyncWindowPrefs(win));
        CommandReport::with_event(cmds, Event::FloatToggled(win))
    }
}

/// Toggle fullscreen for `Some(win)`, or for the selected monitor's focused
/// window when `None`.
///
/// The single funnel for both the `Mod4+F` keybinding and the EWMH
/// `_NET_WM_STATE_FULLSCREEN` client message, so the two channels cannot drift
/// apart in topology, border, snapshot, policy or camera handling.
#[derive(Debug, Clone, Copy)]
pub struct ToggleFullscreen(pub Option<WindowId>);

impl Command for ToggleFullscreen {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        // A targeted window's own monitor, not `sel_mon`: the camera recentre,
        // the pending-focus consumption and the arrange below are all
        // monitor-indexed, and running them against `sel_mon` for a window on
        // another monitor would move the camera and re-arrange the wrong screen.
        let target = self
            .0
            .or_else(|| state.monitors.get(state.sel_mon).and_then(|m| m.focused));
        let mi = match target {
            Some(win) => match state.clients.get(&win) {
                Some(c) => c.monitor,
                None => return CommandReport::new(cmds),
            },
            None => state.sel_mon,
        };
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_index();
        if let Some(win) = target {
            // This command owns ALL fullscreen logical state. The backend's
            // `SetFullscreen` handler is the X11-only half (the EWMH atom and
            // the `_NET_WM_BYPASS_COMPOSITOR` hint published for external
            // compositors) and must not decide topology, border, snapshot, flags
            // or camera — those belong to the core.
            let on = !state
                .clients
                .get(&win)
                .is_some_and(crate::types::Client::is_fullscreen);

            // Topology: float⇄tiled membership plus `FS_WAS_FLOAT`.
            apply_fullscreen_topology(state, cfg, win, on);

            // Border, the `FULLSCREEN` flag, and a forced reconfigure. A
            // fullscreen tile has no border.
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
                // Force a reconfigure even when the rect is unchanged: the border
                // and the state changed without the window moving.
                c.geometry_dirty = true;
            }

            // Snapshot the pre-fullscreen rect on enter (tiled/maximized case;
            // the float case was already snapshotted by
            // `apply_fullscreen_topology`), or restore it on leave.
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
                            policy: c.fullscreen_policy,
                        });
                    }
                }
            } else {
                apply_fullscreen_geom_restore(state, win);
            }

            // Exclusive-overlay promotion. Entering fullscreen promotes to
            // `True` so `present` pins it over the screen, `decide_manage_focus`
            // defers later windows behind it, and the bypass policy can step
            // aside. Must run AFTER the snapshot above (which has to record the
            // pre-promotion policy) and is undone by the restore on leave, so a
            // user rule is never clobbered.
            if on {
                if let Some(c) = state.clients.get_mut(&win) {
                    c.fullscreen_policy = crate::types::FullscreenPolicy::True;
                }
            }

            // Recenter the camera. `scroll_to_focused` folds the newly-set
            // `FULLSCREEN` flag through `fs_ctx`, so the ribbon scrolls the same
            // way the fullscreen column will be projected.
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
        let Some(mon) = state.monitors.get(mi) else {
            return CommandReport::new(cmds);
        };
        let ws_i = mon.active_index();
        if ws_i >= mon.workspaces.len() {
            return CommandReport::new(cmds);
        }
        if let Some(win) = self.0.or(state.monitors.get(mi).and_then(|m| m.focused)) {
            // The target must be a live client, for the same reason
            // `ToggleFloat` checks twice: a focus slot can still name a window
            // that is not a client (teardown purges the bookkeeping of the
            // monitor the window was *placed* on, leaving a slot elsewhere
            // naming it). `is_maximized` reads `false` for a window that is
            // not there, so the toggle resolved to "maximize it", and the
            // command then announced `MaximizeToggled { on: true }` and asked
            // the backend to set a maximized state on a window that does not
            // exist. `apply_maximize` mutates nothing in that case, so the
            // report claimed a transition that never happened.
            let on = match state.clients.get(&win) {
                Some(c) => c.is_maximized(),
                None => return CommandReport::new(cmds),
            };
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
/// The backend's pointer path computes the target rect from pointer motion plus
/// `WM_SIZE_HINTS` and hands it here, so ALL float-geometry state mutation
/// stays inside the `Command` funnel. Emits a single `Effect::ConfigureWindow`,
/// which the backend carries out through the reconciler's `apply_geom` — so
/// `configure_window` keeps exactly one owner and the window's logical `geom`
/// is written in the same place as every other state transition.
#[derive(Debug, Clone, Copy)]
pub struct MoveResize(pub WindowId, pub Rect);

impl Command for MoveResize {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let win = self.0;
        let mut rect = self.1;
        // Sanitize hostile geometry before normalizing: a 0x0 or u32::MAX rect
        // must never reach the float clamp (the tiled path already does this).
        rect.w = rect.w.clamp(1, 16_384);
        rect.h = rect.h.clamp(1, 16_384);
        rect.x = rect.x.clamp(-16_384, 16_384);
        rect.y = rect.y.clamp(-16_384, 16_384);
        // Single float normalization point (see `layout::normalize_float_geom`):
        // the drag already delivers a normalized rect, and re-normalizing leaves
        // it bit-for-bit identical, so it costs nothing while protecting the
        // callers that pass raw rects. Without it `MoveResize` installed sizes
        // outside grid/hints that the next arrange corrected — the float visibly
        // jumped a frame after every move.
        let (hints, wa, bw) = match state.clients.get(&win) {
            Some(c) => {
                let wa = state
                    .monitors
                    .get(c.monitor)
                    .map_or(Rect::new(rect.x, rect.y, 16_384, 16_384), |m| m.workarea);
                (c.hints, wa, c.border_w)
            }
            None => return CommandReport::new(cmds),
        };
        rect = crate::core::layout::normalize_float_geom(rect, hints, wa, bw);
        if let Some(c) = state.clients.get_mut(&win) {
            c.geom = rect;
            cmds.push(Effect::ConfigureWindow {
                win,
                geom: rect,
                border_w: c.border_w,
            });
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
            let ws_i = state.monitors[mi].active_index();
            if ws_i >= state.monitors[mi].workspaces.len() {
                return CommandReport::new(cmds);
            }
            state.monitors[mi].workspaces[ws_i].layout = self.0;
            // Deterministic scroll after a layout switch: re-center the camera
            // on the focused column in the new layout so the focused window is
            // visible at rest even when the camera was displaced.
            if self.0 == LayoutKind::Column {
                let wa = state.monitors[mi].workarea;
                let fs = fs_of(state, mi, ws_i);
                let scroll = ideal_scroll(&state.monitors[mi].workspaces[ws_i], cfg, wa, fs);
                state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
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

/// Select View `self.0` (carousel position) on the selected monitor.
///
/// The **goto** half of the carousel. It addresses a position rather than a
/// `ViewId` because that is what the wire vocabulary already carries
/// (`Action::View(n)`, EWMH `_NET_CURRENT_DESKTOP`, the generated `Mod4+N`
/// binds), and the mapping from one to the other happens here — exactly once,
/// at the boundary. The carousel itself only ever speaks `ViewId`.
///
/// Refuses when the position names no View, and when it is already current: a
/// no-op switch would re-arrange and re-focus the monitor for nothing.
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
        let from = mon.active_index();
        let ws_idx = self.0;
        let Some(target) = mon.view_id(ws_idx) else {
            return CommandReport::new(cmds);
        };
        if Some(target) == mon.carousel.current() {
            return CommandReport::new(cmds);
        }
        // Resolved through the carousel, so the switch is a View-identity change
        // and never a positional index the layout might disagree with.
        if !state.monitors[mi].goto_view(target) {
            return CommandReport::new(cmds);
        }
        // Resolve the post-switch focus *inside* the command (the same
        // resolution the `FocusWindow` effect applies below), so the logical
        // state is coherent immediately and never depends on the effect being
        // applied: `focused == best_focus(active View)` — a window of the new
        // View (or its presented overlay owner), or `None`. An alive
        // `pending_focus` deferral is untouched: `best_focus` never returns the
        // deferred window, so the ping-pong restore keeps handing input to the
        // overlay owner, never to the deferred window.
        let focus = state.best_focus(mi);
        state.monitors[mi].focused = focus;
        // The presented maximize overlay follows `mon.focused`; leaving the
        // source View's overlay recorded while focus moved would dangle the
        // overlay/`pending_focus` bookkeeping (`presented_maximize` must name a
        // maximized client on the *active* View).
        state.sync_presented_maximize(mi);
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

/// Advance the carousel one step on the selected monitor.
///
/// The **next** half: purely logical and instantaneous, wrapping from the last
/// View to the first. It contains no layout branch — `Carousel::next` decides,
/// and `Carousel` knows nothing about `LayoutKind`.
#[derive(Debug, Clone, Copy)]
pub struct NextView;

impl Command for NextView {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        select_relative_view(state, cfg, ViewStep::Next)
    }
}

/// Step the carousel backwards one place, wrapping from the first View to the
/// last. See [`NextView`].
#[derive(Debug, Clone, Copy)]
pub struct PreviousView;

impl Command for PreviousView {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        select_relative_view(state, cfg, ViewStep::Prev)
    }
}

/// Select the origin View — the explicit return point pinned where the session
/// started on this monitor. See [`NextView`] for the "no layout branch" contract.
#[derive(Debug, Clone, Copy)]
pub struct ReturnToOriginView;

impl Command for ReturnToOriginView {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mi = state.sel_mon;
        let Some(mon) = state.monitors.get(mi) else {
            return CommandReport::new(Vec::new());
        };
        let from = mon.active_index();
        if mon.carousel.current() == mon.carousel.origin() {
            return CommandReport::new(Vec::new());
        }
        if !state.monitors[mi].return_to_origin() {
            return CommandReport::new(Vec::new());
        }
        finish_view_switch(state, cfg, mi, from)
    }
}

#[derive(Debug, Clone, Copy)]
enum ViewStep {
    Next,
    Prev,
}

/// Move the carousel by one relative step and settle the monitor.
///
/// Shared by [`NextView`] and [`PreviousView`] so the two cannot drift: both
/// take exactly the same post-switch path, and neither inspects `LayoutKind`.
fn select_relative_view(state: &mut State, cfg: &mut Cfg, step: ViewStep) -> CommandReport {
    let mi = state.sel_mon;
    let Some(mon) = state.monitors.get(mi) else {
        return CommandReport::new(Vec::new());
    };
    let from = mon.active_index();
    let moved = match step {
        ViewStep::Next => state.monitors[mi].next_view(),
        ViewStep::Prev => state.monitors[mi].previous_view(),
    };
    // A single-View carousel refuses to step: there is nowhere to go, and
    // re-arranging for a no-op would only cost a frame.
    if !moved {
        return CommandReport::new(Vec::new());
    }
    finish_view_switch(state, cfg, mi, from)
}

/// The post-switch settle shared by every way of changing the active View:
/// re-derive the monitor's logical focus, re-point the maximize overlay, snap
/// the new View's camera to its own content, then arrange and focus.
///
/// Extracted so goto / next / previous / return cannot diverge: the only thing
/// that distinguishes them is *which* View they selected, never what they do
/// afterwards.
fn finish_view_switch(state: &mut State, cfg: &Cfg, mi: usize, from: usize) -> CommandReport {
    let mut cmds = Vec::new();
    let to = state.monitors[mi].active_index();
    let focus = state.best_focus(mi);
    state.monitors[mi].focused = focus;
    state.sync_presented_maximize(mi);
    let wa = state.monitors[mi].workarea;
    let scroll = ideal_scroll(
        &state.monitors[mi].workspaces[to],
        cfg,
        wa,
        fs_of(state, mi, to),
    );
    state.monitors[mi].workspaces[to].camera.snap(scroll);
    cmds.push(Effect::SetCurrentDesktop(to));
    cmds.push(Effect::ArrangeMonitor(mi));
    cmds.push(Effect::FocusWindow(state.best_focus(mi)));
    CommandReport::with_event(
        cmds,
        Event::WorkspaceChanged {
            monitor: mi,
            from,
            to,
        },
    )
}

/// Append a new empty View on the selected monitor.
///
/// The **create** lifecycle operation. It is a pure logical transition: no X11
/// window, pixmap or slot is created, and the current/origin View is left alone
/// (creating a View must not move the user off what they were looking at).
///
/// The count ceiling is the one already enforced by config parsing for `n_tags`,
/// reused here so `CreateView` cannot produce a desktop layout the rest of the
/// system refuses to describe (EWMH `_NET_DESKTOP_NAMES`, the `tag_names` config,
/// and the generated `Mod4+N` binds are all sized by it).
#[derive(Debug, Clone, Copy)]
pub struct CreateView;

impl Command for CreateView {
    fn execute(&mut self, state: &mut State, _cfg: &mut Cfg) -> CommandReport {
        let mi = state.sel_mon;
        let Some(mon) = state.monitors.get_mut(mi) else {
            return CommandReport::new(Vec::new());
        };
        if mon.workspaces.len() >= MAX_VIEWS {
            return CommandReport::new(Vec::new());
        }
        mon.create_view();
        CommandReport::new(vec![Effect::RefreshDesktops, Effect::PublishIpcState])
    }
}

/// Drop a View by carousel position on the selected monitor.
///
/// The **remove** lifecycle operation. Removing is refused outright when the
/// View still holds clients: where they would go is a policy decision the
/// campaign does not specify, and silently relocating a user's windows would be
/// the more surprising outcome. Empty it first (or move its clients out) and the
/// removal goes through.
///
/// `Carousel::detach` repairs `current` and `origin` inside
/// `Monitor::remove_view_at`, so this command cannot leave either dangling.
#[derive(Debug, Clone, Copy)]
pub struct RemoveView(pub usize);

impl Command for RemoveView {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mi = state.sel_mon;
        let Some(mon) = state.monitors.get(mi) else {
            return CommandReport::new(Vec::new());
        };
        let pos = self.0;
        let Some(view) = mon.workspaces.get(pos) else {
            return CommandReport::new(Vec::new());
        };
        if !view.is_empty() {
            return CommandReport::new(Vec::new());
        }
        let from = mon.active_index();
        // `State::remove_view` (not `Monitor::remove_view_at`) so the clients are
        // re-homed first: the command already refused a non-empty View, but
        // routing through `State` means a future caller cannot get the unsafe
        // half.
        if state.remove_view(mi, pos).is_none() {
            return CommandReport::new(Vec::new());
        }
        // The camera, focus and overlay bookkeeping were all derived against the
        // View that just went away; re-derive them for whatever the carousel
        // selected instead. A `pending_focus` keyed on the removed View is dropped
        // by its own lifetime test on the next transition.
        //
        // Removing the *current* View is a View switch (`detach` re-pointed it) and
        // so owes exactly the settle a goto does — same code path, so the two
        // cannot disagree. Removing any other View leaves the current one alone and
        // only needs the EWMH count refreshed.
        let mut report = if pos == from {
            finish_view_switch(state, cfg, mi, from)
        } else {
            state.monitors[mi].focused = state.best_focus(mi);
            state.sync_presented_maximize(mi);
            CommandReport::new(vec![
                Effect::ArrangeMonitor(mi),
                Effect::FocusWindow(state.best_focus(mi)),
            ])
        };
        report.effects.insert(0, Effect::RefreshDesktops);
        report
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
        // A placement is addressed by the client's OWN `(monitor, workspace)`
        // pair — the same pair `State::remove_client` uses, and the only
        // authority on where the window actually lives. Taking the workspace
        // from the client record while taking the monitor from `sel_mon` mixes
        // two coordinate systems, and the two can disagree: the focus slot is
        // written by the X sink on the monitor the user is looking at, so it can
        // name a window that is placed on another monitor. The removal then
        // addresses a workspace that never held the window, is a silent no-op,
        // and the re-insert below leaves the client referenced from two
        // placements at once.
        let src_mi = match state.clients.get(&win) {
            Some(c) => c.monitor,
            None => return CommandReport::new(cmds),
        };
        let src_ws = match state.clients.get(&win) {
            Some(c) => c.workspace,
            None => return CommandReport::new(cmds),
        };
        // A View belongs to exactly one monitor, so this command can only
        // relocate a window that is already on the selected monitor. A window
        // belonging to another monitor is `MoveWindowToMonitor`'s business — that
        // command re-derives the focus and re-arranges *both* monitors, which a
        // workspace move must not emulate halfway. Absorb instead of half-moving
        // the window, which is the only way the destination insert can be
        // guaranteed to land next to an exact source removal.
        if src_mi != mi {
            return CommandReport::new(cmds);
        }
        let mon = match state.monitors.get(mi) {
            Some(m) => m,
            None => return CommandReport::new(cmds),
        };
        // Destination is a carousel *position* (what the wire carries), resolved to
        // its `ViewId` here; the source is already an identity.
        let ws_idx = self.0;
        let Some(dst_view) = mon.view_id(ws_idx) else {
            return CommandReport::new(cmds);
        };
        // The source comes from the client record, which can name a View that no
        // longer exists (a `n_tags` shrink / session restore). Resolving it
        // through the monitor's own list is what makes that detectable.
        let Some(src_ws_i) = mon.view_index(src_ws) else {
            return CommandReport::new(cmds);
        };
        if src_ws == dst_view {
            return CommandReport::new(cmds);
        }
        // `remove_window` is a no-op when the tree does not actually contain
        // `win` (a stale client record), so bail out rather than duplicating the
        // window into the destination. Same guard `MoveWindowToMonitor` uses.
        let contained = mon.workspaces[src_ws_i]
            .columns
            .iter()
            .any(|c| c.windows.contains(&win))
            || mon.workspaces[src_ws_i].floats.contains(&win);
        if !contained {
            return CommandReport::new(cmds);
        }
        let is_float = state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_float);
        state.monitors[mi].workspaces[src_ws_i].remove_window(win);
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        if is_float {
            // A float's `Client::geom` is WM-authoritative and is *not* touched by a
            // membership change: the window keeps the exact rect the user gave it,
            // and `settle_float_in_workarea` only clears the client-authority seal
            // (the move was the WM's doing, not the client's).
            state.monitors[mi].workspaces[ws_idx].floats.push(win);
            crate::core::layout::settle_float_in_workarea(state, mi, win);
        } else {
            // The source removal above already took the window out of its only
            // placement (the `contained` guard proved where that was), so the
            // destination insert needs no second removal: the two Views are
            // distinct, and re-removing from the destination is what used to
            // address the wrong workspace when the source coordinate was wrong.
            state.monitors[mi].workspaces[ws_idx].add_tiled(win, cfg.column_width);
        }
        if let Some(c) = state.clients.get_mut(&win) {
            // Only membership changes. Floating, geometry, metadata and identity
            // are all deliberately untouched by a View move.
            c.workspace = dst_view;
        }
        // The source View just lost a column: recenter its camera so it
        // doesn't stay scrolled past the new (shorter) ribbon.
        scroll_to_focused(state, cfg, mi, src_ws_i);
        // The moved window may have owned the source View's maximize
        // overlay; clear the now-dangling `presented_maximize` (it must name a
        // maximized client on the active View).
        if state.monitors[mi].workspaces[src_ws_i].presented_maximize == Some(win) {
            state.monitors[mi].workspaces[src_ws_i].presented_maximize = None;
        }
        // Resolve the post-move focus the same way the `FocusWindow` effect will.
        // `sync_presented_maximize` reads `mon.focused`, so it must be pointed at
        // the *final* focus BEFORE syncing — otherwise the maximize-overlay owner
        // is computed against the stale post-`retain` focus and desyncs from
        // `presented_overlay_owner`.
        let new_focus = state.best_focus(mi);
        state.monitors[mi].focused = new_focus;
        state.sync_presented_maximize(mi);
        // The window's workspace just changed, so *every* monitor that can be
        // showing it has to re-derive: another monitor's focus slot may already
        // name it as the presented overlay owner on a workspace it no longer
        // lives on. See `sync_presented_maximize_everywhere`.
        sync_presented_maximize_everywhere(state, win);
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
        let ws_i = state.monitors[mi].active_index();
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
        // neighbours — the ribbon just gets longer/shorter and the camera
        // scrolls. Stealing width from siblings (fit-to-screen) is wrong here.
        let col_count = ws.columns.len();
        // Clamp config gaps: `gaps_inner: u32` is user-controlled and
        // `as i32` wraps at u32::MAX (== -1). Bound it to the same ceiling
        // the layout uses so `usable_w` can never go pathological.
        let gap_i = (cfg.gaps_inner.min(1_000_000)) as i32;
        let usable_w = if col_count > 1 {
            workarea_w.saturating_sub(((col_count as i32 - 1).saturating_mul(gap_i)) as u32) as i32
        } else {
            workarea_w as i32
        };
        if usable_w <= 0 {
            return CommandReport::new(cmds);
        }

        // Convert the pixel delta into a weight delta against the space the
        // columns can actually use, so a drag of N px widens the column by N px
        // of usable width.
        let delta_weight = self.0 as f32 / usable_w as f32;
        let old_weight = ws.columns[ci].weight;
        // A column may fill the whole workarea (`weight == 1.0`) regardless of how
        // many columns exist. A lower ceiling (e.g. `1.0 - 0.05*(n-1)`, reserving
        // 5% of peek per neighbour) is a fit-to-screen leftover: it blocks the
        // second column at 0.95 while a first column created at 1.0 does fill the
        // screen. In a scrollable ribbon nothing has to be reserved — the
        // neighbour simply scrolls out of view.
        let max_w = 1.0;
        ws.columns[ci].weight = (old_weight + delta_weight).clamp(0.05, max_w);

        let scroll = follow_scroll(ws, cfg, wa, fs);
        ws.camera.retarget(scroll);
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
        let ws_i = state.monitors[mi].active_index();
        let win = match state.monitors[mi].focused {
            Some(w) => w,
            None => return CommandReport::new(cmds),
        };
        // The focused window may live on a View other than the active one
        // (left pointed there by `ViewWorkspace`/`MoveToWorkspace`), so operate
        // on its own View — otherwise we would splice it into the wrong
        // tree while it is still tiled on its own (cross-View duplication).
        let ws_i = state
            .clients
            .get(&win)
            .and_then(|c| state.monitors.get(c.monitor))
            .and_then(|m| m.view_index(state.clients[&win].workspace))
            .unwrap_or(ws_i);
        // Same for the monitor: the focus slot lives on the monitor the user is
        // looking at and can name a window that is placed on another one. Splicing
        // that window into this monitor's tree would reference it from two
        // placements at once; relocating it across monitors is
        // `MoveWindowToMonitor`'s job, so absorb the request instead.
        if state.clients.get(&win).is_some_and(|c| c.monitor != mi) {
            return CommandReport::new(cmds);
        }
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

        // Splice the window out of its column. If that emptied the column, the
        // column index shifts, so the insertion point has to be recomputed.
        ws.remove_window(win);

        let new_ci = ci.min(ws.columns.len().saturating_sub(1));
        let survivor_w = if new_ci < ws.columns.len() {
            Some(ws.columns[new_ci].weight)
        } else if !ws.columns.is_empty() {
            Some(ws.columns.last().unwrap().weight)
        } else {
            None
        };

        // New-column policy: the split-out window becomes a sibling column at
        // the configured `column_width` (a fraction of the workarea),
        // independent of how many columns already exist. The surviving column
        // keeps its own width — no stealing, no 70/30 fit-to-screen split. If
        // pulling the window out emptied the only column, the new column is the
        // sole one and fills the whole workarea (weight 1.0) instead of a
        // sub-0.1 sliver of the default width.
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

        let scroll = follow_scroll(ws, cfg, wa, fs);
        ws.camera.retarget(scroll);
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
        let ws_i = state.monitors[mi].active_index();
        let ci = state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let n_cols = state.monitors[mi].workspaces[ws_i].columns.len();
        if n_cols < 2 || ci == 0 || ci >= n_cols {
            return CommandReport::new(cmds);
        }
        let target = ci - 1;
        {
            let ws = &mut state.monitors[mi].workspaces[ws_i];
            // The collapsed column's width must be absorbed by the target, not
            // discarded. The `retain` below drops column `ci`, and
            // `rebalance_weights` only repairs weights <= 0 — it never
            // re-normalizes — so without this transfer the total column weight
            // drops by `columns[ci].weight` and the ribbon leaves a permanent
            // empty gap on the right of the workarea.
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
        let scroll = follow_scroll(
            &state.monitors[mi].workspaces[ws_i],
            cfg,
            state.monitors[mi].workarea,
            fs_of(state, mi, ws_i),
        );
        state.monitors[mi].workspaces[ws_i].camera.retarget(scroll);
        cmds.push(Effect::ArrangeMonitor(mi));
        CommandReport::new(cmds)
    }
}

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
        // Stale sel_mon after hotplug: clamp instead of panicking on
        // `monitors[cur]`.
        if cur >= n {
            state.sel_mon = 0;
            let to = state.best_focus(0);
            cmds.push(Effect::FocusWindow(to));
            return CommandReport::with_event(cmds, Event::FocusChanged { from: None, to });
        }
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
        if mi >= n {
            return CommandReport::new(cmds);
        }
        let win = self.0;
        let new_mi = match self.1 {
            Dir::Left | Dir::Prev => (mi + n - 1) % n,
            _ => (mi + 1) % n,
        };
        // The window must actually live on the source monitor: otherwise
        // `remove_window` on `mi` is a no-op while the `push` below duplicates
        // it onto the destination, so the client would appear in two workspace
        // slots at once.
        let (src_mon, src_ws) = match state.clients.get(&win) {
            Some(c) => (c.monitor, c.workspace),
            None => return CommandReport::new(cmds),
        };
        if src_mon != mi {
            return CommandReport::new(cmds);
        }
        let is_float = state
            .clients
            .get(&win)
            .is_some_and(crate::types::Client::is_float);
        let n_dst = state.monitors[new_mi].workspaces.len();
        // The source View must still exist on the source monitor; its id is what
        // the removal below is addressed by.
        let Some(src_ws_real) = state.monitors[mi].view_index(src_ws) else {
            return CommandReport::new(cmds);
        };
        if n_dst == 0 {
            return CommandReport::new(cmds);
        }
        // The real source position drives removal on the origin monitor; only the
        // insertion position on the destination is clamped, so a monitor with
        // fewer Views lands the window on its last one.
        let dst_ws = src_ws_real.min(n_dst - 1);
        // `remove_window` is a no-op if the tree didn't contain `win`
        // (stale client record): bail instead of duplicating below.
        let contained = state.monitors[mi].workspaces[src_ws_real]
            .columns
            .iter()
            .any(|c| c.windows.contains(&win))
            || state.monitors[mi].workspaces[src_ws_real]
                .floats
                .contains(&win);
        if !contained {
            return CommandReport::new(cmds);
        }
        state.monitors[mi].workspaces[src_ws_real].remove_window(win);
        if is_float {
            state.monitors[new_mi].workspaces[dst_ws].floats.push(win);
            // New workarea (a different monitor): the float rect is no longer a
            // fixed point of the destination projection. Re-settling it once here
            // avoids the visible jump on the destination's first arrange.
            // Single helper, see `layout::settle_float_in_workarea`.
            crate::core::layout::settle_float_in_workarea(state, new_mi, win);
        } else {
            state.monitors[new_mi].workspaces[dst_ws].add_tiled(win, cfg.column_width);
        }
        state.monitors[mi].focus_stack.retain(|&w| w != win);
        if state.monitors[mi].focused == Some(win) {
            state.monitors[mi].focused = state.monitors[mi].focus_stack.last().copied();
        }
        // The window may have owned the source workspace's maximize overlay;
        // clear the now-dangling `presented_maximize` (it must name a maximized
        // client on the active workspace). This must cover the source workspace
        // even when it is *not* the monitor's active one, since the stale entry
        // would otherwise trip the invariant later when it becomes active.
        if state.monitors[mi].workspaces[src_ws_real].presented_maximize == Some(win) {
            state.monitors[mi].workspaces[src_ws_real].presented_maximize = None;
        }
        // The window becomes the most recently focused one on its new monitor, so
        // it belongs at the top of that monitor's stack exactly once — the same
        // `retain`-then-`push` shape `focus_logical_on` uses. A bare `push`
        // would duplicate the entry when the destination stack already names this
        // window (a focus slot on the other monitor, a consumed deferral), and a
        // stack with duplicates breaks the "each client once" focus bookkeeping
        // the invariant checker asserts.
        state.monitors[new_mi].focus_stack.retain(|&w| w != win);
        state.monitors[new_mi].focus_stack.push(win);
        if let Some(c) = state.clients.get_mut(&win) {
            c.monitor = new_mi;
            // The destination monitor has its own `ViewId` space, so the new
            // membership is the destination View's identity — never the source
            // View's id, which would name an unrelated View there.
            c.workspace = state.monitors[new_mi].workspaces[dst_ws].id;
        }
        // Refresh the maximize-overlay owner on both the source (which just lost
        // the window) and destination (which just gained it) monitors so neither
        // keeps a stale `presented_maximize` reference.
        state.sync_presented_maximize(mi);
        state.sync_presented_maximize(new_mi);
        // Recenter the scroll camera on both the origin (which just lost a window)
        // and the destination (which just gained one) so neither monitor is left
        // with a stale camera that hides the focused column.
        let src_wa = state.monitors[mi].workarea;
        let src_scroll = ideal_scroll(
            &state.monitors[mi].workspaces[src_ws_real],
            cfg,
            src_wa,
            fs_of(state, mi, src_ws_real),
        );
        state.monitors[mi].workspaces[src_ws_real]
            .camera
            .retarget(src_scroll);
        let dst_wa = state.monitors[new_mi].workarea;
        let dst_scroll = ideal_scroll(
            &state.monitors[new_mi].workspaces[dst_ws],
            cfg,
            dst_wa,
            fs_of(state, new_mi, dst_ws),
        );
        state.monitors[new_mi].workspaces[dst_ws]
            .camera
            .retarget(dst_scroll);
        cmds.push(Effect::ArrangeMonitor(mi));
        cmds.push(Effect::ArrangeMonitor(new_mi));
        state.sel_mon = new_mi;
        // The focus is *requested*, not applied: the sink resolves the moved
        // window's own monitor — which is the monitor the user is now looking at
        // — and writes that monitor's focus slot. When the destination is the
        // deferral's monitor this takes the focus off a maximize owner, and a
        // maximize overlay is presented exactly while it holds the focus, so the
        // deferral queued behind it is the orphan `check_invariants` #8c
        // rejects. The engine's safety net necessarily runs before the sink
        // applies the request, so the command that requests the move resolves it
        // here. See the helper.
        drop_deferral_yielded_by_focus_move(state, Some(win));
        cmds.push(Effect::FocusWindow(Some(win)));
        CommandReport::with_event(cmds, Event::WindowMoved(win))
    }
}

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

/// Toggle the Overview mode on the active workspace: enter a fixed-scale
/// spatial-navigation viewport, or leave it again.
///
/// Overview is a **viewport** operation, not a client resize, and this command
/// is the whole of what entering does: it flips the View's flag, fixes the
/// entry scale **once** from the focused tile
/// ([`overview_entry_scale_for`]), clears the viewport-zoom axis it is
/// mutually exclusive with, and positions the camera. It never writes
/// *layout* geometry — the column weights, the world ribbon and a float's own
/// rect are untouched — but the scale it fixes is a real one, because without a
/// compositor the projected rectangles *are* what X11 draws: entering Overview
/// visibly shrinks the tiles and leaving it restores them. Navigation
/// afterwards pans the camera at that stored scale; the scale is not
/// re-derived while the mode is on, so the client count shapes the scrollable
/// content but never the scale of a step. Leaving clears the flag and the
/// stored scale, restoring the settled view exactly.
///
/// See [`overview_entry_scale_for`] for the sizing policy the entry applies,
/// and for why a window manager without a compositor cannot do better than
/// write the scale to the tile rectangles.
#[derive(Debug, Clone, Copy)]
pub struct ToggleOverview;

impl Command for ToggleOverview {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_index();
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        let enter = !state.monitors[mi].workspaces[ws_i].overview;
        if enter {
            // Fix the scale once, from the tile the viewport opens on. Read
            // after the flip: `fs_ctx` returns an empty descriptor for an
            // Overview workspace — non-exclusive fullscreen columns are
            // ordinary ribbon participants there — and both the entry scale
            // and the camera target have to be derived from the projection
            // that is about to run, not from the one that just ended.
            state.monitors[mi].workspaces[ws_i].overview = true;
            let fs = fs_of(state, mi, ws_i);
            let scale = crate::core::layout::overview_entry_scale_for(
                &state.monitors[mi].workspaces[ws_i],
                cfg,
                wa,
                &fs,
            );
            state.monitors[mi].workspaces[ws_i].overview_scale = scale;
        } else {
            state.monitors[mi].workspaces[ws_i].overview = false;
            state.monitors[mi].workspaces[ws_i].overview_scale = 1.0;
        }
        // Mutually exclusive with Viewport Zoom: toggling Overview must reset
        // the page-zoom state, or a lingering `Zoomed` mode would keep `alpha` on
        // `page_zoom` (and leave `overview` ignored) — making Overview a silent
        // no-op or leaving a zoom factor no reader expects.
        state.monitors[mi].workspaces[ws_i].viewport_mode = ViewportMode::Normal;
        state.monitors[mi].workspaces[ws_i].page_zoom = 1.0;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        let scroll = if layout == LayoutKind::Column {
            follow_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.retarget(scroll);
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
///
/// Navigation pans the viewport and never touches the scale: when this enters
/// the mode it fixes the entry scale exactly like [`ToggleOverview`]; when the
/// mode is already on the stored scale is left alone and only the focus and
/// the camera move. Repeated navigation therefore cannot shrink the workspace.
#[derive(Debug, Clone, Copy)]
pub struct OverviewNav(pub Dir);

impl Command for OverviewNav {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_index();
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        // Entering Overview (it may already be on — this doubles as "show the
        // viewport"). The entry scale is fixed only on the entering half:
        // re-deriving it here would shrink the workspace on every navigation
        // step, which is exactly the walk-down this mode exists to prevent.
        // Reading the fullscreen descriptor after the flip matters for the
        // reason `ToggleOverview` documents: `fs_ctx` demotes non-exclusive
        // fullscreen columns to ordinary ribbon participants under Overview,
        // and that is the projection the entry scale and the camera target
        // must follow.
        if !state.monitors[mi].workspaces[ws_i].overview {
            state.monitors[mi].workspaces[ws_i].overview = true;
            let fs = fs_of(state, mi, ws_i);
            let scale = crate::core::layout::overview_entry_scale_for(
                &state.monitors[mi].workspaces[ws_i],
                cfg,
                wa,
                &fs,
            );
            state.monitors[mi].workspaces[ws_i].overview_scale = scale;
        }
        let fs = fs_of(state, mi, ws_i);
        let n = state.monitors[mi].workspaces[ws_i].columns.len();
        if n == 0 {
            return CommandReport::new(cmds);
        }
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        let cur = ws.focus.column_idx.min(n - 1);
        let new = match self.0 {
            Dir::Left => cur.saturating_sub(1),
            Dir::Right => (cur + 1).min(n - 1),
            _ => cur,
        };
        ws.focus.column_idx = new;
        // Mutually exclusive with Viewport Zoom: entering Overview must reset
        // the page-zoom state or the zoom-out won't take effect.
        ws.viewport_mode = ViewportMode::Normal;
        ws.page_zoom = 1.0;
        let scroll = if layout == LayoutKind::Column {
            follow_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.retarget(scroll);
        cmds.push(Effect::ArrangeMonitor(mi));
        // Overview navigation must also move the real input focus to the window
        // we just selected, otherwise the keyboard keeps going to the previous
        // window and `ws.focus.column_idx` desyncs from `mon.focused`.
        if let Some(w) = ws.columns.get(new).and_then(Column::focused_win) {
            // That focus is only *requested* here (the X sink applies it), so
            // selecting a column away from the overlay that owns a deferral
            // has to resolve that deferral now; see the helper.
            drop_deferral_yielded_by_focus_move(state, Some(w));
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

/// Drop into the selected column: leave Overview, keeping the current selection
/// as the focused column. Clears the stored entry scale, so the settled view
/// projects at exactly `1.0` and no view state survives inside a client's
/// geometry.
#[derive(Debug, Clone, Copy)]
pub struct OverviewEnter;

impl Command for OverviewEnter {
    fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
        let mut cmds = Vec::new();
        let mi = state.sel_mon;
        if mi >= state.monitors.len() {
            return CommandReport::new(cmds);
        }
        let ws_i = state.monitors[mi].active_index();
        let layout = state.monitors[mi].workspaces[ws_i].layout;
        let wa = state.monitors[mi].workarea;
        let fs = fs_of(state, mi, ws_i);
        let ws = &mut state.monitors[mi].workspaces[ws_i];
        ws.overview = false;
        ws.overview_scale = 1.0;
        // Mutually exclusive with Viewport Zoom: leaving Overview must also
        // drop any pending viewport zoom so the state stays consistent.
        ws.viewport_mode = ViewportMode::Normal;
        ws.page_zoom = 1.0;
        let scroll = if layout == LayoutKind::Column {
            ideal_scroll(ws, cfg, wa, fs)
        } else {
            0.0
        };
        ws.camera.retarget(scroll);
        cmds.push(Effect::ArrangeMonitor(mi));
        // "Enter" drops into the selected column: move the real focus there too,
        // so the key window matches `ws.focus.column_idx`.
        if let Some(w) = ws
            .columns
            .get(ws.focus.column_idx)
            .and_then(Column::focused_win)
        {
            // Same as `OverviewNav`: this focus is requested, not applied, so a
            // deferral owned by the overlay this drops must be resolved here.
            drop_deferral_yielded_by_focus_move(state, Some(w));
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
