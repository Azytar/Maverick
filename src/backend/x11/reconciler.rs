//! Reconciliation — the single owner of "what geometry/stack has
//! actually been written to X11".
//!
//! `AppliedState` keeps one `AppliedWindow` — the last *applied* rect/border
//! per window — and diffs every *desired* placement against it, emitting only
//! the `configure_window` calls that actually changed. No other code path may
//! decide on its own whether a configure is needed.
//!
//! # Pipeline
//!
//! ```text
//! State + Cfg + Phase → layout::arrange → Placements
//!     → present::present_into → DesiredState
//!     → Reconciler::reconcile → Vec<GeometryEffect>
//!     → emit_geometry → X11
//! ```
//!
//! # Source of truth
//!
//! - **Desired** — the pure layout+present snapshot (`DesiredState`): every
//!   window's desired rect/border/stacking for one arrange cycle, produced by
//!   `layout::arrange` + `present::present_into`. `client.geom` is the desired
//!   *logical* geometry the core wants; the reconciler never writes it (only
//!   `emit_geometry` does, on the normal path).
//! - **Applied** — what X11 *currently* shows (`AppliedState`).
//! - **Real** — what X11 *reports* via `ConfigureNotify`
//!   (observed in `events.rs::on_configure_notify`). With
//!   `SUBSTRUCTURE_REDIRECT` on the root this is always the echo of one of our
//!   own requests, so it never becomes the model (see `classify_configure`).
//!
//! The reconciler diffs Desired vs Applied; client intent arrives only through
//! `ConfigureRequest`, which the float sink adopts and records with
//! `AppliedState::observe`.
//!
//! # Idempotency
//!
//! Reconciliation is safe to repeat: the diff only emits when the
//! desired rect/border actually changed from what was last applied.
//! A no-op reconcile (desired == applied) emits nothing, so repeated
//! calls from `arrange_full_phase` (once per animating monitor per
//! frame) do not spam the X server.
//!
//! # Invariants
//!
//! - `diff` never mutates `State`.
//! - A `geometry_dirty` flag forces emission even when the rect is
//!   identical (border/state changes without geometry change).
//! - `forget` on unmanage ensures the window is re-emitted if it
//!   reappears.
//! - Float geometry is always clamped to the workarea via
//!   `clamp_float_to_workarea` before emission (no degenerate
//!   0×0 or off-screen rects ever reach X11).
//! - The `seen` flag tracks whether the window has ever been
//!   applied — a freshly-mapped window always gets its first
//!   configure emitted regardless of rect equality.

use crate::core::desired::DesiredState;
use crate::types::{Rect, State, WindowId};

// Observability-only macro for the reconcile/desired→applied pipeline: no-op
// unless `window-trace` is enabled.
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
    /// X11 sequence number of the request that produced `rect`, when the writer
    /// knew it. No verdict in this module reads it; it exists so an applied
    /// record can be traced back to a request, and is `None` for synthetic or
    /// untracked configures.
    pub sequence: Option<u32>,
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

    /// Record what X11 currently shows **without emitting a configure**.
    ///
    /// `Applied` means "what X11 has", so an observation of a rect the window
    /// genuinely has (e.g. a `ConfigureRequest` we just adopted) must move the
    /// record without poking the window again. Re-issuing `configure_window` for
    /// a rect the window already has is pure protocol noise, and for a float it
    /// is worse than noise: it emits a fresh `ConfigureNotify` that a toolkit may
    /// answer with another `ConfigureRequest` — the feedback loop that reads on
    /// screen as a window that moves by itself.
    // Exercised by the unit tests below, which install already-applied geometry
    // to pin the echo/Stale contract; the production sink records through `diff`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn observe(&mut self, win: WindowId, rect: Rect, border_w: u32) {
        let prev = self.windows.entry(win).or_default();
        prev.rect = rect;
        prev.border_w = border_w;
        prev.seen = true;
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

/// The verdict of comparing an external `ConfigureNotify` against `Applied`.
///
/// The caller acts on the verdict in `on_configure_notify`: `Compliant` does
/// nothing, `Stale` re-asserts the *model* and lets `AppliedState::diff`
/// decide whether a write is even owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureObservation {
    /// Reported geometry equals what we last applied: our own echo. Nothing to do.
    Compliant,
    /// X11 reports a geometry other than the last applied one. The caller
    /// re-asserts the *model* (`client.geom`), never the reported rect, and the
    /// `AppliedState::diff` inside that path suppresses the write when X11
    /// already matches — which is the normal outcome, because the divergence is
    /// usually an echo of our own *older* request.
    Stale,
}

/// Classify an external `ConfigureNotify` for a managed window. Pure: it reads
/// only the reported rect/border and `AppliedState`, so the policy is
/// unit-tested without an X server. The caller acts on the verdict (see
/// `on_configure_notify`).
///
/// # Why there is no "follow the client" verdict
///
/// The WM holds `SUBSTRUCTURE_REDIRECT` on the root, so for a viewable managed
/// window the server turns every client `ConfigureWindow` into a
/// `ConfigureRequest` and leaves the window untouched: **only this WM moves
/// managed windows**. A `ConfigureNotify` is thus an *echo*, and one that
/// diverges from `Applied` is a *stale* echo (an older request of ours whose
/// event was queued behind newer traffic). Adopting it would overwrite the model
/// with a geometry the WM already left behind and re-configure the window onto
/// it, so the client's next request is answered with the past: measured as a
/// ~150 configures/s ping-pong between two geometries (the window visibly
/// jumping between two sizes/positions) with the WM burning a core.
pub(crate) fn classify_configure(
    reported_rect: Rect,
    reported_bw: u32,
    applied: &AppliedWindow,
) -> ConfigureObservation {
    if applied.rect == reported_rect && applied.border_w == reported_bw {
        ConfigureObservation::Compliant
    } else {
        ConfigureObservation::Stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::desired::DesiredWindow;
    use crate::types::{Client, WinFlags};

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

    // Convergence: external ConfigureNotify vs Applied.
    //
    // A managed window's `ConfigureNotify` is an *echo* of a request this WM
    // issued: `SUBSTRUCTURE_REDIRECT` on the root means the server never applies
    // a client's `ConfigureWindow` to a viewable child of the root. The scenarios
    // below pin what happens when that echo is treated as a client decision.

    /// The echo of our own latest request is Compliant — nothing to do. Pinned
    /// for a float and for a tiled window (authority does not enter here: both
    /// are echoes).
    #[test]
    fn matching_echo_is_compliant() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 1000, 800), 2, &applied),
            ConfigureObservation::Compliant
        );
        // A border-only difference is still our own (stale) configure.
        assert_eq!(
            classify_configure(Rect::new(0, 0, 1000, 800), 0, &applied),
            ConfigureObservation::Stale
        );
    }

    /// A reported rect that is not the last applied one is `Stale`, *regardless*
    /// of the window being a float: the verdict carries no `follow` bit anymore,
    /// because with the redirect in place the client cannot have moved it.
    #[test]
    fn diverging_echo_is_stale_not_client_intent() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 400, 300), 2, &applied),
            ConfigureObservation::Stale
        );
        // Degenerate reported rects (hostile client) classify the same way; they
        // never reach the model because the verdict is not "adopt".
        assert_eq!(
            classify_configure(Rect::new(0, 0, 0, 0), 2, &applied),
            ConfigureObservation::Stale
        );
    }

    /// Regression pin for the erratic-float bug (measured as a ~150
    /// configures/s ping-pong between two geometries): a stale echo, issued while
    /// the model has already moved on, must neither move the model nor produce a
    /// configure — the caller re-asserts the model and the diff suppresses the
    /// write because X11 already shows it.
    #[test]
    fn stale_echo_causes_no_configure_storm() {
        let mut applied = AppliedState::default();
        let model = Rect::new(490, 260, 605, 306);
        // The client's `ConfigureRequest` was adopted: the model IS the request,
        // and X11 already shows it (the sink records it via `observe`).
        assert_eq!(applied.diff(1, model, 2, false), Some((model, 2)));
        applied.observe(1, model, 2);
        assert_eq!(
            applied.diff(1, model, 2, false),
            None,
            "adopting a request must not re-poke the window"
        );

        // An echo of our *older* request (the previous geometry) arrives.
        let stale = Rect::new(490, 260, 300, 200);
        assert_eq!(
            classify_configure(stale, 2, &applied.windows[&1]),
            ConfigureObservation::Stale
        );
        // What the caller does with a `Stale` verdict: re-assert the MODEL.
        // Because X11 already matches, this emits nothing — no ping-pong.
        assert_eq!(
            applied.diff(1, model, 2, false),
            None,
            "a stale echo must not generate a configure"
        );
    }

    /// `observe` adopts what X11 really shows without emitting, and the entry is
    /// then `seen`: the `ConfigureRequest` sink's entry point.
    #[test]
    fn observe_records_real_geometry_without_emitting() {
        let mut applied = AppliedState::default();
        applied.observe(7, Rect::new(5, 5, 50, 50), 2);
        let w = &applied.windows[&7];
        assert_eq!(w.rect, Rect::new(5, 5, 50, 50));
        assert!(w.seen);
        assert_eq!(applied.diff(7, Rect::new(5, 5, 50, 50), 2, false), None);
        // A different rect still emits (the record is not a no-op stub).
        assert_eq!(
            applied.diff(7, Rect::new(6, 6, 60, 60), 2, false),
            Some((Rect::new(6, 6, 60, 60), 2))
        );
    }

    /// Two windows: A's echo matches, B's is stale. Per-window records never
    /// bleed into each other.
    #[test]
    fn ab_independent() {
        let applied_a = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let applied_b = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 1000, 800), 2, &applied_a),
            ConfigureObservation::Compliant
        );
        assert_eq!(
            classify_configure(Rect::new(0, 0, 400, 300), 2, &applied_b),
            ConfigureObservation::Stale
        );
    }

    /// A fullscreen window's echo of its own overlay rect is Compliant; a stale
    /// one is Stale. Fullscreen is not special: it is just another rect.
    #[test]
    fn fullscreen_echo_is_not_special() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1920, 1080),
            border_w: 0,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 1920, 1080), 0, &applied),
            ConfigureObservation::Compliant
        );
        assert_eq!(
            classify_configure(Rect::new(40, 40, 640, 480), 0, &applied),
            ConfigureObservation::Stale
        );
    }

    // `reconcile()` — the full Desired × Applied diff.
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
                sequence: None,
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
                sequence: None,
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
                sequence: None,
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
                    sequence: None,
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
                sequence: None,
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

    // Invalid geometry ConfigureRequest on a TILED window.
    //
    // A hostile client (Firefox / Wine / a game) asks for 0×0, a 60000×60000
    // monster, or a rect parked off the monitor. For a *tiled* (WM-owned) window
    // the verdict is `Stale`: the WM re-asserts its own model and never adopts
    // the bogus rect. The verdict does not depend on *which* invalid rect is
    // reported, nor on the window being tiled or a float — anything that differs
    // from `Applied` is stale traffic the caller re-asserts over.
    fn tiled_invalid_is_diverged(reported: Rect) {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(reported, 2, &applied),
            ConfigureObservation::Stale,
            "a report differing from Applied must be Stale — never adopted, re-asserted over"
        );
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

    // Invalid geometry ConfigureRequest on a FLOAT.
    //
    // A float *is* allowed external geometry, but the adopt decision belongs to
    // the backend's single geometry sink, not to this classification. The
    // classification only separates *our echo* (Compliant) from *stale traffic*
    // (Stale), so a 0×0 report on a float is Stale exactly like on a tile and
    // never reaches X11 as a degenerate configure (X11 rejects 0×0 with
    // BadValue).

    #[test]
    fn float_invalid_configure_request_is_followed_then_clamped() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 1000, 800),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 0, 0), 2, &applied),
            ConfigureObservation::Stale,
            "an invalid float report is stale traffic: the sink normalizes and re-asserts"
        );
        // The contract is pure geometry equality, not window policy: had the
        // sink adopted and applied this exact rect, the echo would be
        // Compliant — Stale is about "not what we last applied", nothing else.
        let adopted = AppliedWindow {
            rect: Rect::new(0, 0, 0, 0),
            ..applied
        };
        assert_eq!(
            classify_configure(Rect::new(0, 0, 0, 0), 2, &adopted),
            ConfigureObservation::Compliant
        );
    }

    // Drag authority table.
    //
    // The (float, fullscreen, dragged) → follow table no longer exists:
    // `classify_configure` does not produce a follow decision at all, the sink
    // owns the drag policy. What classify still guarantees — and what this
    // table pins — is that the verdict depends ONLY on geometry equality, never
    // on the window's flags or the drag state: any divergent report is Stale
    // (the caller re-asserts over it), any echo of Applied is Compliant.
    #[test]
    fn drag_authority_table() {
        let mk = |is_float: bool, is_fs: bool| {
            let mut c = Client::new(1, 0, 0);
            if is_float {
                c.flags.set(WinFlags::FLOAT);
            }
            if is_fs {
                c.flags.set(WinFlags::FULLSCREEN);
            }
            let applied = AppliedWindow {
                rect: Rect::new(0, 0, 100, 100),
                border_w: 2,
                seen: true,
                sequence: None,
            };
            let (obs, echo_obs) = (
                classify_configure(Rect::new(10, 10, 200, 200), 2, &applied),
                classify_configure(Rect::new(0, 0, 100, 100), 2, &applied),
            );
            assert_eq!(
                obs,
                ConfigureObservation::Stale,
                "float={is_float} fullscreen={is_fs}: a divergent report must be Stale"
            );
            assert_eq!(
                echo_obs,
                ConfigureObservation::Compliant,
                "float={is_float} fullscreen={is_fs}: our own echo must be Compliant"
            );
        };
        mk(false, false);
        mk(true, false);
        mk(false, true);
        mk(true, true);
    }

    #[test]
    fn float_dragged_does_not_follow() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 100, 100),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        // The drag policy moved to the sink; classify still guarantees that the
        // dragged float's reported rect is never mistaken for our own echo.
        assert_eq!(
            classify_configure(Rect::new(5, 5, 50, 50), 2, &applied),
            ConfigureObservation::Stale,
            "a dragged float's divergent report must be Stale, never treated as our echo"
        );
        // ...and the rect the sink will re-assert (Applied) classifies as our
        // echo — the only Compliant outcome.
        assert_eq!(
            classify_configure(Rect::new(0, 0, 100, 100), 2, &applied),
            ConfigureObservation::Compliant
        );
    }

    #[test]
    fn drag_ends_restores_float_authority() {
        let applied = AppliedWindow {
            rect: Rect::new(0, 0, 100, 100),
            border_w: 2,
            seen: true,
            sequence: None,
        };
        // During the drag and after it, classification is flag-blind: a
        // divergent report is Stale either way. What the drag end changes is
        // the sink's decision (re-assert during, adopt after) — pinned here as
        // the geometry contract classify feeds.
        let during = classify_configure(Rect::new(5, 5, 50, 50), 2, &applied);
        assert_eq!(during, ConfigureObservation::Stale);
        let after = classify_configure(Rect::new(5, 5, 50, 50), 2, &applied);
        assert_eq!(after, ConfigureObservation::Stale);
        // Had the sink adopted (Applied := reported), the echo would be
        // Compliant — the only path back to a quiet classification.
        let adopted = AppliedWindow {
            rect: Rect::new(5, 5, 50, 50),
            ..applied
        };
        assert_eq!(
            classify_configure(Rect::new(5, 5, 50, 50), 2, &adopted),
            ConfigureObservation::Compliant
        );
    }

    // `clamp_float_to_workarea` — the single normalizer.
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
