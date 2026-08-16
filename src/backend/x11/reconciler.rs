// maverick/src/backend/x11/reconciler.rs
//
// The single owner of "what geometry/stack has actually been written to X11".
//
// Before this module, geometry writes were scattered across `render`, `manage`
// and `events`, each re-deriving "has this changed?" with its own heuristic
// (plan 1786564084575, Fase 1, gap #1: "múltiples dueños de configure_window").
// The `Reconciler` keeps one `AppliedState` — the last *applied* rect/border per
// window — and diffs every *desired* placement against it, emitting only the
// `configure_window` calls that actually changed.
//
// Crucially the diff reproduces the old `apply_geom` skip rule: an unchanged
// rect+border is NOT re-emitted, so a busy `arrange` (once per animating monitor
// per frame) does not spam the X server with identical reconfigures (the exact
// thing the `geometry_dirty` flag and the `geom == client.geom` comparison were
// guarding). A pending state transition (`geometry_dirty`) still forces the
// reconfigure even when the rect is identical — borders/state changed without a
// geometry change.

use crate::core::desired::DesiredState;
use crate::types::{Client, Rect, State, WindowId};

// ── window-trace instrumentation (feature `window-trace`) ─────────────────────
// Observability-only macro for the reconcile/desired→applied pipeline. No-op
// unless `window-trace` is enabled. Fase 8.
#[cfg(feature = "window-trace")]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "window-trace"))]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{}};
}

/// One window's last *applied* (written to X11) geometry + border state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AppliedWindow {
    pub rect: Rect,
    pub border_w: u32,
    /// False until the first configure has been applied. A freshly-mapped
    /// window has nothing applied yet, so the first diff always emits.
    pub seen: bool,
}

/// The full set of windows the `Reconciler` believes X11 currently shows.
#[derive(Debug, Default)]
pub struct AppliedState {
    pub windows: std::collections::HashMap<WindowId, AppliedWindow>,
}

impl AppliedState {
    /// Diff the *desired* placement against what was last applied.
    ///
    /// Returns `Some((rect, bw))` when a reconfigure must be emitted, `None`
    /// when the desired state already matches the applied one (and no policy
    /// flag forces a re-emit). `geometry_dirty` mirrors the old
    /// `Client::geometry_dirty` semantics: a pending transition (fullscreen /
    /// maximize on/off) forces emission even when the rect is identical.
    pub fn diff(
        &mut self,
        win: WindowId,
        desired_rect: Rect,
        desired_bw: u32,
        geometry_dirty: bool,
    ) -> Option<(Rect, u32)> {
        let prev = self.windows.entry(win).or_default();
        let changed = geometry_dirty
            || !prev.seen
            || prev.rect != desired_rect
            || prev.border_w != desired_bw;
        if changed {
            prev.rect = desired_rect;
            prev.border_w = desired_bw;
            prev.seen = true;
            Some((desired_rect, desired_bw))
        } else {
            None
        }
    }

    /// Forget a destroyed / unmanaged window so its next appearance re-emits a
    /// full configure (its old applied rect is no longer valid).
    pub fn forget(&mut self, win: WindowId) {
        self.windows.remove(&win);
    }
}

/// A geometry operation the backend must apply to X11 to make it match Desired.
pub enum GeometryEffect {
    Configure {
        win: WindowId,
        rect: Rect,
        border: u32,
    },
}

/// Diff the explicit `Desired` against the recorded `Applied` and produce the X11 geometry
/// effects required to make X11 match Desired.
///
/// Pure with respect to logical state: it reads `client.geometry_dirty` only as an input
/// flag and mutates ONLY `applied` (the last-X11-geometry record). It must never modify
/// `State` logical geometry, decide layout, or read X11 events.
pub fn reconcile(
    desired: &DesiredState,
    state: &State,
    applied: &mut AppliedState,
) -> Vec<GeometryEffect> {
    let mut out = Vec::new();
    for dw in &desired.windows {
        let dirty = state
            .clients
            .get(&dw.window)
            .is_some_and(|c| c.geometry_dirty);
        if let Some((rect, bw)) = applied.diff(dw.window, dw.rect, dw.border, dirty) {
            out.push(GeometryEffect::Configure {
                win: dw.window,
                rect,
                border: bw,
            });
        }
    }
    #[cfg(feature = "window-trace")]
    wtrace!(
        "reconcile desired={} effects={} applied_total={}",
        desired.windows.len(),
        out.len(),
        applied.windows.len()
    );
    out
}

/// The `Reconciler` keeps three distinct geometries straight:
///
/// * **Desired** — `crate::core::desired::DesiredState`, the single explicit
///   desired representation produced purely by `layout::arrange` +
///   `present::present_into` (via `DesiredState::from_placements`) and diffed
///   here against `AppliedState`. `client.geom` is the *desired logical*
///   geometry the core wants; `DesiredState` is the explicit snapshot of every
///   window's desired rect/border/stacking for one arrange cycle.
/// * **Applied** — `AppliedState` (this module), the last rect/border actually
///   written to X11, so unchanged placements are never re-emitted. `Desired`
///   and `Applied` are independent records; the `Reconciler` never writes
///   `Desired` — it only reads `Desired` and mutates `Applied`.
/// * **X11 Real** — what the client *actually* shows, observed externally via
///   `ConfigureNotify`. This is deliberately NOT trusted as state: a self-
///   resizing client (Firefox, Wine, a game) reports a geometry that diverges
///   from `Applied`, and we use that divergence only to decide whether to
///   re-assert `Desired` (WM authority) or to adopt it (a float).
///
/// `diff` takes the *desired* `(win, rect, border_w)` and the *applied*
/// `AppliedWindow` and returns the configure only when they differ.
///
/// The verdict of comparing an external `ConfigureNotify` against `Applied`.
/// `Desired` (`client.geom`) is the third side: when we re-assert we emit
/// `Desired`, never the client's reported rect, so the WM stays the authority
/// for tiled/fullscreen windows.
pub enum ConfigureObservation {
    /// Reported geometry equals what we last applied: the echo of our own
    /// `configure_window`, or a compliant client. Nothing to do.
    Compliant,
    /// The client moved on its own (`X11 Real != Applied`). `follow` says
    /// whether the WM yields to it:
    ///   * `false` — the WM is the authority (tiled/fullscreen): re-emit
    ///     `Desired` so the client snaps back to where the WM put it.
    ///   * `true`  — the window is allowed external geometry (a float): adopt
    ///     the reported rect into the model instead of fighting it.
    Diverged { follow: bool },
}

/// Classify an external `ConfigureNotify` for a managed window. Pure: it only
/// reads `AppliedState` and `Client`, so the convergence policy is unit-tested
/// without an X server. The caller acts on the verdict (see `on_configure_notify`).
pub(crate) fn classify_configure(
    reported_rect: Rect,
    reported_bw: u32,
    applied: &AppliedWindow,
    client: &Client,
) -> ConfigureObservation {
    if applied.rect == reported_rect && applied.border_w == reported_bw {
        return ConfigureObservation::Compliant;
    }
    // Diverged. Floats may size themselves; tiled/fullscreen are WM-owned.
    let follow = client.is_float() && !client.is_fullscreen();
    ConfigureObservation::Diverged { follow }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::desired::DesiredWindow;
    use crate::types::WinFlags;

    #[test]
    fn first_apply_always_emits() {
        let mut s = AppliedState::default();
        // A freshly-tracked window (not yet seen) must emit even an identical
        // desired rect — X11 has nothing applied for it yet.
        assert_eq!(
            s.diff(1, Rect::new(0, 0, 100, 100), 2, false),
            Some((Rect::new(0, 0, 100, 100), 2))
        );
        // Second identical diff must be a no-op (no configure_window storm).
        assert_eq!(s.diff(1, Rect::new(0, 0, 100, 100), 2, false), None);
    }

    #[test]
    fn changed_rect_emits_only_the_delta() {
        let mut s = AppliedState::default();
        s.diff(1, Rect::new(0, 0, 100, 100), 2, false);
        // Border-only change must re-emit.
        assert_eq!(
            s.diff(1, Rect::new(0, 0, 100, 100), 4, false),
            Some((Rect::new(0, 0, 100, 100), 4))
        );
        // Rect change must re-emit.
        assert_eq!(
            s.diff(1, Rect::new(10, 10, 120, 80), 4, false),
            Some((Rect::new(10, 10, 120, 80), 4))
        );
        // Re-diffing the now-applied rect must skip (no configure_window storm).
        assert_eq!(s.diff(1, Rect::new(10, 10, 120, 80), 4, false), None);
    }

    #[test]
    fn geometry_dirty_forces_emit_on_identical_rect() {
        let mut s = AppliedState::default();
        s.diff(1, Rect::new(0, 0, 100, 100), 2, false);
        // A pending transition with an identical rect must still emit.
        assert_eq!(
            s.diff(1, Rect::new(0, 0, 100, 100), 2, true),
            Some((Rect::new(0, 0, 100, 100), 2))
        );
        // And a subsequent dirty emit of the same rect again (still dirty) too.
        assert_eq!(
            s.diff(1, Rect::new(0, 0, 100, 100), 2, true),
            Some((Rect::new(0, 0, 100, 100), 2))
        );
    }

    #[test]
    fn forget_clears_applied() {
        let mut s = AppliedState::default();
        s.diff(1, Rect::new(0, 0, 100, 100), 2, false);
        s.forget(1);
        // After forget, the same rect re-emits (as if freshly mapped).
        assert_eq!(
            s.diff(1, Rect::new(0, 0, 100, 100), 2, false),
            Some((Rect::new(0, 0, 100, 100), 2))
        );
    }

    // ── Step 3: convergence — external ConfigureNotify vs Applied ──────────
    //
    // These encode the concrete desync scenarios the WM must survive: a client
    // that resizes itself (Firefox / Wine / a game) must NOT drag the model
    // with it. For tiled/fullscreen the WM is the authority and must re-assert;
    // for a float it may adopt the new geometry.

    /// A tiled 1000x800; the client attempts 400x300 → divergence the WM must
    /// re-assert (snap back), never follow.
    #[test]
    fn tiled_self_resize_is_authority_diverged() {
        let c = Client::new(1, 0, 0); // tiled: not a float
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let obs = classify_configure(Rect::new(0, 0, 400, 300), 2, &applied, &c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
            "tiled self-resize must be re-asserted, not followed"
        );
    }

    /// A fullscreen window; the client attempts a resize → must stay fullscreen
    /// (re-assert), even though the reported rect differs wildly.
    #[test]
    fn fullscreen_self_resize_is_authority_diverged() {
        let mut c = Client::new(1, 0, 0);
        c.flags.set(WinFlags::FULLSCREEN);
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1920, 1080),
            border_w: 0,
            seen: true,
        };
        let obs = classify_configure(Rect::new(40, 40, 640, 480), 0, &applied, &c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
            "fullscreen must re-assert and stay fullscreen"
        );
    }

    /// Our own configure echo (or a compliant client) reports exactly what we
    /// applied → must be ignored, not flagged as a divergence.
    #[test]
    fn compliant_echo_is_ignored() {
        let c = Client::new(1, 0, 0); // tiled
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let obs = classify_configure(Rect::new(0, 0, 1000, 800), 2, &applied, &c);
        assert!(
            matches!(obs, ConfigureObservation::Compliant),
            "matching geometry must be treated as our own echo"
        );
    }

    /// A float that resizes itself is allowed external geometry → the model
    /// follows (adopts the reported rect) rather than fighting it.
    #[test]
    fn float_self_resize_follows_model() {
        let mut c = Client::new(1, 0, 0);
        c.flags.set(WinFlags::FLOAT);
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let obs = classify_configure(Rect::new(0, 0, 400, 300), 2, &applied, &c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: true }),
            "a float may resize itself; the model should follow"
        );
    }

    /// A tiled + B tiled; B self-resizes. A must be Compliant (unaffected), B
    /// Diverged (re-assert). Per-window authority — focus and scroll untouched.
    #[test]
    fn ab_tiled_independent() {
        let a = Client::new(1, 0, 0); // tiled
        let b = Client::new(2, 0, 0); // tiled
        let applied_a = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let applied_b = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let obs_a = classify_configure(Rect::new(0, 0, 1000, 800), 2, &applied_a, &a);
        let obs_b = classify_configure(Rect::new(0, 0, 400, 300), 2, &applied_b, &b);
        assert!(
            matches!(obs_a, ConfigureObservation::Compliant),
            "A did not move → ignore"
        );
        assert!(
            matches!(obs_b, ConfigureObservation::Diverged { follow: false }),
            "B diverged → re-assert"
        );
    }

    /// A fullscreen + B fullscreen; A attempts a resize. A must re-assert, B
    /// must stay fullscreen (unaffected) — the two snapshots don't interfere.
    #[test]
    fn ab_fullscreen_independent() {
        let mut a = Client::new(1, 0, 0);
        a.flags.set(WinFlags::FULLSCREEN);
        let mut b = Client::new(2, 0, 0);
        b.flags.set(WinFlags::FULLSCREEN);
        let applied_a = AppliedWindow {
            rect: Rect::new(0, 0, 1920, 1080),
            border_w: 0,
            seen: true,
        };
        let applied_b = AppliedWindow {
            rect: Rect::new(0, 0, 1920, 1080),
            border_w: 0,
            seen: true,
        };
        let obs_a = classify_configure(Rect::new(40, 40, 640, 480), 0, &applied_a, &a);
        let obs_b = classify_configure(Rect::new(0, 0, 1920, 1080), 0, &applied_b, &b);
        assert!(
            matches!(obs_a, ConfigureObservation::Diverged { follow: false }),
            "A re-asserts its fullscreen rect"
        );
        assert!(
            matches!(obs_b, ConfigureObservation::Compliant),
            "B is untouched and stays fullscreen"
        );
    }

    // ── Phase 10: reconcile() — the full Desired × Applied diff ────────────
    //
    // `reconcile` is the top-level entry the backend calls once per arrange
    // cycle: it walks `DesiredState` and emits exactly the `GeometryEffect`s
    // needed to make `AppliedState` match `Desired`, reading `geometry_dirty`
    // as the only (pure) input flag. These tests pin the contract.

    #[test]
    fn desired_equals_applied_produces_no_effect() {
        let win: WindowId = 1;
        let rect = Rect::new(10, 10, 100, 200);
        let border: u32 = 2;
        let mut state = State::new();
        let mut c = Client::new(win, 0, 0);
        c.geometry_dirty = false;
        state.clients.insert(win, c);
        let mut applied = AppliedState::default();
        applied.windows.insert(
            win,
            AppliedWindow {
                rect,
                border_w: border,
                seen: true,
            },
        );
        let desired = DesiredState {
            windows: vec![DesiredWindow {
                window: win,
                rect,
                border,
                mapped: true,
            }],
            raise: vec![win],
        };
        let effects = reconcile(&desired, &state, &mut applied);
        assert!(
            effects.is_empty(),
            "identical desired/applied must produce zero effects"
        );
    }

    #[test]
    fn desired_differs_from_applied_emits_configure() {
        let win: WindowId = 1;
        let rect = Rect::new(10, 10, 100, 200);
        let applied_rect = Rect::new(0, 0, 50, 50);
        let border: u32 = 2;
        let mut state = State::new();
        let mut c = Client::new(win, 0, 0);
        c.geometry_dirty = false;
        state.clients.insert(win, c);
        let mut applied = AppliedState::default();
        applied.windows.insert(
            win,
            AppliedWindow {
                rect: applied_rect,
                border_w: border,
                seen: true,
            },
        );
        let desired = DesiredState {
            windows: vec![DesiredWindow {
                window: win,
                rect,
                border,
                mapped: true,
            }],
            raise: vec![win],
        };
        let effects = reconcile(&desired, &state, &mut applied);
        assert_eq!(
            effects.len(),
            1,
            "changed window must emit exactly one effect"
        );
        match &effects[0] {
            GeometryEffect::Configure {
                win: w,
                rect: r,
                border: b,
            } => {
                assert_eq!(*w, win);
                assert_eq!(*r, rect);
                assert_eq!(*b, border);
            }
        }
        assert_eq!(
            applied.windows[&win].rect, rect,
            "applied must be updated to desired"
        );
    }

    #[test]
    fn desired_same_rect_force_reapply_emits_when_required() {
        let win: WindowId = 1;
        let rect = Rect::new(10, 10, 100, 200);
        let border: u32 = 2;
        let mut state_dirty = State::new();
        let mut c = Client::new(win, 0, 0);
        c.geometry_dirty = true;
        state_dirty.clients.insert(win, c);
        let mut applied = AppliedState::default();
        applied.windows.insert(
            win,
            AppliedWindow {
                rect,
                border_w: border,
                seen: true,
            },
        );
        let desired = DesiredState {
            windows: vec![DesiredWindow {
                window: win,
                rect,
                border,
                mapped: true,
            }],
            raise: vec![win],
        };
        let e1 = reconcile(&desired, &state_dirty, &mut applied);
        assert_eq!(
            e1.len(),
            1,
            "geometry_dirty must force a reapply even when rect equals applied"
        );
        // After the forced apply, simulate geometry_dirty cleared:
        let mut state_clean = State::new();
        let mut c2 = Client::new(win, 0, 0);
        c2.geometry_dirty = false;
        state_clean.clients.insert(win, c2);
        let e2 = reconcile(&desired, &state_clean, &mut applied);
        assert!(
            e2.is_empty(),
            "once re-applied and not dirty, no further effect"
        );
    }

    #[test]
    fn multiple_windows_diff_independent() {
        let (a, b, c): (WindowId, WindowId, WindowId) = (1, 2, 3);
        let rect_a = Rect::new(0, 0, 100, 100);
        let rect_b = Rect::new(0, 0, 100, 100);
        let rect_c_new = Rect::new(500, 500, 80, 80);
        let rect_c_old = Rect::new(0, 0, 10, 10);
        let border: u32 = 1;
        let mut state = State::new();
        for (w, dirty) in [(a, false), (b, false), (c, false)] {
            let mut cl = Client::new(w, 0, 0);
            cl.geometry_dirty = dirty;
            state.clients.insert(w, cl);
        }
        let mut applied = AppliedState::default();
        for (w, r) in [(a, rect_a), (b, rect_b), (c, rect_c_old)] {
            applied.windows.insert(
                w,
                AppliedWindow {
                    rect: r,
                    border_w: border,
                    seen: true,
                },
            );
        }
        let desired = DesiredState {
            windows: vec![
                DesiredWindow {
                    window: a,
                    rect: rect_a,
                    border,
                    mapped: true,
                },
                DesiredWindow {
                    window: b,
                    rect: rect_b,
                    border,
                    mapped: true,
                },
                DesiredWindow {
                    window: c,
                    rect: rect_c_new,
                    border,
                    mapped: true,
                },
            ],
            raise: vec![a, b, c],
        };
        let effects = reconcile(&desired, &state, &mut applied);
        assert_eq!(effects.len(), 1, "only the changed window (c) emits");
        match &effects[0] {
            GeometryEffect::Configure { win: w, .. } => assert_eq!(*w, c),
        }
        assert_eq!(applied.windows[&c].rect, rect_c_new);
        assert_eq!(applied.windows[&a].rect, rect_a);
    }

    #[test]
    fn destroy_window_removes_desired_and_applied_cleanly() {
        let win: WindowId = 1;
        let mut applied = AppliedState::default();
        applied.windows.insert(
            win,
            AppliedWindow {
                rect: Rect::new(0, 0, 10, 10),
                border_w: 1,
                seen: true,
            },
        );
        applied.forget(win);
        assert!(
            !applied.windows.contains_key(&win),
            "forget must drop the applied record"
        );
        let state = State::new();
        let desired = DesiredState {
            windows: vec![],
            raise: vec![],
        };
        let effects = reconcile(&desired, &state, &mut applied);
        assert!(
            effects.is_empty(),
            "a window absent from desired produces no effect"
        );
    }

    // ── Fase 1.3: invalid geometry ConfigureRequest on a TILED window ────────
    //
    // A hostile client (Firefox / Wine / a game) asks for 0×0, a 60000×60000
    // monster, or a rect parked off the monitor. For a *tiled* (WM-owned) window
    // the verdict MUST be `Diverged { follow: false }` — the WM re-asserts its
    // own Desired and never adopts the bogus rect. `classify_configure` is pure
    // (no layout knowledge), so the verdict is the same regardless of *which*
    // invalid rect is reported; what matters is that the model refuses to follow.
    fn tiled_invalid_is_diverged(reported: Rect) {
        let c = Client::new(1, 0, 0); // tiled: not a float
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        match classify_configure(reported, 2, &applied, &c) {
            ConfigureObservation::Diverged { follow } => {
                assert!(
                    !follow,
                    "tiled invalid ConfigureRequest must NOT be followed"
                );
            }
            ConfigureObservation::Compliant => panic!("invalid rect cannot be our own echo"),
        }
    }

    #[test]
    fn tiled_zero_size_configure_request_is_rejected() {
        tiled_invalid_is_diverged(Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn tiled_huge_configure_request_is_rejected() {
        tiled_invalid_is_diverged(Rect::new(0, 0, 60000, 60000));
    }

    #[test]
    fn tiled_off_monitor_configure_request_is_rejected() {
        // A rect whose top-left sits outside the monitor entirely.
        tiled_invalid_is_diverged(Rect::new(5000, 5000, 300, 300));
    }

    // ── Fase 1.3: invalid geometry ConfigureRequest on a FLOAT ───────────────
    //
    // A float IS allowed external geometry, so the verdict is
    // `Diverged { follow: true }` — but the backend's single geometry sink then
    // routes the reported rect through `clamp_float_to_workarea`, so a 0×0 /
    // overflow / off-monitor request is normalized to a valid in-workarea rect
    // and never reaches X11 as a degenerate configure (X11 rejects 0×0 with
    // BadValue).

    #[test]
    fn float_invalid_configure_request_is_followed_then_clamped() {
        let mut c = Client::new(1, 0, 0);
        c.flags.set(WinFlags::FLOAT);
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
        };
        let obs = classify_configure(Rect::new(0, 0, 0, 0), 2, &applied, &c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: true }),
            "a float may follow an external (even invalid) rect"
        );
    }

    // ── Fase 1.3: `clamp_float_to_workarea` — the single normalizer ──────────
    //
    // Pure over (rect, workarea, border): every degenerate the hostile client
    // can send must come back as a strictly-positive, in-workarea rect.
    use crate::backend::x11::render::clamp_float_to_workarea;

    #[test]
    fn clamp_zero_size_never_reaches_x11() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let g = clamp_float_to_workarea(Rect::new(0, 0, 0, 0), wa, 2);
        assert!(
            g.w >= 1 && g.h >= 1,
            "0×0 must normalize to the X11-valid minimum"
        );
        assert!(
            g.x >= wa.x && g.y >= wa.y,
            "clamped rect must stay inside workarea"
        );
        assert!(g.right() <= wa.right() && g.bottom() <= wa.bottom());
    }

    #[test]
    fn clamp_huge_size_fits_workarea() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let g = clamp_float_to_workarea(Rect::new(0, 0, 60000, 60000), wa, 2);
        assert!(
            g.w <= wa.w && g.h <= wa.h,
            "huge request must be clamped to workarea"
        );
        assert!(g.x >= wa.x && g.y >= wa.y);
        assert!(g.right() <= wa.right() && g.bottom() <= wa.bottom());
    }

    #[test]
    fn clamp_negative_position_stays_inside_workarea() {
        let wa = Rect::new(100, 50, 800, 600);
        let g = clamp_float_to_workarea(Rect::new(-9000, -9000, 300, 200), wa, 0);
        assert!(g.x >= wa.x, "negative x must clamp to workarea left");
        assert!(g.y >= wa.y, "negative y must clamp to workarea top");
        assert!(g.right() <= wa.right() && g.bottom() <= wa.bottom());
    }

    #[test]
    fn clamp_overflow_offscreen_bottom_right_stays_inside() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let g = clamp_float_to_workarea(Rect::new(5000, 5000, 400, 400), wa, 2);
        assert!(
            g.right() <= wa.right() && g.bottom() <= wa.bottom(),
            "off-monitor rect must be pulled back inside"
        );
    }
}
