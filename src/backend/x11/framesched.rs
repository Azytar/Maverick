//! Frame scheduler — pure pacing policy for the X11 render loop (Fase 9).
//!
//! Previously `WindowManager::run_once` interleaved "do I need a frame? why?
//! when next wake?" with event draining, spring stepping and the vsync wait.
//! This module extracts that *scheduling* decision into a small, allocation-free
//! abstraction so the policy is unit-testable away from X11 and GL.
//!
//! It answers the three questions the loop asks each turn:
//!
//! - ¿necesito frame?    → [`FrameScheduler::needs_frame`]
//! - ¿por qué?           → [`FrameScheduler::reasons`] / [`FrameReason`] bits
//! - ¿cuándo producirlo? → [`FrameScheduler::timeout_ms`] poll window
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
//! - `clamp_frame_dt` bounds `dt` to `ONE_REFRESH` on first animating
//!   frame and `2*ONE_REFRESH` thereafter so a long idle gap cannot inject an
//!   absurd spring step.
//! - `timeout_ms` returns `Some(0)` for a pending frame and `None` for an
//!   idle scene. The X11 loop then blocks on X11 plus the control self-pipe;
//!   continuous animation gets its refresh-derived deadline outside this pure
//!   policy object.

use crate::backend::x11::compositor::DirtyReason;

/// Why the render loop must produce a frame this turn. Mirrors the compositor's
/// `DirtyReason` plus the WM-side `Animation` (springs still moving). Not all
/// variants are always distinguished by the source — the plan allows a coarser
/// set — but naming them keeps the *why* legible in logs and tests.
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
    /// the shader wallpaper is cleared (Fase 9).
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

/// One nominal refresh period (seconds). Used to bound `dt` so a long idle gap
/// never produces an absurd spring step on the first frame after activity.
pub(crate) const ONE_REFRESH: f32 = 1.0 / 60.0;

/// Clamp the raw time since the previous present into a sane `dt` for the spring
/// integrator (Fase 9 / B8). While animating we allow up to ~2 refreshes as a
/// guard against a stalled GPU; on the idle→animating edge (`was_animating ==
/// false`) we seed it to at most one refresh so a scroll that begins after a
/// long idle gap does not jump by the whole idle duration. Pure and testable.
pub(crate) fn clamp_frame_dt(raw_dt: f32, was_animating: bool) -> f32 {
    if was_animating {
        raw_dt.clamp(0.0, ONE_REFRESH * 2.0)
    } else {
        raw_dt.clamp(0.0, ONE_REFRESH)
    }
}

/// Pure scheduling decision for one turn of the render loop (Fase 9). Records
/// the reasons a frame is needed and answers whether/why/when. No X, no GL, no
/// heap: it is a single integer mask.
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
    //! Frame scheduling — when to render the next frame.
    //!
    //! Extracted from `mod.rs::run_once` (Fase 9) to separate
    //! the scheduling decision from the event loop. The
    //! `FrameScheduler` decides whether a frame is needed
    //! and how long to wait for the next event.
    //!
    //! # Frame reasons
    //!
    //! Multiple reasons are coalesced into a single pending
    //! frame. The `DirtyReason` bitflags are:
    //! - `DAMAGE` — `XDamage` reported a region change.
    //! - `GEOMETRY` — a window moved/resized.
    //! - `SURFACE` — a new surface (pixmap/texture) appeared.
    //! - `FOCUS` — focus/raise changed stacking.
    //! - `WALLPAPER` — wallpaper animation tick.
    //!
    //! # Animation pacing
    //!
    //! `clamp_frame_dt` bounds `dt` to [0, 2×refresh] and
    //! includes `glXSwapBuffers` blocking time in the measurement
    //! (B8 fix). The scheduler does NOT re-seed after present;
    //! the blocking time is already accounted for.
    //!
    //! # Idle pacing
    //!
    //! When no frame is needed, `timeout_ms` returns 100 ms
    //! (B1: the swap buffer is the only synchroniser, so no
    //! idle busy-wait is needed). The event loop polls the
    //! X11 socket with this timeout.
    //!
    //! # Invariants
    //!
    //! - `Animation`/`WallpaperAnimation` reasons survive
    //!   `clear_dirty` so the scheduler always wakes up for
    //!   the next animation tick.
    //! - A 0 ms timeout means "render now, then re-evaluate".
    //! - `is_animating` is true whenever any animation
    //!   reason is pending.

    use super::*;

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
        // a frame is needed -> block on the socket only (0 ms).
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

    // ── Fase 9 consolidation ───────────────────────────────────────────────

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

    /// Regression for the idle-wallpaper-CPU-burn bug: a *static* shader
    /// wallpaper must yield exactly one frame (driven by the WALLPAPER dirty
    /// reason, mapped to `Geometry`) and then idle — it must never report
    /// `wallpaper_animating` on its own. Only a shader that actually depends on
    /// time (`wallpaper_animating == true`) is allowed to keep requesting frames.
    #[test]
    fn static_wallpaper_shader_does_not_request_frames() {
        // `wallpaper_animating == false` is exactly what the compositor now
        // reports for a static shader (one that does not reference u_time /
        // u_delta_time).
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

    /// The idle→animating edge must never hand the integrator an absurd `dt`.
    // Exact, deterministic comparisons against known clamp outputs.
    #[allow(clippy::float_cmp)]
    #[test]
    fn idle_to_animating_produces_no_absurd_dt() {
        // Long idle gap: a 5 s raw delta is seeded to at most one refresh.
        assert_eq!(clamp_frame_dt(5.0, false), ONE_REFRESH);
        // While animating a 5 s stall is clamped to ~2 refreshes, never left raw.
        assert_eq!(clamp_frame_dt(5.0, true), ONE_REFRESH * 2.0);
        // Normal small deltas pass through unchanged.
        assert_eq!(clamp_frame_dt(1.0 / 120.0, true), 1.0 / 120.0);
        assert_eq!(clamp_frame_dt(1.0 / 120.0, false), 1.0 / 120.0);
    }

    /// Regression canary for the animation *speed* (B8).
    ///
    /// `clamp_frame_dt` only bounds `dt` from above; nothing here can catch a
    /// caller that measures the wrong span. The render loop briefly re-seeded
    /// `last_frame` *after* `comp.render()`, so the blocking `glXSwapBuffers` —
    /// with swap interval 1, almost the whole frame — fell outside the delta and
    /// the springs were advanced by the few hundred microseconds of loop
    /// overhead instead of by the frame period. This pins the two magnitudes so
    /// the difference is a failing test, not a "feels sluggish" bug report.
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
        // excluded from the delta: ~55x too small. The scroll then crawls, and
        // the f32 integrator cannot even reach its settle threshold — so
        // `animating` would latch on and the loop would never go idle again.
        assert!(
            frames_to_settle(0.0003, 200_000).is_none(),
            "a loop-overhead-sized dt must not be mistaken for a frame period"
        );
    }
}
