//! Composition policy — the single, authoritative decision of *how* Maverick
//! presents each monitor, kept strictly separate from window management
//! (`State`/`Client`), rendering (`compositor::Compositor`) and frame pacing
//! (vsync / swap interval).
//!
//! The policy answers one pure question:
//!
//! ```text
//! Window Management state (State)
//!     → CompositionPolicy
//!     → CompositionMode { Disabled, Compose, Bypass }
//! ```
//!
//! It MUST NOT touch X11, GLX, GL, `VSync` or the compositor's internal resources.
//! Those concerns live in `backend/x11/compositor.rs`, which *consumes* the
//! mode the policy produces.
//!
//! # Modes
//!
//! * `Disabled` — the user turned the compositor off (`[compositor].enabled =
//!   false`). The WM runs on the classic `ConfigureWindow` path; Maverick never
//!   owns `_NET_WM_CM_S0` and never redirects subwindows. Nothing may silently
//!   re-enable it because a window went fullscreen.
//! * `Compose` — Maverick composites normally: tiled, floating, multiple
//!   windows, overlays, transparency. The default.
//! * `Bypass` — the compositor is configured, but for *this output* it steps
//!   aside and lets one eligible fullscreen window present itself directly
//!   (see `compositor::Compositor::engage_bypass`). This reduces latency/overhead
//!   for fullscreen games/video without Maverick touching the app's own frame
//!   pacing (`VSync` is the application's concern, never the policy's).
//!
//! # Eligibility
//!
//! A monitor bypasses only when ALL of these hold:
//!
//! * the compositor is enabled and `fullscreen_bypass` is on;
//! * there is exactly ONE fullscreen window on the monitor that actually covers
//!   the screen (Grid/True overlay owner, or a Column covering fullscreen);
//! * that window is mapped and not hidden;
//! * no other managed window on the same monitor would need compositing *above*
//!   it — i.e. no floating window, no dialog/utility/menu/toolbar/splash/
//!   notification, and no transient popup.
//!
//! Any other scene (maximized-only, multiple windows, a dialog over the game, a
//! floating overlay) keeps `Compose`. This is deliberately conservative: a
//! desktop that still needs the compositor must never be bypassed.

use crate::config::Cfg;
use crate::types::{Client, Monitor, State, WindowId};

/// The composition mode for one output, as decided by the [`CompositionPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionMode {
    /// Compositor disabled by config — never composed.
    Disabled,
    /// Normal compositing.
    Compose,
    /// One eligible fullscreen window presents directly; Maverick steps aside.
    Bypass,
}

impl CompositionMode {
    /// Stable, human-readable tag (used by the opt-in composition trace).
    pub fn as_str(self) -> &'static str {
        match self {
            CompositionMode::Disabled => "Disabled",
            CompositionMode::Compose => "Compose",
            CompositionMode::Bypass => "Bypass",
        }
    }
}

/// Decide the composition mode for `mon_idx`.
///
/// Pure: depends only on `cfg` and `state`, never on X11/GL/runtime. The
/// `Disabled` arm short-circuits everything because when the compositor is off
/// no mode (not even `Bypass`) can exist.
pub fn mode_for(cfg: &Cfg, state: &State, mon_idx: usize) -> CompositionMode {
    if !cfg.compositor.enabled {
        return CompositionMode::Disabled;
    }
    if cfg.compositor.fullscreen_bypass && bypass_candidate(cfg, state, mon_idx).is_some() {
        return CompositionMode::Bypass;
    }
    CompositionMode::Compose
}

/// The single eligible fullscreen window that may be presented directly on
/// `mon_idx`, or `None` if the monitor must keep compositing.
///
/// Eligibility is the conjunction of:
///  * exactly one screen-covering fullscreen window exists on the monitor
///    (`presented_overlay_owner` for Grid/True, or `covering_fullscreen_window`
///    for the Column ribbon tile);
///  * it is mapped and not hidden;
///  * no other managed window on the monitor would be composited above it
///    (floating, dialog-like, or a transient popup).
pub fn bypass_candidate(_cfg: &Cfg, state: &State, mon_idx: usize) -> Option<WindowId> {
    let mon: &Monitor = state.monitors.get(mon_idx)?;

    // Collect every window that currently covers the whole monitor as a
    // fullscreen. These two helpers are disjoint (see `State`), so the union is
    // at most one window per monitor.
    let mut candidates: Vec<WindowId> = Vec::with_capacity(2);
    if let Some(w) = state.presented_overlay_owner(mon_idx) {
        candidates.push(w);
    }
    if let Some(w) = state.covering_fullscreen_window(mon_idx) {
        candidates.push(w);
    }
    // Exactly one fullscreen may cover the screen; anything else is ambiguous
    // and must stay composed.
    if candidates.len() != 1 {
        return None;
    }
    let win = candidates[0];

    let client: &Client = state.clients.get(&win)?;
    // EWMH `_NET_WM_BYPASS_COMPOSITOR` hint: 1=force compositor ON → never bypass,
    // 2=force bypass (when otherwise eligible), 0/None=auto.
    if client.bypass_hint == Some(1) {
        return None;
    }
    // The covering window must live on this monitor and be shown.
    if client.monitor != mon_idx || client.wm_hidden {
        return None;
    }
    // `presented_overlay_owner` / `covering_fullscreen_window` only name windows
    // that are genuinely fullscreen, but be defensive: a window that lost its
    // fullscreen flag between the helper call and here must not bypass.
    if !client.is_fullscreen() {
        return None;
    }
    // Reject if any other managed window on the same monitor would need to be
    // composited above the fullscreen (floating overlay, dialog, transient, …).
    if occluding_window_present(state, mon_idx, win) {
        return None;
    }
    // Sanity: the covering window's own geometry must actually span the
    // monitor's screen, or the compositor's transparent hole would be left
    // uncovered.
    if !covers_screen(client, mon) {
        return None;
    }
    Some(win)
}

/// True when some managed window other than `win` on `mon_idx` is a floating
/// window, a dialog-like window type, or a transient popup — any of which would
/// have to be composited above the fullscreen and therefore forbids bypass.
fn occluding_window_present(state: &State, mon_idx: usize, win: WindowId) -> bool {
    for (&id, c) in &state.clients {
        if id == win {
            continue;
        }
        if c.monitor != mon_idx || c.wm_hidden || c.is_unmanaged {
            continue;
        }
        if c.is_float() {
            return true;
        }
        if c.window_types.iter().any(|t| {
            matches!(
                t.as_str(),
                "dialog" | "utility" | "menu" | "toolbar" | "splash" | "notification"
            )
        }) {
            return true;
        }
        if c.transient_parent.is_some() {
            return true;
        }
    }
    false
}

/// Best-effort check that `client` actually fills the monitor's screen. We trust
/// the presentation layer (`presented_overlay_owner` / `covering_fullscreen_window`)
/// for *which* window covers the screen, but a window whose own `geom` does not
/// span the screen is not a safe direct-presentation candidate (a partial
/// window would leave the compositor's transparent hole uncovered).
fn covers_screen(client: &Client, mon: &Monitor) -> bool {
    let r = client.geom;
    r.x <= mon.screen.x
        && r.y <= mon.screen.y
        && r.w >= mon.screen.w
        && r.h >= mon.screen.h
        && r.w > 0
        && r.h > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LayoutKind, Rect, WinFlags};

    /// Build a one-monitor state in `Grid` layout with `n` tiled clients added
    /// to the active workspace, the first focused. Returns the state plus the
    /// list of window ids created.
    fn setup(n: usize) -> (State, Vec<WindowId>) {
        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        mon.workspaces[0].layout = LayoutKind::Column;
        let mut wins = Vec::new();
        for i in 0..n {
            let w: WindowId = (i + 1) as u32;
            let mut c = Client::new(w, 0, 0);
            c.geom = Rect::new(0, 0, 100, 100);
            c.workspace = 0;
            state.add_client(c);
            mon.workspaces[0].add_tiled(w, 0.5);
            mon.focus_stack.push(w);
            wins.push(w);
        }
        state.monitors.push(mon);
        (state, wins)
    }

    fn make_fullscreen(state: &mut State, w: WindowId) {
        let s = state.monitors[0].screen;
        let c = state.clients.get_mut(&w).unwrap();
        c.flags.set(WinFlags::FULLSCREEN);
        // A genuine fullscreen covers the whole monitor (the WM sets this when
        // entering fullscreen); the policy rejects partial windows.
        c.geom = Rect::new(s.x, s.y, s.w, s.h);
        state.monitors[0].focused = Some(w);
        state.monitors[0].focus_stack.retain(|&x| x != w);
        state.monitors[0].focus_stack.push(w);
    }

    fn cfg(enabled: bool, bypass: bool) -> Cfg {
        let mut c = Cfg::default();
        c.compositor.enabled = enabled;
        c.compositor.fullscreen_bypass = bypass;
        c
    }

    #[test]
    fn disabled_is_disabled_regardless_of_state() {
        let (mut state, wins) = setup(1);
        make_fullscreen(&mut state, wins[0]);
        assert_eq!(
            mode_for(&cfg(false, true), &state, 0),
            CompositionMode::Disabled
        );
        // Even with a fullscreen candidate, a disabled compositor never bypasses.
        assert!(bypass_candidate(&cfg(false, true), &state, 0).is_some());
    }

    #[test]
    fn enabled_normal_is_compose() {
        let (state, _wins) = setup(2);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
    }

    #[test]
    fn enabled_fullscreen_bypass_true_is_bypass() {
        let (mut state, wins) = setup(1);
        make_fullscreen(&mut state, wins[0]);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Bypass
        );
        assert_eq!(bypass_candidate(&cfg(true, true), &state, 0), Some(wins[0]));
    }

    #[test]
    fn enabled_fullscreen_bypass_false_is_compose() {
        let (mut state, wins) = setup(1);
        make_fullscreen(&mut state, wins[0]);
        assert_eq!(
            mode_for(&cfg(true, false), &state, 0),
            CompositionMode::Compose
        );
        // Candidate exists, but bypass is off, so the mode is Compose.
        assert!(bypass_candidate(&cfg(true, false), &state, 0).is_some());
    }

    #[test]
    fn fullscreen_with_floating_overlay_is_compose() {
        let (mut state, wins) = setup(2);
        make_fullscreen(&mut state, wins[0]);
        // wins[1] becomes a floating window on the same monitor.
        state
            .clients
            .get_mut(&wins[1])
            .unwrap()
            .flags
            .set(WinFlags::FLOAT);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
        assert!(bypass_candidate(&cfg(true, true), &state, 0).is_none());
    }

    #[test]
    fn fullscreen_with_visible_dialog_is_compose() {
        let (mut state, wins) = setup(2);
        make_fullscreen(&mut state, wins[0]);
        state.clients.get_mut(&wins[1]).unwrap().window_types = vec!["dialog".to_string()];
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
        assert!(bypass_candidate(&cfg(true, true), &state, 0).is_none());
    }

    #[test]
    fn maximized_non_fullscreen_is_compose() {
        let (mut state, wins) = setup(1);
        // Maximized only — must NOT bypass (only fullscreen may).
        state
            .clients
            .get_mut(&wins[0])
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        state.monitors[0].focused = Some(wins[0]);
        state.monitors[0].workspaces[0].presented_maximize = Some(wins[0]);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
        assert!(bypass_candidate(&cfg(true, true), &state, 0).is_none());
    }

    #[test]
    fn multiple_visible_windows_is_compose() {
        // Two normal windows, none fullscreen → no candidate → Compose.
        let (state, _wins) = setup(3);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
    }

    #[test]
    fn transient_popup_forbids_bypass() {
        let (mut state, wins) = setup(2);
        make_fullscreen(&mut state, wins[0]);
        // wins[1] is a transient popup (e.g. a game launcher dialog) on the same
        // monitor.
        state.clients.get_mut(&wins[1]).unwrap().transient_parent = Some(wins[0]);
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
        assert!(bypass_candidate(&cfg(true, true), &state, 0).is_none());
    }

    #[test]
    fn hidden_fullscreen_is_not_a_candidate() {
        let (mut state, wins) = setup(1);
        make_fullscreen(&mut state, wins[0]);
        state.clients.get_mut(&wins[0]).unwrap().wm_hidden = true;
        assert_eq!(
            mode_for(&cfg(true, true), &state, 0),
            CompositionMode::Compose
        );
        assert!(bypass_candidate(&cfg(true, true), &state, 0).is_none());
    }

    #[test]
    fn per_monitor_independence() {
        // Monitor 0: lone fullscreen game → Bypass. Monitor 1: Firefox → Compose.
        let mut state = State::new();
        let mut mon0 = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon0.workarea = Rect::new(0, 0, 800, 600);
        mon0.workspaces[0].layout = LayoutKind::Column;
        let mut mon1 = Monitor::new(Rect::new(800, 0, 800, 600), 1);
        mon1.workarea = Rect::new(800, 0, 800, 600);
        mon1.workspaces[0].layout = LayoutKind::Column;

        let game: WindowId = 1;
        let mut gc = Client::new(game, 0, 0);
        gc.geom = Rect::new(0, 0, 800, 600);
        gc.workspace = 0;
        state.add_client(gc);
        mon0.workspaces[0].add_tiled(game, 0.5);
        mon0.focus_stack.push(game);

        let ff: WindowId = 2;
        let mut fc = Client::new(ff, 1, 0);
        fc.geom = Rect::new(800, 0, 400, 300);
        fc.workspace = 0;
        state.add_client(fc);
        mon1.workspaces[0].add_tiled(ff, 0.5);
        mon1.focus_stack.push(ff);

        state.monitors.push(mon0);
        state.monitors.push(mon1);

        make_fullscreen(&mut state, game);

        let c = cfg(true, true);
        assert_eq!(mode_for(&c, &state, 0), CompositionMode::Bypass);
        assert_eq!(mode_for(&c, &state, 1), CompositionMode::Compose);
    }
}
