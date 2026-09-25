//! Composition policy — the single, authoritative decision of *how* Maverick
//! presents each monitor. One place so the WM, the X backend and the compositor
//! agree: window management (`State`/`Client`), rendering
//! (`compositor::Compositor`) and frame pacing (vsync / swap interval) all
//! consume the mode produced here instead of deciding for themselves.
//!
//! The policy answers one pure question:
//!
//! ```text
//! Window Management state (State)
//!     → CompositionPolicy
//!     → CompositionMode { Disabled, Compose, Bypass }
//! ```
//!
//! It MUST NOT touch X11, GLX, GL, `VSync` or the compositor's internal
//! resources. Those concerns live in `backend/x11/compositor.rs`, which
//! *consumes* the mode the policy produces.
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
//!   (see `compositor::Compositor::engage_bypass`). This reduces latency and
//!   overhead for fullscreen games/video without Maverick touching the app's own
//!   frame pacing (`VSync` is the application's concern, never the policy's).
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
//! desktop that still needs the compositor must never be bypassed, because
//! bypass leaves a transparent hole that only compositing can fill.

use crate::config::Cfg;
use crate::types::{Client, Monitor, State, StateExt, WindowId};

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
    #[allow(dead_code)]
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
///  * the client does not demand the compositor via `_NET_WM_BYPASS_COMPOSITOR=1`;
///  * it lives on this monitor and is shown;
///  * no other managed window on the monitor would be composited above it
///    (floating, dialog-like, or a transient popup);
///  * its geometry actually spans the monitor's screen.
///
/// `_cfg` is unused: the config gates live in `mode_for`, and every test needs
/// to call this predicate directly with bypass enabled to inspect the candidate.
pub fn bypass_candidate(_cfg: &Cfg, state: &State, mon_idx: usize) -> Option<WindowId> {
    let mon: &Monitor = state.monitors.get(mon_idx)?;

    // Collect the windows this monitor presents over its whole screen. The two
    // fullscreen sources are disjoint by construction — `fs_ctx` excludes
    // `FullscreenPolicy::True` windows, which only
    // `presented_overlay_owner` reports — so the union is at most one
    // fullscreen window. It can still hold a second entry: the maximize branch
    // of `presented_overlay_owner` names a merely maximized window, which
    // `is_fullscreen` below rejects.
    let mut candidates: Vec<WindowId> = Vec::with_capacity(2);
    if let Some(w) = state.presented_overlay_owner(mon_idx) {
        candidates.push(w);
    }
    if let Some(w) = state.covering_fullscreen_window(mon_idx) {
        candidates.push(w);
    }
    // More than one candidate is ambiguous — two windows cannot both own the
    // screen — so the monitor keeps compositing.
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
    // The two helpers only name genuine fullscreen windows, but a maximized
    // overlay owner or a flag lost between the helper call and here must never
    // bypass.
    if !client.is_fullscreen() {
        return None;
    }
    // Reject if any other managed window on the same monitor would need to be
    // composited above the fullscreen (floating overlay, dialog, transient, …).
    if occluding_window_present(state, mon_idx, win) {
        return None;
    }
    // Last line of defence: the window's own geometry must span the monitor's
    // screen, or the compositor's transparent hole would be left uncovered.
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

/// Whether `client`'s geometry (pixels) fully spans the monitor's screen. The
/// presentation helpers decide *which* window covers the screen, but a window
/// whose own `geom` does not span it is not a safe direct-presentation
/// candidate: the compositor's transparent hole would be left uncovered.
/// Over-sized is accepted (WM rounding, borders), zero-sized is not.
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

    /// Build a one-monitor state in `Column` layout with `n` tiled clients on
    /// the active workspace, the first focused. Returns the state plus the
    /// window ids created.
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

    /// Put `w` into a real fullscreen state: fullscreen flag, screen-sized
    /// geometry (the policy rejects partial windows) and top of the focus
    /// stack, which is what `presented_overlay_owner` searches.
    fn make_fullscreen(state: &mut State, w: WindowId) {
        let s = state.monitors[0].screen;
        let c = state.clients.get_mut(&w).unwrap();
        c.flags.set(WinFlags::FULLSCREEN);
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
        // A candidate exists; the `fullscreen_bypass = false` gate alone keeps
        // the mode at Compose.
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
        // Maximized-only must not bypass: `presented_overlay_owner` can name a
        // maximize owner, and only the `is_fullscreen` check rejects it.
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
        // wins[1] is a transient popup (e.g. a game launcher dialog) on the
        // same monitor.
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
        // Bypass is decided per output: the fullscreen game on monitor 0 does
        // not license bypassing the tiled window on monitor 1.
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

/// Property-based coverage of the composition-mode decision.
///
/// The example tests above pin individual eligibility clauses. These properties
/// instead state the *consequence* the policy owes the rest of the compositor —
/// a monitor is only ever told to step aside when nothing would be left
/// uncovered — and then push generated scenes at it, because the scene space
/// (how many outputs, which window is covering, what else sits on the monitor)
/// is far wider than any list of hand-written cases.
#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::types::{FullscreenPolicy, LayoutKind, Rect, WinFlags};
    use proptest::prelude::*;

    /// Window types the occlusion predicate treats as needing to be composited
    /// above a fullscreen window. Mirrors the literal list in
    /// `occluding_window_present`; a new type added there without a test update
    /// shows up as a failing property rather than silently widening the scene
    /// space.
    const DIALOG_LIKE: &[&str] = &[
        "dialog",
        "utility",
        "menu",
        "toolbar",
        "splash",
        "notification",
    ];

    /// A second client on the same monitor, described by the single property the
    /// occlusion predicate branches on.
    ///
    /// `HiddenFloat` and `UnmanagedFloat` are the control cases: they are shaped
    /// like an occluder but the predicate is documented to skip them, so an
    /// over-broad filter that vetoed them would starve real fullscreen games of
    /// the latency win they are bypassing for.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Extra {
        Absent,
        Float,
        HiddenFloat,
        UnmanagedFloat,
        DialogLike,
        Transient,
    }

    /// One generated output, together with everything the policy branches on for
    /// it. Fields are drawn independently so a single run covers scenes that no
    /// hand-written example would bother to build.
    #[derive(Debug, Clone)]
    struct MonitorSpec {
        screen: Rect,
        /// Install a screen-covering fullscreen window as this output's candidate.
        cover: bool,
        /// Keep the fullscreen flag but shrink the window off the screen.
        undersized: bool,
        /// WM-hidden candidate.
        hidden: bool,
        /// `_NET_WM_BYPASS_COMPOSITOR` wire value.
        hint: u32,
        /// Second client on this output.
        extra: Extra,
        /// Add a maximized-but-not-fullscreen window. `presented_overlay_owner`
        /// reports it as the overlay owner, and the independent `is_fullscreen`
        /// guard is what rejects it.
        max_only: bool,
        /// Add a `FullscreenPolicy::True` fullscreen overlay — the *other*
        /// candidate source. Unlike the ribbon tile this one is invisible to
        /// `fs_ctx`, so a scene carrying both produces two candidates and is the
        /// only way to reach the ambiguity guard on its own.
        true_overlay: bool,
        n_tags: usize,
    }

    /// A `Cfg` with the two policy gates set. Named apart from the `cfg` helper
    /// in the sibling `tests` module so the built-in `cfg!` macro stays readable
    /// at every call site.
    fn policy_cfg(enabled: bool, bypass: bool) -> Cfg {
        let mut c = Cfg::default();
        c.compositor.enabled = enabled;
        c.compositor.fullscreen_bypass = bypass;
        c
    }

    /// A monitor whose only covering fullscreen window is otherwise eligible, with
    /// every discretionary field pinned so a test can vary exactly one thing.
    fn covering(screen: Rect) -> MonitorSpec {
        MonitorSpec {
            screen,
            cover: true,
            undersized: false,
            hidden: false,
            hint: 0,
            extra: Extra::Absent,
            max_only: false,
            true_overlay: false,
            n_tags: 1,
        }
    }

    fn arb_extra() -> impl Strategy<Value = Extra> {
        prop_oneof![
            Just(Extra::Absent),
            Just(Extra::Float),
            Just(Extra::HiddenFloat),
            Just(Extra::UnmanagedFloat),
            Just(Extra::DialogLike),
            Just(Extra::Transient),
        ]
    }

    /// Output geometry drawn across the whole plausible range rather than a few
    /// display sizes: the coverage arithmetic has to hold for a 1x1 output and an
    /// 8K one alike, and only a wide range proves the clamps are not tuned to
    /// conventional resolutions.
    fn arb_screen() -> impl Strategy<Value = Rect> {
        (0i32..8192, 0i32..4320, 1u32..8192, 1u32..4320)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    fn arb_monitor() -> impl Strategy<Value = MonitorSpec> {
        (
            arb_screen(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0u32..=u32::MAX,
            arb_extra(),
            any::<bool>(),
            any::<bool>(),
            1usize..=4,
        )
            .prop_map(
                |(
                    screen,
                    cover,
                    undersized,
                    hidden,
                    hint,
                    extra,
                    max_only,
                    true_overlay,
                    n_tags,
                )| MonitorSpec {
                    screen,
                    cover,
                    undersized,
                    hidden,
                    hint,
                    extra,
                    max_only,
                    true_overlay,
                    n_tags,
                },
            )
    }

    /// Materialise a generated scene into a `State`, returning the screen-covering
    /// fullscreen window of each output in `specs` order (`None` where the spec
    /// installs no candidate).
    ///
    /// Windows are placed through the same public API the WM itself drives
    /// (`add_client` plus `add_tiled`, and `Workspace::floats` for floats) instead
    /// of poking workspace internals, so a generated scene exercises the same
    /// lookup path as a real one.
    ///
    /// Placement order is load-bearing. `add_tiled` inserts the new column to the
    /// right of the focused one *and moves the focus onto it*, and `fs_ctx` only
    /// reports a covering fullscreen window from the focused column. The covering
    /// window is therefore added last, and float-ish clients go to `floats`,
    /// which the column ribbon never sees. Adding a neighbour after the covering
    /// window would silently un-focus it and make every bypassable scene look
    /// ambiguous.
    fn build(specs: &[MonitorSpec]) -> (State, Vec<Option<WindowId>>) {
        let mut state = State::new();
        let mut next: WindowId = 1;
        let mut cover_ids = Vec::with_capacity(specs.len());
        for (mon_idx, spec) in specs.iter().enumerate() {
            let mut mon = Monitor::new(spec.screen, spec.n_tags);
            mon.workspaces[0].layout = LayoutKind::Column;

            // Every output keeps one ordinary tiled client so no generated scene is
            // empty; that keeps "no candidate at all" from being the only outcome
            // the properties can observe.
            let base = next;
            next += 1;
            let mut c = Client::new(base, mon_idx, 0);
            c.geom = Rect::new(spec.screen.x, spec.screen.y, 64, 64);
            state.add_client(c);
            mon.workspaces[0].add_tiled(base, 0.5);
            mon.focus_stack.push(base);

            if spec.max_only {
                let w = next;
                next += 1;
                let mut c = Client::new(w, mon_idx, 0);
                c.geom = spec.screen;
                c.flags.set(WinFlags::MAXIMIZED_V);
                c.flags.set(WinFlags::MAXIMIZED_H);
                state.add_client(c);
                mon.workspaces[0].add_tiled(w, 0.5);
                mon.focus_stack.push(w);
                mon.workspaces[0].presented_maximize = Some(w);
            }

            if spec.true_overlay {
                // Policy `True` makes this the presented overlay, which is the
                // candidate source `fs_ctx` deliberately excludes — so installing
                // one alongside a ribbon tile yields two candidates.
                let w = next;
                next += 1;
                let mut c = Client::new(w, mon_idx, 0);
                c.geom = spec.screen;
                c.flags.set(WinFlags::FULLSCREEN);
                c.fullscreen_policy = FullscreenPolicy::True;
                state.add_client(c);
                mon.workspaces[0].add_tiled(w, 0.5);
                mon.focus_stack.push(w);
            }

            let mut cover_id = None;
            if spec.cover {
                let w = next;
                next += 1;
                let mut c = Client::new(w, mon_idx, 0);
                c.flags.set(WinFlags::FULLSCREEN);
                c.geom = if spec.undersized {
                    Rect::new(
                        spec.screen.x,
                        spec.screen.y,
                        (spec.screen.w / 2).max(1),
                        (spec.screen.h / 2).max(1),
                    )
                } else {
                    Rect::new(spec.screen.x, spec.screen.y, spec.screen.w, spec.screen.h)
                };
                c.wm_hidden = spec.hidden;
                // Wire value 0 is EWMH "auto / absent", so it is recorded as no
                // hint at all; every other value is stored verbatim, including the
                // out-of-range ones a non-conforming client can send.
                c.bypass_hint = match spec.hint {
                    0 => None,
                    other => Some(other),
                };
                state.add_client(c);
                mon.workspaces[0].add_tiled(w, 0.5);
                mon.focus_stack.push(w);
                mon.focused = Some(w);
                cover_id = Some(w);
            }

            if spec.extra != Extra::Absent {
                let w = next;
                next += 1;
                let mut c = Client::new(w, mon_idx, 0);
                c.geom = Rect::new(spec.screen.x, spec.screen.y, 200, 200);
                match spec.extra {
                    Extra::Absent => {}
                    Extra::Float | Extra::HiddenFloat => c.flags.set(WinFlags::FLOAT),
                    Extra::UnmanagedFloat => {
                        c.flags.set(WinFlags::FLOAT);
                        c.is_unmanaged = true;
                    }
                    Extra::DialogLike => {
                        c.window_types =
                            vec![DIALOG_LIKE[next as usize % DIALOG_LIKE.len()].to_string()];
                    }
                    Extra::Transient => c.transient_parent = cover_id,
                }
                c.wm_hidden = spec.extra == Extra::HiddenFloat;
                state.add_client(c);
                mon.workspaces[0].floats.push(w);
                mon.focus_stack.push(w);
            }

            state.monitors.push(mon);
            cover_ids.push(cover_id);
        }
        (state, cover_ids)
    }

    /// True when some other managed, shown client on `mon_idx` is one the
    /// predicate has to keep compositing above a bypassing fullscreen window.
    fn would_occlude(state: &State, mon_idx: usize, win: WindowId) -> bool {
        state.clients.iter().any(|(&other, c)| {
            other != win
                && c.monitor == mon_idx
                && !c.wm_hidden
                && !c.is_unmanaged
                && (c.is_float()
                    || c.transient_parent.is_some()
                    || c.window_types
                        .iter()
                        .any(|t| DIALOG_LIKE.contains(&t.as_str())))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// A bypass is only safe if the window Maverick steps aside for really
        /// does span the whole output and nothing else on that output would have
        /// to be composited over it. Any other scene leaves a transparent hole
        /// that only compositing can fill, so the policy must never report
        /// `Bypass` for it — whichever combination of flags, geometry and
        /// neighbours produced it.
        #[test]
        fn bypass_is_never_chosen_when_something_would_be_left_uncovered(
            specs in prop::collection::vec(arb_monitor(), 1..=3),
        ) {
            let (state, _) = build(&specs);
            let cfg = policy_cfg(true, true);
            for idx in 0..specs.len() {
                if mode_for(&cfg, &state, idx) != CompositionMode::Bypass {
                    continue;
                }
                let mon = &state.monitors[idx];
                let win = bypass_candidate(&cfg, &state, idx)
                    .expect("Bypass implies a candidate");
                let c = state
                    .clients
                    .get(&win)
                    .expect("the candidate names a live client");
                prop_assert!(!c.wm_hidden, "hidden client {} was bypassed", win);
                prop_assert_eq!(c.monitor, idx);
                prop_assert!(c.is_fullscreen(), "client {} is not fullscreen", win);
                prop_assert!(c.geom.w > 0 && c.geom.h > 0, "client {} is empty", win);
                prop_assert!(
                    c.geom.x <= mon.screen.x && c.geom.y <= mon.screen.y,
                    "client {} does not start at the output origin", win
                );
                prop_assert!(
                    c.geom.w >= mon.screen.w && c.geom.h >= mon.screen.h,
                    "client {} is smaller than the output", win
                );
                prop_assert!(
                    !would_occlude(&state, idx, win),
                    "a neighbour would have to be composited above bypassed client {}",
                    win
                );
            }
        }

        /// Nothing may silently re-enable compositing for an output the user
        /// turned it off on — not even a scene that would otherwise bypass, and
        /// not even for an output index the backend does not know.
        #[test]
        fn a_disabled_compositor_is_never_composed(
            bypass_gate in any::<bool>(),
            specs in prop::collection::vec(arb_monitor(), 1..=3),
        ) {
            let (state, _) = build(&specs);
            let cfg = policy_cfg(false, bypass_gate);
            for idx in 0..=specs.len() {
                prop_assert_eq!(mode_for(&cfg, &state, idx), CompositionMode::Disabled);
            }
        }

        /// `fullscreen_bypass = false` is a user decision that no scene may
        /// override, so the mode stays `Compose` even where a candidate exists.
        #[test]
        fn the_bypass_gate_is_never_overridden_by_a_scene(
            specs in prop::collection::vec(arb_monitor(), 1..=3),
        ) {
            let (state, _) = build(&specs);
            let cfg = policy_cfg(true, false);
            for idx in 0..specs.len() {
                prop_assert_eq!(mode_for(&cfg, &state, idx), CompositionMode::Compose);
            }
        }

        /// The policy is asked about output indices the backend may not know about
        /// yet — a hotplugged screen, a stale `sel_mon`. It must answer `Compose`
        /// rather than invent a bypass for an output it cannot see.
        #[test]
        fn an_unknown_output_never_bypasses(specs in prop::collection::vec(arb_monitor(), 1..=3)) {
            let (state, _) = build(&specs);
            let cfg = policy_cfg(true, true);
            for idx in specs.len()..specs.len() + 3 {
                prop_assert_eq!(mode_for(&cfg, &state, idx), CompositionMode::Compose);
                prop_assert!(bypass_candidate(&cfg, &state, idx).is_none());
            }
        }

        /// Each shape the occlusion predicate is documented to reject really does
        /// forbid bypass: a second compositable client on the output is exactly
        /// the scene a transparent hole would ruin.
        #[test]
        fn a_second_compositable_client_forbids_bypass(
            extra in prop_oneof![
                Just(Extra::Float),
                Just(Extra::DialogLike),
                Just(Extra::Transient),
            ],
            screen in arb_screen(),
        ) {
            let mut spec = covering(screen);
            spec.extra = extra;
            let (state, cover) = build(std::slice::from_ref(&spec));
            let win = cover[0].expect("covering monitor reports its window");
            let cfg = policy_cfg(true, true);
            prop_assert!(would_occlude(&state, 0, win));
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
        }

        /// …and the three shapes it is documented to skip must still bypass.
        /// Over-broad filtering would not be a correctness bug, but it would
        /// silently cost every fullscreen game the latency this mode exists to
        /// reclaim, so the filter is pinned from both sides.
        #[test]
        fn a_skipped_neighbour_still_allows_bypass(
            extra in prop_oneof![
                Just(Extra::Absent),
                Just(Extra::HiddenFloat),
                Just(Extra::UnmanagedFloat),
            ],
            screen in arb_screen(),
        ) {
            let mut spec = covering(screen);
            spec.extra = extra;
            let (state, cover) = build(std::slice::from_ref(&spec));
            let win = cover[0].expect("covering monitor reports its window");
            let cfg = policy_cfg(true, true);
            prop_assert!(!would_occlude(&state, 0, win));
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), Some(win));
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Bypass);
        }

        /// A fullscreen flag without a screen-sized rect is not a safe
        /// direct-presentation candidate: the compositor's hole would stay open.
        /// The generated geometry is derived from the output so the window is
        /// always *nearly* right and the shortfall is the only thing under test.
        #[test]
        fn a_covering_window_that_does_not_span_the_output_is_not_a_candidate(
            screen in arb_screen(),
        ) {
            let mut spec = covering(screen);
            spec.undersized = true;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
        }

        /// A hidden candidate is not shown, so presenting it directly would leave
        /// the output blank.
        #[test]
        fn a_hidden_candidate_is_not_a_candidate(screen in arb_screen()) {
            let mut spec = covering(screen);
            spec.hidden = true;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
        }

        /// `_NET_WM_BYPASS_COMPOSITOR = 1` is the client demanding the compositor.
        /// Only that exact value is a veto: 0 (auto/absent), 2 (force bypass) and
        /// the out-of-range values a non-conforming client can send must all be
        /// read as "no opinion", or a stray hint would disable the optimization
        /// for every app that happens to send one.
        #[test]
        fn only_the_exact_compositor_demand_vetoes_bypass(
            hint in 0u32..=u32::MAX,
            screen in arb_screen(),
        ) {
            let mut spec = covering(screen);
            spec.hint = hint;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            if hint == 1 {
                prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
                prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
            } else {
                prop_assert!(
                    bypass_candidate(&cfg, &state, 0).is_some(),
                    "bypass hint {} was read as a veto",
                    hint
                );
                prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Bypass);
            }
        }

        /// Maximize fills the workarea but is not fullscreen: the ribbon around it
        /// still has to be drawn. Generating degenerate outputs matters here,
        /// because on a 1x1 screen the geometry checks are trivially satisfiable
        /// and the flag distinction is the only thing left holding the property.
        #[test]
        fn a_maximized_window_never_bypasses(screen in arb_screen()) {
            let mut spec = covering(screen);
            spec.cover = false;
            spec.max_only = true;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
        }

        /// A `FullscreenPolicy::True` overlay and a `Normal` ribbon tile both
        /// cover the screen, and neither is visible to the other lookup, so the
        /// policy sees two candidates where it would need one. Two windows cannot
        /// both own the output, so the ambiguity guard has to refuse.
        ///
        /// This is the only scene that reaches that guard on its own: pairing a
        /// ribbon tile with a *maximized* owner also produces two candidates, but
        /// the independent `is_fullscreen` check rejects the maximize owner
        /// anyway, so such a scene would pass even with the guard removed.
        #[test]
        fn two_covering_fullscreen_windows_never_bypass(screen in arb_screen()) {
            let mut spec = covering(screen);
            spec.true_overlay = true;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            prop_assert_eq!(bypass_candidate(&cfg, &state, 0), None);
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Compose);
        }

        /// A lone `FullscreenPolicy::True` overlay is unambiguous and does cover
        /// the output, so it must still bypass. Pinning the guard from this side
        /// matters: treating any overlay as ambiguous would quietly cost every
        /// exclusive-fullscreen app the bypass.
        #[test]
        fn a_lone_true_policy_overlay_still_bypasses(screen in arb_screen()) {
            let mut spec = covering(screen);
            spec.cover = false;
            spec.true_overlay = true;
            let (state, _) = build(std::slice::from_ref(&spec));
            let cfg = policy_cfg(true, true);
            prop_assert!(bypass_candidate(&cfg, &state, 0).is_some());
            prop_assert_eq!(mode_for(&cfg, &state, 0), CompositionMode::Bypass);
        }

        /// Bypass is decided per output, so whatever is happening on a
        /// neighbouring screen must not reach across and change — or enable —
        /// this one's mode. Two states differing only in the neighbour's whole
        /// scene have to agree here.
        #[test]
        fn a_neighbouring_output_cannot_change_this_ones_mode(
            screen in arb_screen(),
            left_neighbour in arb_monitor(),
            right_neighbour in arb_monitor(),
        ) {
            let cfg = policy_cfg(true, true);
            let (left, _) = build(&[covering(screen), left_neighbour]);
            let (right, _) = build(&[covering(screen), right_neighbour]);
            prop_assert_eq!(mode_for(&cfg, &left, 0), CompositionMode::Bypass);
            prop_assert_eq!(mode_for(&cfg, &right, 0), CompositionMode::Bypass);
        }
    }
}
