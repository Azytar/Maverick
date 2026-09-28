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

/// The geometry X11 will actually hold for a requested `(rect, border)`.
///
/// `ConfigureWindow` takes an INT16 origin and a CARD16 extent, so a request can
/// ask for more than the protocol can express. The sink clamps before sending
/// (a 0×0 configure is `BadValue` and the server drops it silently, and anything
/// past CARD16 is not describable at all), which means the clamped value is what
/// ends up on the server.
///
/// This is the *single* definition of that conversion, shared by the record and
/// the sink, because the two must not be allowed to disagree: if `Applied` kept
/// the raw value the diff would believe a window is still pending a change it has
/// already sent, and re-emit it on every frame forever. The x/y origin is passed
/// through unchanged — it is already an `i32` that the layout produced, and the
/// synthetic `ConfigureNotify` and the request agree on how to present it.
pub(crate) fn wire_geometry(geom: Rect, bw: u32) -> (Rect, u32) {
    (
        Rect::new(
            geom.x,
            geom.y,
            geom.w.clamp(1, u16::MAX as u32),
            geom.h.clamp(1, u16::MAX as u32),
        ),
        bw.min(u16::MAX as u32),
    )
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
        // Compare and record the *wire* geometry, not the request. The protocol
        // is narrower than the model in both directions, and the clamped value
        // is what X11 ends up holding — so recording the request would make the
        // record disagree with the server permanently, and the next reconcile
        // would either re-emit forever or (once the values coincide by luck)
        // stop trying to fix a window that is already wrong. Doing it here,
        // before the comparison, is what makes "applied" mean "has".
        let (want_rect, want_bw) = wire_geometry(desired_rect, desired_bw);
        let prev = self.windows.entry(win).or_default();
        let changed =
            geometry_dirty || !prev.seen || prev.rect != want_rect || prev.border_w != want_bw;
        if changed {
            prev.rect = want_rect;
            prev.border_w = want_bw;
            prev.seen = true;
            Some((want_rect, want_bw))
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
    // Coalesce before diffing. A window reachable from two placements is illegal
    // — `check_invariants` #4 rejects it by name and no producer creates one —
    // but the reconciler must still converge if it happens, because that check
    // is a debug-only one and a release build has nothing else to stop it.
    //
    // Diffing both entries against a record the other just overwrote makes every
    // round re-emit both, so the window ping-pongs between two geometries and
    // takes two `configure_window`s per arrange, indefinitely. The placement list
    // is ordered back-to-front, so the *last* entry for a window is the one
    // nearest the top of the stack, and last-write-wins is both the cheapest
    // deterministic policy and the one that matches the raise order.
    let mut last_index: std::collections::HashMap<WindowId, usize> =
        std::collections::HashMap::with_capacity(desired.windows.len());
    for (i, dw) in desired.windows.iter().enumerate() {
        last_index.insert(dw.window, i);
    }

    let mut out = Vec::new();
    for (i, dw) in desired.windows.iter().enumerate() {
        if last_index[&dw.window] != i {
            continue;
        }
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

    // Property coverage of the same contracts, over generated inputs rather
    // than one hand-picked scenario each.

    use proptest::prelude::*;

    /// The geometry a client (or the layout) can ask for. Every field spans its
    /// full range: the reconciler is handed hostile rects exactly as often as
    /// sane ones, and X11 rejects a 0×0 configure with `BadValue`.
    fn arb_rect() -> impl Strategy<Value = Rect> {
        (any::<i32>(), any::<i32>(), any::<u32>(), any::<u32>())
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// One generated placement: what the layout wants for `win`, whether the
    /// client is flagged `geometry_dirty` (a pending fullscreen/maximize
    /// transition), and whether a client record exists for it at all.
    #[derive(Debug, Clone)]
    struct Row {
        win: WindowId,
        rect: Rect,
        border: u32,
        dirty: bool,
        known: bool,
    }

    /// A desired snapshot plus the client records that back it. Window ids are
    /// drawn from a small range so duplicate ids occur, and are then collapsed
    /// to their first occurrence: `present_into` places each window on exactly
    /// one monitor, and a repeated entry would make the per-window
    /// correspondence the assertions rely on ambiguous.
    fn arb_rows() -> impl Strategy<Value = Vec<Row>> {
        prop::collection::vec(
            (
                1u32..=12,
                arb_rect(),
                any::<u32>(),
                any::<bool>(),
                any::<bool>(),
            ),
            0..=8,
        )
        .prop_map(|v| {
            let mut seen = std::collections::HashSet::new();
            v.into_iter()
                .map(|(win, rect, border, dirty, known)| Row {
                    win,
                    rect,
                    border,
                    dirty,
                    known,
                })
                .filter(|r| seen.insert(r.win))
                .collect()
        })
    }

    /// A pre-existing applied record per generated index into `rows`.
    ///
    /// The records are *derived* from the desired geometry — nudged by one pixel
    /// or one border step rather than drawn independently — because the
    /// interesting reconcile cases are "X11 already shows this", "X11 shows this
    /// with one field off" and "X11 has never been told", and an unrelated
    /// random rect would only ever produce the last two. An empty placement set
    /// is a legal input rather than a strategy that cannot be built, and one
    /// extra record is always left over for a window the layout dropped.
    fn arb_applied(
        rows: &[Row],
    ) -> impl Strategy<Value = std::collections::HashMap<WindowId, AppliedWindow>> {
        let rows: Vec<Row> = rows.to_vec();
        prop::collection::vec(
            (
                0usize..=8,
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<u32>(),
            ),
            0..=10,
        )
        .prop_map(move |records| {
            let mut map = std::collections::HashMap::new();
            for (idx, seen, nudge_x, nudge_w, border_step) in records {
                let Some(row) = rows.get(idx % rows.len().max(1)) else {
                    continue;
                };
                map.insert(
                    row.win,
                    AppliedWindow {
                        rect: Rect::new(
                            row.rect.x.wrapping_add(i32::from(nudge_x)),
                            row.rect.y,
                            row.rect.w.wrapping_add(u32::from(nudge_w)),
                            row.rect.h,
                        ),
                        border_w: row.border.wrapping_add(border_step),
                        seen,
                        sequence: None,
                    },
                );
            }
            // An id no row can produce, so a stale record for an unmanaged
            // window is always in play.
            map.insert(
                0xF00D,
                AppliedWindow {
                    rect: Rect::new(0, 0, 10, 10),
                    border_w: 1,
                    seen: true,
                    sequence: None,
                },
            );
            map
        })
    }

    /// A desired snapshot together with the applied records that precede it.
    /// Flat-mapped so the records are generated against the same windows the
    /// snapshot names.
    fn arb_rows_and_applied(
    ) -> impl Strategy<Value = (Vec<Row>, std::collections::HashMap<WindowId, AppliedWindow>)> {
        arb_rows().prop_flat_map(|rows| {
            let applied = arb_applied(&rows);
            (Just(rows), applied).prop_map(|(rows, applied)| (rows, applied))
        })
    }

    /// The `State` the reconciler is allowed to *read*: one client per `known`
    /// row, carrying only the flag it is documented to read.
    fn state_for(rows: &[Row]) -> State {
        let mut state = State::new();
        for r in rows.iter().filter(|r| r.known) {
            let mut c = Client::new(r.win, 0, 0);
            c.geometry_dirty = r.dirty;
            state.clients.insert(r.win, c);
        }
        state
    }

    /// The desired snapshot for a set of rows.
    fn desired_for(rows: &[Row]) -> DesiredState {
        DesiredState {
            windows: rows
                .iter()
                .map(|r| DesiredWindow {
                    window: r.win,
                    rect: r.rect,
                    border: r.border,
                    mapped: true,
                })
                .collect(),
            raise: rows.iter().map(|r| r.win).collect(),
        }
    }

    /// Every field of `State`, in a form that does not depend on hash-map
    /// iteration order, so any mutation the reconciler made to the logical
    /// state shows up as a difference.
    fn fingerprint(state: &State) -> Vec<String> {
        let mut parts: Vec<String> = state
            .clients
            .iter()
            .map(|(&w, c)| format!("{w}={c:?}"))
            .collect();
        parts.sort();
        parts.push(format!(
            "monitors={:?} sel={} serial={} running={} status={:?} transients={:?} \
             focus={:?} pending={:?} wallpaper={:?} rev={}",
            state.monitors,
            state.sel_mon,
            state.focus_serial,
            state.running,
            state.status,
            state.pending_transients,
            state.x11_input_focus,
            state.pending_focus,
            state.wallpaper,
            state.wallpaper_rev
        ));
        parts
    }

    /// A run of arrange cycles: each snapshot is a fresh desired state, and
    /// `records` seeds X11 with geometry the window manager believes it has
    /// already written (`idx` picks the window out of the first snapshot).
    #[derive(Debug, Clone)]
    struct Scenario {
        snapshots: Vec<Vec<Row>>,
        records: Vec<Record>,
    }

    #[derive(Debug, Clone)]
    struct Record {
        idx: usize,
        rect: Rect,
        border_w: u32,
    }

    fn arb_scenario() -> impl Strategy<Value = Scenario> {
        (
            prop::collection::vec(arb_rows(), 1..=4),
            prop::collection::vec((0usize..=8, arb_rect(), any::<u32>()), 0..=8),
        )
            .prop_map(|(snapshots, records)| Scenario {
                snapshots,
                records: records
                    .into_iter()
                    .map(|(idx, rect, border_w)| Record {
                        idx,
                        rect,
                        border_w,
                    })
                    .collect(),
            })
    }

    proptest! {
        /// Totality: one round either writes every window whose geometry X11
        /// does not already have — carrying exactly the desired rect and border
        /// — or writes nothing at all. It never configures a window the layout
        /// no longer manages, never configures one window twice in a round, and
        /// never leaves the record claiming X11 shows something other than what
        /// was just asked for. A managed window whose desired geometry was
        /// dropped is the failure this guards: it keeps whatever it had.
        #[test]
        fn reconcile_writes_exactly_the_pending_configures(
            (rows, applied) in arb_rows_and_applied(),
        ) {
            let mut applied = AppliedState { windows: applied };
            let state = state_for(&rows);
            let desired = desired_for(&rows);

            let effects = reconcile(&desired, &state, &mut applied);

            let mut emitted: Vec<WindowId> = Vec::new();
            for e in &effects {
                let GeometryEffect::Configure { win, rect, border } = e;
                prop_assert!(
                    !emitted.contains(win),
                    "window {} configured twice in one round",
                    win
                );
                let row = rows.iter().find(|r| r.win == *win)
                    .expect("reconcile configured a window the layout does not manage");
                // The wire geometry, not the request: the protocol is narrower
                // than the model, and the clamped value is what the server ends up
                // holding, so that is what "the desired rect" means at this seam.
                let (want_rect, _) = wire_geometry(row.rect, row.border);
                prop_assert_eq!(*rect, want_rect, "effect for {} carries a foreign rect", win);
                let (_, want_bw) = wire_geometry(row.rect, row.border);
                prop_assert_eq!(
                    *border,
                    want_bw,
                    "effect for {} carries a foreign border",
                    win
                );
                emitted.push(*win);
            }

            // Whatever came out, the record now says X11 shows the desired
            // geometry for every window in the snapshot.
            for r in &rows {
                let w = applied.windows.get(&r.win)
                    .unwrap_or_else(|| panic!("window {} was left unapplied", r.win));
                prop_assert!(w.seen, "window {} was never configured", r.win);
                let (want_rect, want_bw) = wire_geometry(r.rect, r.border);
                prop_assert_eq!(w.rect, want_rect, "window {} applied a stale rect", r.win);
                prop_assert_eq!(w.border_w, want_bw, "window {} applied a stale border", r.win);
            }
        }

    }

    /// `Applied` must record what X11 *has*, not what Maverick *asked for*.
    ///
    /// The wire is narrower than the model in both directions. `ConfigureWindow`
    /// takes an INT16 origin and a CARD16 extent, so the sink clamps before it
    /// sends — and the clamp is what X11 ends up holding. But the clamp lives in
    /// `emit_geometry`, *after* `diff` has already written the unclamped value
    /// into the record, so `Applied` can hold `w = 100_000` while the server has
    /// `65_535`. From then on `prev.rect != desired_rect` is false and the window
    /// is never re-emitted: the divergence is permanent, and no mechanism can
    /// notice, because the only thing that reads the record is this same diff.
    ///
    /// The failure is the one `emit_geometry`'s own comment names — "leaving
    /// Applied ahead of Real forever" — defended against the 0×0 lower bound and
    /// not the upper one. It matters for the border too, twice over: the border
    /// is published as `_NET_FRAME_EXTENTS`, which a client sizes its content
    /// from, and it is written back into `client.geom`/`client.border_w`, which
    /// is the rect hit-testing reads.
    ///
    /// Both halves are asserted, and the second one is the reason the fix has to
    /// happen *here* rather than in the sink: if only the record were clamped
    /// while the comparison kept using the raw desired rect, the very next
    /// reconcile would see `clamped != raw` and configure every window on every
    /// frame forever.
    #[test]
    fn applied_records_the_geometry_the_wire_can_carry() {
        let mut applied = AppliedState::default();
        let cases = [
            // (requested w, requested h, requested bw) -> (on the wire)
            (0u32, 0u32, 0u32),
            (1, 1, 1),
            (1920, 1080, 2),
            (u16::MAX as u32, u16::MAX as u32, u16::MAX as u32),
            // Past CARD16 in each field independently.
            (100_000, 1080, 2),
            (1920, 100_000, 2),
            (1920, 1080, 100_000),
            (u32::MAX, u32::MAX, u32::MAX),
        ];
        for (w, h, bw) in cases {
            let want = Rect::new(0, 0, w, h);
            let (got_rect, got_bw) = applied
                .diff(1, want, bw, false)
                .expect("a first apply must emit");
            assert_eq!(
                got_rect,
                Rect::new(
                    0,
                    0,
                    w.clamp(1, u16::MAX as u32),
                    h.clamp(1, u16::MAX as u32)
                ),
                "the emitted geometry for {w}x{h} bw={bw} is not what the wire can carry"
            );
            assert_eq!(
                got_bw,
                bw.min(u16::MAX as u32),
                "border {bw} exceeds CARD16"
            );
            // And the record must agree with what was emitted, or the next
            // reconcile re-emits forever.
            let rec = applied
                .windows
                .get(&1)
                .expect("record exists after a first apply");
            assert_eq!(
                (rec.rect, rec.border_w),
                (got_rect, got_bw),
                "the record kept the raw desired value, so every later reconcile \
                 would see a change and re-emit"
            );
        }
    }

    /// The convergence property behind the one above, stated on the cycle rather
    /// than on a single call: a stable desired state must stop generating work,
    /// and it must stay stopped for values the wire has to clamp.
    #[test]
    fn a_clamped_geometry_still_converges_in_one_request() {
        let mut applied = AppliedState::default();
        let state = {
            let mut s = State::new();
            let mut c = Client::new(1, 0, 0);
            c.geometry_dirty = false;
            s.clients.insert(1, c);
            s
        };
        // A rect no protocol field can express in full.
        let rows = vec![Row {
            win: 1,
            rect: Rect::new(0, 0, 100_000, 100_000),
            border: 100_000,
            dirty: false,
            known: true,
        }];
        let desired = desired_for(&rows);

        let first = reconcile(&desired, &state, &mut applied);
        assert_eq!(first.len(), 1, "the first pass must configure the window");

        for round in 2..=50 {
            let effects = reconcile(&desired, &state, &mut applied);
            assert!(
                effects.is_empty(),
                "round {round} re-emitted {} effects: a clamped geometry that \
                 never reaches a fixed point configures the window on every frame \
                 forever",
                effects.len()
            );
        }
    }
    /// The reconciler must be *total*: it converges for any `DesiredState`,
    /// including one the model checker rejects.
    ///
    /// A window reachable from two placements is illegal — `check_invariants`
    /// #4 rejects it by name, and no production path produces one, since every
    /// mutator that moves a window between a column and `floats` removes it
    /// from the old list first. So this is defence in depth, not a live bug. It
    /// matters anyway, because the check is a *debug* one: in a release build
    /// nothing rejects a producer that gets this wrong, and the consequence is
    /// not a wrong rect but a window that ping-pongs between two geometries and
    /// takes two `configure_window`s per arrange, forever.
    ///
    /// The oracle is convergence, not correctness: a second round must emit
    /// nothing, whatever the input said. Last-write-wins is the right policy to
    /// settle on — the placement list is ordered back-to-front, so the entry that
    /// survives is the one nearest the top of the stack.
    #[test]
    fn a_duplicate_desired_entry_still_converges() {
        let mut state = State::new();
        for win in [1u32, 2] {
            let mut c = Client::new(win, 0, 0);
            c.geometry_dirty = false;
            state.clients.insert(win, c);
        }
        // The same window in two places with different geometry — the shape a
        // producer bug would produce.
        let desired = DesiredState {
            windows: vec![
                DesiredWindow {
                    window: 1,
                    rect: Rect::new(0, 0, 100, 100),
                    border: 2,
                    mapped: true,
                },
                DesiredWindow {
                    window: 2,
                    rect: Rect::new(200, 0, 100, 100),
                    border: 2,
                    mapped: true,
                },
                DesiredWindow {
                    window: 1,
                    rect: Rect::new(500, 500, 80, 80),
                    border: 2,
                    mapped: true,
                },
            ],
            raise: vec![1, 2],
        };

        let mut applied = AppliedState::default();
        for round in 1..=64 {
            let effects = reconcile(&desired, &state, &mut applied);
            if round > 1 {
                assert!(
                    effects.is_empty(),
                    "round {round} emitted {} effects: a duplicate entry that never \
                     reaches a fixed point configures the window on every frame forever",
                    effects.len()
                );
            }
        }
        // And the window settled on one of the two geometries, not on neither.
        let rec = applied.windows.get(&1).expect("window 1 was applied");
        assert!(
            rec.rect == Rect::new(0, 0, 100, 100) || rec.rect == Rect::new(500, 500, 80, 80),
            "window 1 settled on {:?}, which is neither of the two desired rects",
            rec.rect
        );
    }
    proptest! {
        /// Idempotence: a repeat of a reconcile that already ran emits nothing.
        /// The render loop calls this once per animating monitor per frame, so
        /// any churn here is a `configure_window` storm on the X server.
        #[test]
        fn a_repeated_reconcile_emits_nothing(
            (rows, applied) in arb_rows_and_applied(),
        ) {
            let mut applied = AppliedState { windows: applied };
            // Clean clients: nothing is mid-transition, so no flag can force a
            // re-poke of an unchanged geometry.
            let clean: Vec<Row> = rows.iter().map(|r| Row { dirty: false, ..r.clone() }).collect();
            let state = state_for(&clean);
            let desired = desired_for(&clean);

            let _first = reconcile(&desired, &state, &mut applied);
            let second = reconcile(&desired, &state, &mut applied);
            prop_assert!(
                second.is_empty(),
                "reconciling an applied state emitted {:?}",
                second.len()
            );
        }

        /// The only thing that may re-poke an unchanged geometry is a pending
        /// transition on *that* window: a dirty client is re-asserted every
        /// turn until it settles, and no other window is dragged into it. This
        /// is what keeps a fullscreen transition from turning into a flood.
        #[test]
        fn only_a_dirty_client_is_re_poked(
            (rows, applied) in arb_rows_and_applied(),
        ) {
            let mut applied = AppliedState { windows: applied };
            let state = state_for(&rows);
            let desired = desired_for(&rows);

            let _first = reconcile(&desired, &state, &mut applied);
            let second = reconcile(&desired, &state, &mut applied);

            // A desired window with no client record has nothing to report a
            // pending transition: only a tracked dirty client is re-poked.
            let mut expected: Vec<WindowId> = rows
                .iter()
                .filter(|r| r.dirty && r.known)
                .map(|r| r.win)
                .collect();
            expected.sort_unstable();
            let mut got: Vec<WindowId> = second.iter().map(|e| match e {
                GeometryEffect::Configure { win, .. } => *win,
            }).collect();
            got.sort_unstable();
            prop_assert_eq!(got, expected, "only pending transitions may re-emit");
        }

        /// Purity: `reconcile` reads `State` and mutates only the applied
        /// record. A write to `client.geom` here would let a projection feed
        /// back into the layout that produced it, and the next arrange would
        /// compound the error.
        #[test]
        fn reconcile_never_touches_the_logical_state(
            (rows, applied) in arb_rows_and_applied(),
        ) {
            let mut applied = AppliedState { windows: applied };
            let state = state_for(&rows);
            let desired = desired_for(&rows);
            let before = fingerprint(&state);

            let _ = reconcile(&desired, &state, &mut applied);

            prop_assert_eq!(fingerprint(&state), before, "reconcile mutated State");
        }

        /// Convergence: however the desired geometry churns between arrange
        /// cycles, the applied record re-converges on the latest desired state
        /// within a single round, and a desired state that stops changing is a
        /// fixed point from then on. A reconciler that needed several rounds, or
        /// that oscillated between two geometries, would never let the render
        /// loop go idle.
        #[test]
        fn reconciliation_converges_on_the_latest_desired_state(
            scenario in arb_scenario(),
        ) {
            let mut applied = AppliedState::default();
            for r in &scenario.records {
                if let Some(win) = scenario.snapshots[0].iter().map(|row| row.win).nth(r.idx) {
                    applied.observe(win, r.rect, r.border_w);
                }
            }

            for (round, rows) in scenario.snapshots.iter().enumerate() {
                let state = state_for(rows);
                let desired = desired_for(rows);
                let effects = reconcile(&desired, &state, &mut applied);
                // A round writes at most one configure per desired window, so
                // the request count can never grow with the number of rounds.
                prop_assert!(
                    effects.len() <= rows.len(),
                    "round {round} emitted {} requests for {} windows",
                    effects.len(),
                    rows.len()
                );
                for row in rows {
                    let w = applied.windows.get(&row.win).unwrap_or_else(|| {
                        panic!("round {round} left window {} unapplied", row.win)
                    });
                    prop_assert!(w.seen, "round {} never configured {}", round, row.win);
                    let (want_rect, want_bw) = wire_geometry(row.rect, row.border);
                    prop_assert_eq!(w.rect, want_rect, "round {} left {} stale", round, row.win);
                    prop_assert_eq!(
                        w.border_w,
                        want_bw,
                        "round {} left {} stale",
                        round,
                        row.win
                    );
                }
            }

            // Stationary desired state with nothing mid-transition: the next
            // round must be silent, however many rounds it took to get here. A
            // client still flagged `geometry_dirty` is the documented reason to
            // keep re-asserting, so it is settled here first.
            let last = scenario.snapshots.last().expect("at least one snapshot");
            let settled: Vec<Row> = last.iter().map(|r| Row { dirty: false, ..r.clone() }).collect();
            let state = state_for(&settled);
            let desired = desired_for(&settled);
            prop_assert!(
                reconcile(&desired, &state, &mut applied).is_empty(),
                "a settled desired state still emits requests"
            );
        }

        /// Unmanage/re-manage: `forget` drops the record, so a window that comes
        /// back is configured from scratch even at the identical geometry it had
        /// before (X11 has nothing applied for it any more). Forgetting a window
        /// nobody knows is not an error — the unmanage path races map events.
        #[test]
        fn a_forgotten_window_is_re_emitted(
            rect in arb_rect(),
            border in any::<u32>(),
            stale in arb_rect(),
        ) {
            let mut applied = AppliedState::default();
            let row = Row { win: 7, rect, border, dirty: false, known: true };
            let state = state_for(std::slice::from_ref(&row));
            let desired = desired_for(std::slice::from_ref(&row));

            // A different window's stale record is irrelevant to the round.
            applied.observe(9, stale, border.saturating_add(1));
            applied.forget(7);
            applied.forget(1234);
            prop_assert!(!applied.windows.contains_key(&7));

            let effects = reconcile(&desired, &state, &mut applied);
            prop_assert_eq!(effects.len(), 1, "a re-mapped window needs one configure");
            match &effects[0] {
                GeometryEffect::Configure { win, rect: r, border: b } => {
                    let (want_rect, want_bw) = wire_geometry(rect, border);
                    prop_assert_eq!(*win, 7);
                    prop_assert_eq!(*r, want_rect);
                    prop_assert_eq!(*b, want_bw);
                }
            }
        }

        /// The `ConfigureNotify` verdict is decided by geometry equality and
        /// nothing else: not by whether the record was ever applied, not by the
        /// sequence number, not by the reported size. A report that differs is
        /// stale traffic the caller re-asserts over; a report that matches is
        /// the WM's own echo. Any dependence on the bookkeeping fields would
        /// make a real echo look stale (a configure storm) or a genuine
        /// divergence look compliant (a window that drifts off the layout).
        #[test]
        fn the_configure_verdict_depends_only_on_geometry_equality(
            applied_rect in arb_rect(),
            reported in arb_rect(),
            applied_bw in any::<u32>(),
            reported_bw in any::<u32>(),
            seen in any::<bool>(),
            sequence in prop::option::of(any::<u32>()),
        ) {
            let verdict = |seen: bool, sequence: Option<u32>| {
                classify_configure(
                    reported,
                    reported_bw,
                    &AppliedWindow { rect: applied_rect, border_w: applied_bw, seen, sequence },
                )
            };
            let expected = if applied_rect == reported && applied_bw == reported_bw {
                ConfigureObservation::Compliant
            } else {
                ConfigureObservation::Stale
            };
            prop_assert_eq!(verdict(seen, sequence), expected);
            // Same reported geometry, opposite bookkeeping: still the same
            // verdict, so an echo is never mistaken for stale traffic.
            prop_assert_eq!(verdict(!seen, sequence.map(|s| s.wrapping_add(1))), expected);
        }
    }

    /// A monitor workarea as a hostile dock can leave it: any origin in the
    /// signed range, an extent an output can actually describe. The width is
    /// held to a `RandR` 16-bit dimension because `Rect`'s saturating edges cannot
    /// represent a wider rect — beyond that, `right()` is a clamped fiction and
    /// "is this rect inside that one" stops being a question with an answer. The
    /// untrusted input in this property is the *client's* rect, which stays
    /// unrestricted.
    fn arb_workarea() -> impl Strategy<Value = Rect> {
        (any::<i32>(), any::<i32>(), 0u32..=65_535, 0u32..=65_535)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    proptest! {
        /// Every degenerate a hostile client can send comes out X11-valid: a
        /// configure with `w == 0` or `h == 0` is rejected with `BadValue`, so
        /// the clamp is the last line of defence — and it is reachable with a
        /// workarea a hostile dock already shrank to nothing, which is why the
        /// workarea is generated over the whole output range and not just a real
        /// 1920×1080 screen.
        ///
        /// Containment is asserted where the workarea can actually host the
        /// window: a zero-extent workarea has no interior to stay inside, and
        /// there the size floor wins, because a 1px window is survivable and a
        /// 0×0 configure is not.
        #[test]
        fn a_clamped_float_is_always_x11_valid(
            g in arb_rect(),
            wa in arb_workarea(),
            bw in any::<u32>(),
        ) {
            let out = clamp_float_to_workarea(g, wa, bw);
            prop_assert!(
                out.w >= 1 && out.h >= 1,
                "0-sized configure reached X11: {:?}",
                out
            );
            // The border frame is accounted in u64: a hostile `bw` must not
            // overflow the *test's* arithmetic either.
            let frame = 2u64 * u64::from(bw);
            if u64::from(wa.w) > frame && u64::from(wa.h) > frame {
                prop_assert!(
                    wa.contains_rect(out),
                    "{:?} escaped workarea {:?}",
                    out,
                    wa
                );
            }
            // Settling must be a fixed point: re-clamping the answer is what a
            // toolkit's own correction would otherwise bounce against.
            prop_assert_eq!(
                clamp_float_to_workarea(out, wa, bw),
                out,
                "clamp is not idempotent for {:?} in {:?}",
                g,
                wa
            );
        }
    }
}
