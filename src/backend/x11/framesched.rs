//! Frame scheduler — pure pacing policy for the X11 render loop.
//!
//! One turn of the loop asks three questions, and this module answers all of
//! them without touching X11 or GL so the policy stays unit-testable:
//!
//! - is a frame needed?   → [`FrameScheduler::needs_frame`]
//! - why?                → [`FrameScheduler::reasons`] / [`FrameReason`] bits
//! - when may it wait?    → [`FrameScheduler::timeout_ms`] poll window
//!
//! # Ownership & lifecycle
//!
//! `FrameScheduler` is ephemeral — constructed once per `run_once` turn via
//! [`FrameScheduler::from_compositor`] from the WM-side `animating` flag plus
//! the compositor's [`DirtyReason`] bits, consulted for both the render gate
//! and the socket poll timeout, then `clear_dirty` before the wait phase. No
//! X11 or GL state is held.
//!
//! # Protocol why
//!
//! - `clamp_frame_dt` bounds the idle→animating edge to `ONE_REFRESH` and
//!   active animation to `MAX_ANIMATION_DT`, preserving ordinary elapsed time
//!   without allowing an unbounded catch-up loop.
//! - `timeout_ms` returns `Some(0)` for a pending frame and `None` for an
//!   idle scene. The X11 loop then blocks on X11 plus the control self-pipe;
//!   continuous animation gets its refresh-derived deadline outside this pure
//!   policy object.

use crate::backend::x11::compositor::DirtyReason;

/// Why the render loop must produce a frame this turn. Mirrors the compositor's
/// `DirtyReason` plus the WM-side `Animation` (springs still moving). Not every
/// variant is distinguished at the source, but naming them keeps the *why*
/// legible in logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameReason {
    /// A camera/spring is still moving (scroll, zoom, accordion).
    Animation,
    /// A client repainted (`XDamage`).
    Damage,
    /// A window's geometry changed (configure, opacity, hide, wallpaper).
    Geometry,
    /// A surface appeared/disappeared (map, unmap, destroy).
    SurfaceChange,
    /// The stacking order changed (focus / raise / restack).
    Focus,
    /// A wallpaper shader is still animating (its `wallpaper_clock` advances).
    /// Treated like `Animation` — it keeps requesting frames every turn until
    /// the shader wallpaper is cleared.
    WallpaperAnimation,
}

impl FrameReason {
    const ALL: [FrameReason; 6] = [
        FrameReason::Animation,
        FrameReason::Damage,
        FrameReason::Geometry,
        FrameReason::SurfaceChange,
        FrameReason::Focus,
        FrameReason::WallpaperAnimation,
    ];

    /// Stable, lower-case tag for logs.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FrameReason::Animation => "animation",
            FrameReason::Damage => "damage",
            FrameReason::Geometry => "geometry",
            FrameReason::SurfaceChange => "surface",
            FrameReason::Focus => "focus",
            FrameReason::WallpaperAnimation => "wallpaper",
        }
    }

    const fn bit(self) -> u8 {
        match self {
            FrameReason::Animation => 1 << 0,
            FrameReason::Damage => 1 << 1,
            FrameReason::Geometry => 1 << 2,
            FrameReason::SurfaceChange => 1 << 3,
            FrameReason::Focus => 1 << 4,
            FrameReason::WallpaperAnimation => 1 << 5,
        }
    }
}

/// One nominal refresh period (seconds). Doubles as the seed for the
/// idle→animating edge: a long idle gap must never become the first spring
/// step after activity resumes.
pub(crate) const ONE_REFRESH: f32 = 1.0 / 60.0;

/// Upper bound (seconds) on the frame delta [`clamp_frame_dt`] keeps while a
/// transition is already running. Ordinary stalls pass through untouched; only a
/// pathological multi-second gap (a suspended process, a stopped compositor)
/// hits this guard, which keeps 15/30/60/120 Hz on one monotonic timeline
/// without an unbounded catch-up loop.
pub(crate) const MAX_ANIMATION_DT: f32 = 1.0;

/// Whether the event loop should add a software refresh wait after a present.
/// GLX swap interval 1 already blocks until vblank when `VSync` is enabled.
pub(crate) const fn should_wait_after_swap(vsync_on: bool) -> bool {
    !vsync_on
}

/// A transition which was active and settles during this tick still needs one
/// compositor frame to install its exact endpoint.
pub(crate) const fn needs_endpoint_frame(was_animating: bool, animating: bool) -> bool {
    was_animating && !animating
}

/// Bound the raw time since the previous present into a usable frame `dt`:
/// [`ONE_REFRESH`] on the idle→animating edge, [`MAX_ANIMATION_DT`] while a
/// transition is already running.
pub(crate) fn clamp_frame_dt(raw_dt: f32, was_animating: bool) -> f32 {
    if was_animating {
        raw_dt.clamp(0.0, MAX_ANIMATION_DT)
    } else {
        raw_dt.clamp(0.0, ONE_REFRESH)
    }
}

/// Pure scheduling decision for one turn of the render loop. Records the
/// reasons a frame is needed and answers whether/why/when. No X, no GL, no
/// heap: it is a single integer mask, so the whole policy is unit-testable away
/// from a display.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameScheduler {
    reasons: u8,
}

impl FrameScheduler {
    pub(crate) fn trace_bits(&self) -> u8 {
        self.reasons
    }

    pub(crate) fn new() -> Self {
        Self { reasons: 0 }
    }

    /// Build directly from the WM-side animation flag, the wallpaper animation
    /// flag, and the compositor's `DirtyReason`, mapping each compositor reason
    /// to its `FrameReason`.
    pub(crate) fn from_compositor(
        animating: bool,
        wallpaper_animating: bool,
        dirty: DirtyReason,
    ) -> Self {
        let mut s = Self::new();
        if animating {
            s.mark(FrameReason::Animation);
        }
        if wallpaper_animating {
            s.mark(FrameReason::WallpaperAnimation);
        }
        if dirty.contains(DirtyReason::DAMAGE) {
            s.mark(FrameReason::Damage);
        }
        if dirty.contains(DirtyReason::GEOMETRY) {
            s.mark(FrameReason::Geometry);
        }
        if dirty.contains(DirtyReason::SURFACE) {
            s.mark(FrameReason::SurfaceChange);
        }
        if dirty.contains(DirtyReason::FOCUS) {
            s.mark(FrameReason::Focus);
        }
        // A wallpaper (re)set is a structural, full-screen repaint — treated like
        // any other one-shot geometry change (one frame, then idle).
        if dirty.contains(DirtyReason::WALLPAPER) {
            s.mark(FrameReason::Geometry);
        }
        s
    }

    #[inline]
    pub(crate) fn mark(&mut self, r: FrameReason) {
        self.reasons |= r.bit();
    }

    #[inline]
    pub(crate) fn has(&self, r: FrameReason) -> bool {
        self.reasons & r.bit() != 0
    }

    /// Whether any reason is pending — i.e. a frame must be produced this turn.
    /// This is the single `NEED_FRAME` / `NO_FRAME` decision; both the render gate
    /// and the wait timeout read it, so no subsystem can request a redundant
    /// render in the same turn.
    #[inline]
    pub(crate) fn needs_frame(&self) -> bool {
        self.reasons != 0
    }

    /// True while a camera/spring is still moving. Distinct from `has_dirty`:
    /// `animating` keeps requesting frames every turn until the springs settle,
    /// whereas a `dirty` reason is consumed by a single present and then goes
    /// idle (unless re-marked).
    #[inline]
    pub(crate) fn is_animating(&self) -> bool {
        self.has(FrameReason::Animation)
    }

    /// True when a one-shot reason (damage/geometry/surface/focus) is pending.
    /// Such a reason produces exactly one frame; it does not keep the loop
    /// awake on its own once presented.
    #[inline]
    pub(crate) fn has_dirty(&self) -> bool {
        self.reasons & !(FrameReason::Animation.bit() | FrameReason::WallpaperAnimation.bit()) != 0
    }

    /// Drop the one-shot (dirty) reasons while preserving the `Animation` and
    /// `WallpaperAnimation` bits. Called once a present has consumed the
    /// accumulated dirty reasons: only an ongoing animation (or an animating
    /// wallpaper shader) should keep the loop tight. Keeps the scheduler the
    /// sole authority for the wait-window decision.
    #[inline]
    pub(crate) fn clear_dirty(&mut self) {
        self.reasons &= FrameReason::Animation.bit() | FrameReason::WallpaperAnimation.bit();
    }

    /// Consume this frame's damage and refresh animation state after presentation.
    /// Preparation can start a transition after the scheduler was constructed.
    pub(crate) fn after_present(&mut self, animating: bool) {
        self.clear_dirty();
        self.reasons &= !FrameReason::Animation.bit();
        if animating {
            self.mark(FrameReason::Animation);
        }
    }

    /// The reasons currently pending, as an iterator (for logs/tests).
    pub(crate) fn reasons(&self) -> impl Iterator<Item = FrameReason> + '_ {
        FrameReason::ALL
            .iter()
            .copied()
            .filter(move |r| self.has(*r))
    }

    /// True when the only pending work is an ongoing animation/shader. A dirty
    /// reason remains eligible for immediate presentation; continuous work may
    /// use a refresh-derived deadline.
    pub(crate) fn is_continuous(&self) -> bool {
        self.is_animating() || self.has(FrameReason::WallpaperAnimation)
    }

    /// How long the loop may block after this decision. A pending frame wakes
    /// immediately; a settled WM parks indefinitely on X11 plus the control
    /// self-pipe, so idle does not become a heartbeat poll.
    pub(crate) fn timeout_ms(&self) -> Option<u64> {
        self.needs_frame().then_some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invariants pinned by this module:
    //  - reasons coalesce: N marks in a turn cost exactly one frame;
    //  - `Animation`/`WallpaperAnimation` survive `clear_dirty`, the one-shot
    //    reasons do not, so only ongoing work keeps the loop tight;
    //  - `Some(0)` means "render now", `None` parks on X11 + the self-pipe;
    //  - `clamp_frame_dt` bounds the idle edge to one refresh and an already
    //    running transition to `MAX_ANIMATION_DT`.
    //
    // The property tests below cover the same ground over the *whole* input
    // space (every dirty-reason mask, both animation flags, arbitrary mark
    // sequences and arbitrary frame deltas) instead of one hand-picked turn
    // each; the examples that follow stay as the readable narrative of the
    // policy.

    use proptest::prelude::*;

    /// Every compositor dirty reason, as the render loop can report them. The
    /// mask is what `from_compositor` maps onto `FrameReason` bits, so the
    /// properties below enumerate all 32 combinations of it.
    const ALL_DIRTY: [DirtyReason; 5] = [
        DirtyReason::DAMAGE,
        DirtyReason::GEOMETRY,
        DirtyReason::SURFACE,
        DirtyReason::FOCUS,
        DirtyReason::WALLPAPER,
    ];

    /// An arbitrary set of compositor dirty reasons, drawn from the same set a
    /// real turn can report (a damage plus a focus change, a bare geometry
    /// change, nothing at all).
    fn arb_dirty() -> impl Strategy<Value = DirtyReason> {
        prop::collection::vec(
            prop::sample::select(ALL_DIRTY.to_vec()),
            0..=ALL_DIRTY.len(),
        )
        .prop_map(|parts| {
            let mut d = DirtyReason::NONE;
            for p in parts {
                d.insert(p);
            }
            d
        })
    }

    /// An arbitrary burst of frame requests within one turn.
    fn arb_marks() -> impl Strategy<Value = Vec<FrameReason>> {
        prop::collection::vec(prop::sample::select(FrameReason::ALL.to_vec()), 0..=24)
    }

    /// True when at least one compositor reason is set. `contains` is a
    /// bitwise overlap test, so it cannot be asked about `NONE` directly.
    fn any_dirty(d: DirtyReason) -> bool {
        ALL_DIRTY.iter().any(|r| d.contains(*r))
    }

    proptest! {
        /// (a)+(b) — a frame is requested *iff* the turn has work: something
        /// animating, an animating wallpaper, or a dirty reason. A settled
        /// scene with no pending reason must park on X11 plus the self-pipe (no
        /// heartbeat poll, no wasted GL work), and any pending reason must
        /// produce a frame. The reason *set* is checked too: a turn must not
        /// invent a reason it was not told about (a phantom `Animation` is what
        /// pins the compositor at full rate on a static desktop) nor lose one
        /// (`WALLPAPER` is a one-shot geometry change, not a dropped reason).
        #[test]
        fn a_frame_is_requested_exactly_when_the_turn_has_work(
            animating in any::<bool>(),
            wallpaper_animating in any::<bool>(),
            dirty in arb_dirty(),
        ) {
            let s = FrameScheduler::from_compositor(animating, wallpaper_animating, dirty);
            let has_work = animating || wallpaper_animating || any_dirty(dirty);

            prop_assert_eq!(s.needs_frame(), has_work);
            prop_assert_eq!(s.timeout_ms(), has_work.then_some(0));
            prop_assert_eq!(s.is_animating(), animating);
            prop_assert_eq!(s.has(FrameReason::WallpaperAnimation), wallpaper_animating);
            prop_assert_eq!(s.is_continuous(), animating || wallpaper_animating);
            prop_assert_eq!(s.has_dirty(), any_dirty(dirty));

            let expected = |r: FrameReason| match r {
                FrameReason::Animation => animating,
                FrameReason::WallpaperAnimation => wallpaper_animating,
                FrameReason::Damage => dirty.contains(DirtyReason::DAMAGE),
                FrameReason::SurfaceChange => dirty.contains(DirtyReason::SURFACE),
                FrameReason::Focus => dirty.contains(DirtyReason::FOCUS),
                // A wallpaper (re)set is a structural repaint, so it shares
                // the one-shot geometry reason rather than a bit of its own.
                FrameReason::Geometry => {
                    dirty.contains(DirtyReason::GEOMETRY)
                        || dirty.contains(DirtyReason::WALLPAPER)
                }
            };
            for r in FrameReason::ALL {
                prop_assert_eq!(s.has(r), expected(r), "reason {:?} mismatch", r.as_str());
            }
            prop_assert_eq!(
                s.reasons().count(),
                usize::from(expected(FrameReason::Animation))
                    + usize::from(expected(FrameReason::WallpaperAnimation))
                    + usize::from(expected(FrameReason::Damage))
                    + usize::from(expected(FrameReason::Geometry))
                    + usize::from(expected(FrameReason::SurfaceChange))
                    + usize::from(expected(FrameReason::Focus)),
                "each pending reason is reported exactly once"
            );
        }

        /// (c) — many requests in one turn coalesce: N marks (repeats
        /// included) still leave a single pending frame, the reported reasons
        /// carry no duplicates, and nothing already marked is lost when another
        /// reason arrives. A present then drops every one-shot reason while the
        /// continuous ones survive, so only ongoing work keeps the loop tight.
        #[test]
        fn reasons_coalesce_into_one_pending_frame(
            marks in arb_marks(),
        ) {
            let mut s = FrameScheduler::new();
            for &r in &marks {
                s.mark(r);
            }

            prop_assert_eq!(s.needs_frame(), !marks.is_empty());
            // One decision, not one per event: a pending frame is a single
            // immediate wake-up regardless of how many sources asked for it.
            prop_assert_eq!(s.timeout_ms(), (!marks.is_empty()).then_some(0));

            let reported: Vec<FrameReason> = s.reasons().collect();
            let mut sorted_reported = reported.clone();
            sorted_reported.sort_by_key(|r| r.as_str());
            sorted_reported.dedup();
            prop_assert_eq!(
                reported.len(),
                sorted_reported.len(),
                "a repeated reason must not be reported twice: {:?}",
                reported
            );
            let mut expected: Vec<FrameReason> = marks.clone();
            expected.sort_by_key(|r| r.as_str());
            expected.dedup();
            prop_assert_eq!(
                sorted_reported, expected,
                "the reported set is exactly what was marked"
            );

            // Marking more never unsets what is already pending.
            let before: Vec<bool> = FrameReason::ALL.iter().map(|&r| s.has(r)).collect();
            s.mark(FrameReason::Geometry);
            for (i, &r) in FrameReason::ALL.iter().enumerate() {
                prop_assert!(s.has(r) || !before[i], "reason {:?} was dropped", r.as_str());
            }

            s.clear_dirty();
            prop_assert!(!s.has_dirty(), "a present consumes every one-shot reason");
            prop_assert_eq!(s.is_continuous(), marks.contains(&FrameReason::Animation)
                || marks.contains(&FrameReason::WallpaperAnimation));
            prop_assert_eq!(s.is_animating(), marks.contains(&FrameReason::Animation));
            prop_assert_eq!(
                s.has(FrameReason::WallpaperAnimation),
                marks.contains(&FrameReason::WallpaperAnimation)
            );
        }

        /// The animation bit is a *report*, not a latch: over any sequence of
        /// turns it is exactly what the last turn said the springs were doing.
        /// A transition that starts during a frame keeps the loop awake for one
        /// more turn, and a settled one parks it — a latched bit is a WM
        /// rendering at full rate forever, and a bit that is dropped too early
        /// is a half-presented transition.
        #[test]
        fn the_animation_bit_tracks_the_last_turn_and_never_latches(
            turns in prop::collection::vec(any::<bool>(), 0..=16),
        ) {
            let mut s = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
            prop_assert_eq!(s.timeout_ms(), None, "a settled loop parks");
            for animating in &turns {
                s.after_present(*animating);
                prop_assert_eq!(s.is_animating(), *animating);
                prop_assert_eq!(s.has_dirty(), false, "a present leaves no dirty reason");
                prop_assert_eq!(s.timeout_ms(), (*animating).then_some(0));
            }
            // Whatever happened before, a settled turn parks the loop: the
            // scheduler must not be able to hold a frame open on its own.
            s.after_present(false);
            prop_assert_eq!(s.timeout_ms(), None);
            prop_assert!(!s.needs_frame());
        }

        /// (d) — the delta handed to the springs is always usable: finite, never
        /// negative, and inside the bound for the edge it is clamped for. The
        /// idle edge is never the looser of the two, so a long idle gap can
        /// never become the first spring step of a resumed transition, and an
        /// already-clamped delta is a fixed point (re-clamping the same frame
        /// twice must not drift it).
        ///
        /// `NaN` is excluded on purpose: it cannot come from `Instant`
        /// arithmetic, and `f32::clamp` propagates it, so pinning it here would
        /// assert a guarantee this policy does not claim.
        #[allow(clippy::float_cmp)]
        #[test]
        fn the_clamped_dt_is_always_a_usable_spring_step(
            raw in prop_oneof![
                any::<f32>().prop_filter("finite", |v: &f32| v.is_finite()),
                // The magnitudes a real `Instant` gap actually takes, from a
                // 240 Hz refresh to a multi-second suspend.
                Just(1.0f32 / 240.0),
                Just(1.0 / 120.0),
                Just(ONE_REFRESH),
                Just(0.25),
                Just(1.0),
                Just(60.0),
                Just(3_600.0),
                Just(f32::MAX),
            ],
            was_animating in any::<bool>(),
        ) {
            let dt = clamp_frame_dt(raw, was_animating);
            let bound = if was_animating { MAX_ANIMATION_DT } else { ONE_REFRESH };

            prop_assert!(dt.is_finite(), "raw {} produced a non-finite dt", raw);
            prop_assert!(dt >= 0.0, "a negative dt must never reach the springs");
            prop_assert!(dt <= bound, "dt {} escaped the {}s bound", dt, bound);
            prop_assert_eq!(
                clamp_frame_dt(dt, was_animating),
                dt,
                "an already-clamped dt must be a fixed point"
            );
            prop_assert!(
                clamp_frame_dt(raw, false) <= clamp_frame_dt(raw, true),
                "the idle edge must never be the looser of the two"
            );
        }
    }

    #[test]
    fn transition_started_during_frame_does_not_idle_before_next_frame() {
        let mut scheduler = FrameScheduler::from_compositor(false, false, DirtyReason::GEOMETRY);
        assert!(!scheduler.is_animating());
        scheduler.after_present(true);
        assert_eq!(scheduler.timeout_ms(), Some(0));
        assert!(!scheduler.has_dirty());
        scheduler.after_present(false);
        assert_eq!(scheduler.timeout_ms(), None);
    }

    #[test]
    fn finishing_presentation_preserves_wallpaper_animation() {
        let mut scheduler = FrameScheduler::from_compositor(true, true, DirtyReason::GEOMETRY);
        scheduler.after_present(false);
        assert!(!scheduler.is_animating());
        assert!(scheduler.has(FrameReason::WallpaperAnimation));
        assert_eq!(scheduler.timeout_ms(), Some(0));
    }

    #[test]
    fn empty_scheduler_needs_no_frame() {
        let s = FrameScheduler::new();
        assert!(
            !s.needs_frame(),
            "a fresh scheduler must not request frames"
        );
        assert_eq!(s.timeout_ms(), None, "idle parks on the 100 ms poll");
    }

    #[test]
    fn animation_alone_needs_a_frame() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Animation);
        assert!(s.needs_frame());
        assert!(s.has(FrameReason::Animation));
        assert_eq!(s.timeout_ms(), Some(0));
    }

    #[test]
    fn idle_scheduler_parks_without_a_heartbeat_timeout() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Damage);
        assert!(s.needs_frame());
        assert_eq!(s.timeout_ms(), Some(0));
    }

    #[test]
    fn from_compositor_maps_reasons() {
        let mut dirty = DirtyReason::DAMAGE;
        dirty.insert(DirtyReason::FOCUS);
        let s = FrameScheduler::from_compositor(true, false, dirty);
        assert!(s.needs_frame());
        assert!(s.has(FrameReason::Animation));
        assert!(s.has(FrameReason::Damage));
        assert!(s.has(FrameReason::Focus));
        assert!(!s.has(FrameReason::Geometry));
        assert!(!s.has(FrameReason::SurfaceChange));
    }

    #[test]
    fn reasons_iter_reports_only_pending() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Geometry);
        s.mark(FrameReason::SurfaceChange);
        let tags: Vec<&str> = s.reasons().map(super::FrameReason::as_str).collect();
        assert_eq!(tags, vec!["geometry", "surface"]);
    }

    /// Damage, Damage, Configure, Animation, Damage before the next frame must
    /// collapse into a single pending request, not five.
    #[test]
    fn coalesces_multiple_requests_into_one_pending() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Damage);
        s.mark(FrameReason::Damage);
        s.mark(FrameReason::Geometry);
        s.mark(FrameReason::Animation);
        s.mark(FrameReason::Damage);
        // One decision, not one per event.
        assert!(s.needs_frame());
        // Distinct reasons only — repeats OR into the same bit.
        let distinct: Vec<FrameReason> = s.reasons().collect();
        assert_eq!(distinct.len(), 3);
        assert!(s.is_animating());
        assert!(s.has_dirty());
        // A pending frame parks on the 0 ms socket poll.
        assert_eq!(s.timeout_ms(), Some(0));
    }

    /// A one-shot dirty reason yields exactly one frame, then idles.
    #[test]
    fn dirty_without_animation_renders_once_then_idles() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Damage);
        assert!(s.needs_frame());
        assert!(!s.is_animating());
        assert!(s.has_dirty());

        // The present consumed the dirty reason; clear it for the wait window.
        s.clear_dirty();
        assert!(
            !s.needs_frame(),
            "after presenting, a dirty-only frame idles"
        );
        assert!(!s.is_animating());
        assert!(!s.has_dirty());
        assert_eq!(s.timeout_ms(), None);
    }

    /// Animation keeps requesting frames every turn until it stops.
    #[test]
    fn animation_keeps_requesting_frames() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::Animation);
        assert!(s.needs_frame());
        assert!(s.is_animating());

        // Clearing the dirty reasons must NOT stop an animation.
        s.clear_dirty();
        assert!(
            s.needs_frame(),
            "an ongoing animation keeps the loop awake after a present"
        );
        assert!(s.is_animating());
        assert!(!s.has_dirty());
        assert_eq!(s.timeout_ms(), Some(0));
    }

    /// When the animation ends the scheduler returns to idle (the loop calls
    /// `from_compositor(false, …)` next turn, omitting the Animation bit).
    #[test]
    fn ending_animation_returns_to_idle() {
        // While animating a frame is needed.
        let running = FrameScheduler::from_compositor(true, false, DirtyReason::NONE);
        assert!(running.needs_frame());
        assert!(running.is_animating());

        // Next turn the springs have settled: no Animation bit -> idle.
        let idle = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
        assert!(
            !idle.needs_frame(),
            "settled animation must not keep rendering"
        );
        assert!(!idle.is_animating());
        assert_eq!(idle.timeout_ms(), None);
    }

    #[test]
    fn wallpaper_animation_alone_needs_a_frame() {
        let mut s = FrameScheduler::new();
        s.mark(FrameReason::WallpaperAnimation);
        assert!(s.needs_frame());
        assert!(s.has(FrameReason::WallpaperAnimation));
        assert_eq!(s.timeout_ms(), Some(0));
    }

    #[test]
    fn clear_dirty_preserves_wallpaper_animation() {
        let mut s = FrameScheduler::from_compositor(false, true, DirtyReason::DAMAGE);
        assert!(s.needs_frame());
        assert!(s.has(FrameReason::WallpaperAnimation));
        assert!(s.has(FrameReason::Damage));
        s.clear_dirty();
        // The animating wallpaper survives the present; one-shot damage does not.
        assert!(
            s.needs_frame(),
            "an animating wallpaper keeps requesting frames every turn"
        );
        assert!(s.has(FrameReason::WallpaperAnimation));
        assert!(!s.has(FrameReason::Damage));
        assert_eq!(s.timeout_ms(), Some(0));
    }

    #[test]
    fn stopping_wallpaper_shader_returns_to_idle() {
        let anim = FrameScheduler::from_compositor(false, true, DirtyReason::NONE);
        assert!(anim.needs_frame());
        assert!(anim.has(FrameReason::WallpaperAnimation));
        let idle = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
        assert!(
            !idle.needs_frame(),
            "a static wallpaper must not keep the render loop awake"
        );
        assert_eq!(idle.timeout_ms(), None);
    }

    /// A *static* shader wallpaper must yield exactly one frame (driven by the
    /// `WALLPAPER` dirty reason, mapped to `Geometry`) and then idle: it must
    /// never report `wallpaper_animating` on its own, or the loop spins at full
    /// rate on a static desktop. Only a shader that actually depends on time
    /// may keep requesting frames.
    #[test]
    fn static_wallpaper_shader_does_not_request_frames() {
        // `wallpaper_animating == false` is what the compositor reports for a
        // static shader (one referencing neither u_time nor u_delta_time).
        let s = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
        assert!(
            !s.needs_frame(),
            "a static shader wallpaper must not keep the loop awake"
        );
        assert!(!s.has(FrameReason::WallpaperAnimation));
        assert_eq!(s.timeout_ms(), None);
    }

    #[test]
    fn animated_wallpaper_shader_requests_frames() {
        // `wallpaper_animating == true` is what the compositor reports for a
        // shader that depends on time.
        let mut s = FrameScheduler::from_compositor(false, true, DirtyReason::NONE);
        assert!(s.needs_frame());
        assert!(s.has(FrameReason::WallpaperAnimation));
        assert_eq!(s.timeout_ms(), Some(0));
        // A present consumes the dirty (one-shot) reasons but the animated
        // wallpaper survives, so the loop keeps ticking…
        s.clear_dirty();
        assert!(s.needs_frame());
        // …until the compositor stops reporting it (shader removed / static).
        let stopped = FrameScheduler::from_compositor(false, false, DirtyReason::NONE);
        assert!(!stopped.needs_frame());
        assert_eq!(stopped.timeout_ms(), None);
    }

    #[test]
    fn vsync_on_relies_on_swap_instead_of_a_second_timer() {
        assert!(!should_wait_after_swap(true));
        assert!(should_wait_after_swap(false));
    }

    #[test]
    fn endpoint_transition_gets_one_terminal_frame() {
        assert!(needs_endpoint_frame(true, false));
        assert!(!needs_endpoint_frame(false, false));
        assert!(!needs_endpoint_frame(true, true));
    }

    /// The idle→animating edge must never hand the integrator an absurd `dt`;
    /// these pin the exact clamp outputs rather than a tolerance.
    #[allow(clippy::float_cmp)]
    #[test]
    fn idle_to_animating_produces_no_absurd_dt() {
        // Long idle gap: a 5 s raw delta is seeded to at most one refresh.
        assert_eq!(clamp_frame_dt(5.0, false), ONE_REFRESH);
        // Once active, elapsed time is retained through ordinary stalls; only a
        // pathological multi-second gap hits the one-second safety guard.
        assert_eq!(clamp_frame_dt(0.1, true), 0.1);
        assert_eq!(clamp_frame_dt(5.0, true), MAX_ANIMATION_DT);
        // Normal small deltas pass through unchanged.
        assert_eq!(clamp_frame_dt(1.0 / 120.0, true), 1.0 / 120.0);
        assert_eq!(clamp_frame_dt(1.0 / 120.0, false), 1.0 / 120.0);
    }

    /// Regression canary for the animation *speed*.
    ///
    /// `clamp_frame_dt` only bounds `dt` from above; nothing here can catch a
    /// caller that measures the wrong span. Re-seeding `last_frame` *after*
    /// `comp.render()` leaves the blocking `glXSwapBuffers` — with swap
    /// interval 1, almost the whole frame — outside the delta, and the springs
    /// are then advanced by the few hundred microseconds of loop overhead
    /// instead of by the frame period. This pins the two magnitudes so the
    /// difference is a failing test, not a "feels sluggish" bug report.
    #[test]
    fn springs_need_a_whole_frame_of_dt_not_the_loop_overhead() {
        use crate::types::Camera;

        /// Frames until a 500 px scroll settles, or `None` if it never does.
        fn frames_to_settle(dt: f32, cap: u32) -> Option<u32> {
            let mut cam = Camera::new(0.0);
            cam.target = 500.0;
            (1..=cap).find(|_| !cam.step(dt))
        }

        // Fed one real 60 Hz refresh, the default camera (stiffness 220,
        // damping 30) settles a 500 px scroll in a little over a second — it
        // covers 99% of the distance in ~0.5 s, which is the intended feel.
        let n = frames_to_settle(ONE_REFRESH, 10_000)
            .expect("a 500 px scroll must settle when fed a real frame period");
        let secs = n as f32 * ONE_REFRESH;
        assert!(
            (0.5..2.5).contains(&secs),
            "500 px scroll should settle in ~1.3 s at 60 Hz, took {secs:.2} s"
        );

        // 0.3 ms is the order of magnitude `dt` collapses to when the present is
        // excluded from the delta: ~55x too small. The scroll then needs far more
        // steps, but it must still retire in the same *simulated* time — the
        // spring governs convergence, not how often it is sampled. That is what
        // keeps a loop fed a loop-overhead delta from spinning indefinitely, and
        // it is the property this half of the test is really about.
        //
        // An earlier version asserted the opposite: that the f32 integrator could
        // never reach its settle threshold at this `dt`. That pinned the
        // integrator's rounding error rather than the contract, so it went red
        // once `Camera` carried its analytic state in f64 — even though the
        // behaviour it forbade (a loop that never goes idle) had been fixed.
        let small = frames_to_settle(0.0003, 200_000)
            .expect("a loop-overhead-sized dt must still converge, just in more steps");
        let small_secs = small as f32 * 0.0003;
        assert!(
            (0.5..2.5).contains(&small_secs),
            "settling must be measured in simulated seconds, not steps: took \
             {small_secs:.2} s over {small} steps"
        );
    }
}
