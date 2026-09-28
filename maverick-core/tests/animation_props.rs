//! Properties of the animation primitives: the 1D scroll `Camera`, the
//! exponential `spring_smooth` approach, and `sanitize_spring`.
//!
//! These are the only pieces of the core that are driven by wall-clock frame
//! deltas and by user-supplied spring constants, and every one of them
//! documents the same two obligations: a poisoned value must never survive a
//! step, and a step must leave the value *at* the target rather than near it.

mod common;

use common::{arb_non_finite, arb_poisoned};
use maverick_core::types::{sanitize_spring, spring_smooth, Camera};
use proptest::prelude::*;

/// A scroll offset in pixels. Bounded by what the ribbon can actually span — a
/// workarea a few screens wide times one full-width column per window, so a
/// five- or six-figure offset is already an extreme case and a four-figure one
/// is an ordinary 4K/5K session.
fn arb_scroll() -> impl Strategy<Value = f32> {
    prop_oneof![-100_000.0f32..=100_000.0, 0.0f32..=100_000.0]
}

/// A scroll velocity in pixels per second, over the same range as the offset.
fn arb_scroll_velocity() -> impl Strategy<Value = f32> {
    prop_oneof![-100_000.0f32..=100_000.0, 0.0f32..=100_000.0]
}

// A camera whose state has gone non-finite snaps back to its target instead of
// keeping the poison, whatever field was poisoned and whatever the frame delta.
//
// Invariant C depends on this: `check_invariants` rejects a camera carrying NaN,
// and this recovery is the only thing standing between a bad frame delta (or a
// config restore) and a permanently desynced compositor.
proptest! {
    #[test]
    fn a_poisoned_camera_snaps_back_to_its_target(
        // Which field carries the poison: 0 = position, 1 = target, 2 = velocity,
        // 3 = all three.
        poisoned_field in 0u8..=3,
        bad in arb_non_finite(),
        position in arb_scroll(),
        target in arb_scroll(),
        velocity in arb_scroll_velocity(),
        dt in prop_oneof![0.0f32..=0.5, arb_poisoned()],
    ) {
        let poisons_target = poisoned_field == 1 || poisoned_field == 3;
        let mut cam = Camera::new(0.0);
        cam.position = if poisoned_field == 0 || poisoned_field == 3 { bad } else { position };
        cam.target = if poisons_target { bad } else { target };
        cam.velocity = if poisoned_field == 2 || poisoned_field == 3 { bad } else { velocity };
        if !dt.is_finite() {
            // An invalid frame delta is refused outright: it must not move the
            // camera, and it must not launder the poison either.
            let before = (
                cam.position.to_bits(),
                cam.target.to_bits(),
                cam.velocity.to_bits(),
            );
            prop_assert!(!cam.step(dt), "a poisoned frame delta reported motion");
            prop_assert_eq!(
                (
                    cam.position.to_bits(),
                    cam.target.to_bits(),
                    cam.velocity.to_bits()
                ),
                before,
                "an invalid frame delta touched the camera"
            );
            return Ok(());
        }
        // Before the step repairs it, a poisoned camera must look *animated* to
        // whoever is scheduling. The gap test below is `|NaN - target| > 0.5`,
        // and a comparison against NaN is false — so a camera holding a NaN
        // reports itself settled unless the non-finite check is consulted first.
        // The scheduler polls exactly this predicate, so getting it wrong parks
        // a poisoned camera instead of asking for the repair step.
        prop_assert!(
            cam.needs_update(),
            "a camera holding a non-finite field reported itself settled: \
             pos={} target={} v={}",
            cam.position, cam.target, cam.velocity
        );
        prop_assert!(!cam.step(dt), "a recovered camera reported motion");
        prop_assert!(cam.position.is_finite(), "position stayed poisoned: {}", cam.position);
        prop_assert!(cam.target.is_finite(), "target stayed poisoned: {}", cam.target);
        prop_assert!(cam.velocity.is_finite(), "velocity stayed poisoned: {}", cam.velocity);
        prop_assert_eq!(cam.velocity, 0.0, "a recovered camera kept momentum");
        // Documented: the recovery snaps onto the *target*, and only falls back to
        // zero when the target itself is the poisoned field.
        let endpoint = if poisons_target { 0.0 } else { target };
        prop_assert_eq!(cam.position, endpoint, "the camera did not recover onto its target");
        prop_assert_eq!(cam.target, endpoint, "the recovered target is not a finite endpoint");
    }
}

// Retargeting changes the destination and nothing else: the animated position is
// retained so the ribbon never teleports, and stale momentum is dropped so a
// reversal does not first accelerate away from the new destination.
proptest! {
    #[test]
    fn retarget_keeps_the_visual_position_and_drops_stale_momentum(
        position in arb_scroll(),
        velocity in arb_scroll_velocity(),
        (old_target, new_target) in prop_oneof![
            (arb_scroll(), arb_scroll()),
            // Repeated notifications for the endpoint the camera is already
            // heading to: the documented case that must *not* cancel the spring.
            (arb_scroll(), prop_oneof![Just(0.0f32), Just(1e-5), Just(-1e-5), Just(1e-3), Just(2.0)])
                .prop_map(|(old, delta)| (old, old + delta)),
        ],
    ) {
        let mut cam = Camera::new(position);
        cam.velocity = velocity;
        cam.target = old_target;

        cam.retarget(new_target);
        prop_assert_eq!(cam.position, position, "retarget teleported the camera");
        prop_assert_eq!(cam.target, new_target, "retarget did not move the destination");
        if (old_target - new_target).abs() > 1e-4 {
            prop_assert_eq!(cam.velocity, 0.0, "retarget kept momentum from the old direction");
        } else {
            prop_assert_eq!(cam.velocity, velocity, "a repeated notification cancelled an in-flight spring");
        }
    }
}

/// Why `retarget` exists, and what bypassing it costs.
///
/// `retarget` is the only sanctioned way to move the destination, and its
/// contract includes dropping stale momentum so a reversal does not first carry
/// the camera *further* in the direction it was already going. The `unmanage`
/// teardown is exactly that case: closing a window under a held scroll key
/// re-derives the destination for the shorter ribbon while the spring is still
/// in flight, and it used to reach into the `target` field directly.
///
/// The consequence is not overshoot — the scroll spring is at ζ ≈ 1.01, so it
/// does not overshoot at all, and a test claiming it did would be wrong. The
/// consequence is *direction*: with the velocity kept, the ribbon keeps sliding
/// the way the user was already scrolling while the destination has been placed
/// behind it, and only turns around once its own acceleration catches up. That
/// is the visible glitch, and it is measured here as the first frame's movement.
#[test]
fn a_reversal_through_retarget_turns_around_immediately_and_a_field_write_does_not() {
    // A scroll in flight, as the teardown finds it: 2962 px from the origin,
    // heading for 11 448 at 61 997 px/s. The window closes and the destination
    // for the shorter ribbon is back at 0.
    let (position, old_target, velocity, new_target) =
        (2962.536f32, 11_448.0f32, 61_997.457f32, 0.0f32);

    let one_frame = |mut cam: Camera| -> f32 {
        cam.step(1.0 / 60.0);
        cam.position
    };

    // Writing the field keeps the velocity, so the camera travels a further
    // ~1033 px *away* from a destination that now sits behind it.
    let mut raw = Camera::new(position);
    raw.target = old_target;
    raw.velocity = velocity;
    raw.target = new_target;
    let raw_after = one_frame(raw);
    assert_eq!(
        raw.velocity, velocity,
        "the raw write consumed the momentum"
    );
    assert!(
        raw_after > position,
        "with the destination behind it at {new_target}, the camera went from \
         {position} to {raw_after} — it kept scrolling the old way"
    );

    // Going through the sanctioned entry point drops the momentum, so the very
    // first frame already moves toward the new destination.
    let mut fixed = Camera::new(position);
    fixed.target = old_target;
    fixed.velocity = velocity;
    fixed.retarget(new_target);
    let fixed_after = one_frame(fixed);
    assert_eq!(
        fixed.velocity, 0.0,
        "retarget kept momentum from the old direction"
    );
    assert!(
        fixed_after < position,
        "a retarget onto {new_target} must move the camera toward it from the \
         first frame, but it went {position} -> {fixed_after}"
    );
}

// A non-finite destination is refused outright, not stored: the compositor reads
// `target` for settled geometry, so a poisoned target would move every tile.
proptest! {
    #[test]
    fn retarget_refuses_a_poisoned_destination(
        position in arb_scroll(),
        target in arb_scroll(),
        velocity in arb_scroll_velocity(),
        bad in arb_non_finite(),
    ) {
        let mut cam = Camera::new(position);
        cam.target = target;
        cam.velocity = velocity;
        cam.retarget(bad);
        prop_assert_eq!(cam.target, target, "a poisoned destination was stored");
        prop_assert_eq!(cam.position, position, "a refused retarget moved the camera");
        prop_assert_eq!(cam.velocity, velocity, "a refused retarget changed momentum");
    }
}

// `sanitize_spring` maps every parameter into the effective domain the analytic
// step is stable in: a positive bounded stiffness, and a damper bounded both
// below and relative to `sqrt(stiffness)` so an overdamped pole can never be slow
// enough to keep a settled camera animating forever. Applying it to its own
// output is a no-op, which is what lets the backend sanitize at configure time
// and the camera re-sanitize at every step.
proptest! {
    #[test]
    fn sanitize_spring_lands_in_the_effective_domain_and_is_idempotent(
        stiffness in prop_oneof![any::<f32>(), 0.0f32..=1.0e9, Just(0.0), Just(-100.0)],
        damping in prop_oneof![any::<f32>(), 0.0f32..=1.0e9, Just(0.0), Just(-25.0)],
    ) {
        let (k, c) = sanitize_spring(stiffness, damping);
        prop_assert!(k.is_finite() && c.is_finite(), "sanitize produced a non-finite spring");
        prop_assert!((1.0..=62_500.0).contains(&k), "stiffness {} is outside the effective domain", k);
        prop_assert!(c >= 0.1, "damping {} is below the numerical floor", c);
        prop_assert!(c <= 10.0 * k.sqrt(), "damping {} is unbounded relative to stiffness {}", c, k);
        if !stiffness.is_finite() || stiffness <= 0.0 {
            prop_assert_eq!(k, 220.0, "a rejected stiffness did not fall back to the default");
        }
        let (k2, c2) = sanitize_spring(k, c);
        prop_assert_eq!(k2, k, "sanitize is not idempotent in stiffness");
        prop_assert_eq!(c2, c, "sanitize is not idempotent in damping");
    }
}

// A rejected damper is ignored, not blended: every unusable damping value maps to
// the same one, so a config typo cannot turn into an arbitrary spring.
proptest! {
    #[test]
    fn a_rejected_damper_always_yields_the_same_spring(
        stiffness in 1.0f32..=62_500.0,
        bad in prop_oneof![Just(0.0f32), Just(-1.0), Just(-1.0e9), Just(f32::NAN), Just(f32::INFINITY), Just(f32::NEG_INFINITY)],
    ) {
        let a = sanitize_spring(stiffness, bad);
        let b = sanitize_spring(stiffness, f32::NAN);
        let c = sanitize_spring(stiffness, f32::INFINITY);
        prop_assert_eq!(a.1, b.1, "the rejected damper leaked into the spring");
        prop_assert_eq!(a.1, c.1, "the rejected damper leaked into the spring");
        prop_assert_eq!(a.0, stiffness, "sanitizing the damper changed the stiffness");
    }
}

// A scroll offset small enough that the settle envelope is representable in f32.
//
// The camera stores its offset as f32, whose unit in the last place grows with
// the magnitude: about 0.0001 px at 2^10, 0.002 px at 2^14 and 0.016 px at
// 10^5. Once the residual falls to one ULP there is nothing left for the spring
// to cancel, so the sub-pixel envelope `needs_update` tests can no longer be
// satisfied. The exact-endpoint contract below is therefore asserted over the
// range where it is a statement about the spring rather than about float
// precision; "arrives within a pixel" is asserted over the whole range
// separately, and the gap between them is recorded as a defect.
fn arb_settling_scroll() -> impl Strategy<Value = f32> {
    prop_oneof![-1_024.0f32..=1_024.0, 0.0f32..=1_024.0]
}

/// A `(stiffness, damping)` pair as raw, possibly hostile user input.
///
/// `sanitize_spring` is what the camera's own documentation means by "sanitized
/// at integration time": it maps every input — non-finite, zero, negative —
/// into the effective `(k, c)` domain, and is itself idempotent. The
/// convergence properties below feed the result to `step`, because asserting
/// that an *unsanitized* spring converges would be asserting something the
/// documented contract deliberately does not promise: an undamped oscillator
/// (damping exactly 0) genuinely never settles, and no implementation could
/// make it.
fn arb_raw_spring() -> impl Strategy<Value = (f32, f32)> {
    (
        prop_oneof![
            0.0f32..=62_500.0,
            Just(220.0),
            Just(1.0),
            Just(0.0),
            Just(-100.0),
            Just(f32::NAN),
            Just(f32::INFINITY),
            Just(f32::NEG_INFINITY),
        ],
        prop_oneof![
            0.0f32..=40.0,
            Just(30.0),
            0.0f32..=0.2,
            Just(0.0),
            Just(-25.0),
            Just(f32::NAN),
            Just(f32::INFINITY),
            Just(f32::NEG_INFINITY),
        ],
    )
}

// Every camera reaches its destination: whatever the caller wrote straight into
// the public fields, stepping with a positive frame delta must terminate with
// the exact endpoint installed, not merely "not explode".
//
// The step budget is a watchdog against a hung test; the assertion that matters
// is the endpoint and the parked frame request, because those are what the
// compositor's scheduler acts on.
proptest! {
    #[test]
    fn a_camera_always_converges_onto_its_target(
        position in arb_settling_scroll(),
        target in arb_settling_scroll(),
        velocity in arb_scroll_velocity(),
        (raw_stiffness, raw_damping) in arb_raw_spring(),
    ) {
        let (stiffness, damping) = sanitize_spring(raw_stiffness, raw_damping);
        let mut cam = Camera::new(position);
        cam.target = target;
        cam.velocity = velocity;
        cam.stiffness = stiffness;
        cam.damping = damping;

        let mut steps = 0u32;
        while cam.step(1.0 / 60.0) {
            steps += 1;
            prop_assert!(cam.position.is_finite(), "position diverged: {}", cam.position);
            prop_assert!(cam.velocity.is_finite(), "velocity diverged: {}", cam.velocity);
            prop_assert!(
                steps <= 40_000,
                "still animating after {} frames: {} px from the target at {} px/s",
                steps,
                cam.position - cam.target,
                cam.velocity
            );
        }
        prop_assert_eq!(cam.position, target, "the camera stopped away from its target");
        prop_assert_eq!(cam.velocity, 0.0, "the camera stopped with momentum");
        prop_assert!(!cam.needs_update(), "a settled camera still asks for frames");
    }
}

// Across the whole representable offset range — including the five-figure
// virtual desktops the first property has to exclude — the camera stays bounded
// and finite, however hostile the spring it was handed.
//
// This deliberately asserts no *time* bound and does not require the transition
// to finish. `Camera::step` carries no frame limit: it reports "still animating"
// and the compositor schedules another frame, so a slow spring taking its time
// is the documented behaviour rather than a defect, and a camera that has
// arrived but not yet parked is a separate defect recorded below. What must
// hold at every scale, over a fixed horizon, is that the arithmetic never
// escapes: a spring launched from a large offset with a large velocity can
// overshoot by roughly the distance it was thrown, but it can never run away to
// infinity or go non-finite. That is the difference between a slow transition
// and an unstable one, and it is what this pins.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_camera_stays_bounded_at_any_magnitude(
        position in arb_scroll(),
        target in arb_scroll(),
        velocity in arb_scroll_velocity(),
        (raw_stiffness, raw_damping) in arb_raw_spring(),
    ) {
        let (stiffness, damping) = sanitize_spring(raw_stiffness, raw_damping);
        let mut cam = Camera::new(position);
        cam.target = target;
        cam.velocity = velocity;
        cam.stiffness = stiffness;
        cam.damping = damping;

        // A spring cannot carry the camera further than the offset it started
        // from plus the distance its initial velocity would throw it, with slack
        // for the closed-form step overshooting by up to one period.
        let reach = (position - target).abs() + velocity.abs() * 2.0 + 1.0;
        for step in 1..=2_000 {
            let _ = cam.step(1.0 / 60.0);
            prop_assert!(cam.position.is_finite(), "position diverged at step {step}: {}", cam.position);
            prop_assert!(cam.velocity.is_finite(), "velocity diverged at step {step}: {}", cam.velocity);
            prop_assert!(cam.target.is_finite(), "target diverged at step {step}: {}", cam.target);
            prop_assert!(
                (cam.position - cam.target).abs() <= reach,
                "the camera ran away to {} px after {step} frames (reach {reach})",
                (cam.position - cam.target).abs()
            );
        }
    }
}

// The documented promise is stronger than "arrives": "damping is also bounded
// relative to sqrt(stiffness) so a slow overdamped pole cannot keep a
// pixel-settled camera active indefinitely". A camera that has arrived is
// pixel-settled, so it must also stop asking the compositor's scheduler for
// frames — at any offset, with any spring the sanitizers accept.
//
// The offset is the reason this is interesting: the residual falls to the f32
// unit in the last place long before `CAMERA_SETTLE_POSITION` (0.5 px) is in
// play, so whether the camera can park at all is decided by a state whose
// resolution is `ulp(position)`. Re-seeding each step from that rounded position
// gave the trajectory a speed floor of roughly `k/c · ½ · ulp(position)`, which
// passes `CAMERA_SETTLE_VELOCITY` (0.01 px/s) at around 12 000 px — three
// full-width columns on a 4K workarea, or twelve on a 1024 px one, so it was
// reachable in ordinary sessions. Integrating the f64 continuation instead makes
// the envelope reachable everywhere, and the least-damped springs — whose floor
// is the highest, and which were frozen *outside* the envelope, 1 to 2 px short
// of the target — park too.
proptest! {
    #[test]
    fn a_camera_parks_itself_however_far_it_travelled(
        position in arb_scroll(),
        target in arb_scroll(),
        velocity in arb_scroll_velocity(),
        (raw_stiffness, raw_damping) in arb_raw_spring(),
    ) {
        let (stiffness, damping) = sanitize_spring(raw_stiffness, raw_damping);
        let mut cam = Camera::new(position);
        cam.target = target;
        cam.velocity = velocity;
        cam.stiffness = stiffness;
        cam.damping = damping;

        let mut steps = 0u32;
        while cam.step(1.0 / 60.0) {
            steps += 1;
            prop_assert!(steps <= 40_000, "still animating after {steps} frames");
        }
        prop_assert!(!cam.needs_update(), "a settled camera still asks for frames");
    }
}

// A camera that has already arrived stops asking for frames, and a zero or
// negative elapsed interval makes no progress at all.
//
// The compositor schedules a frame only while `needs_update()` is true, so the
// settle predicate is what lets an idle session park; and a zero-resolution
// `Instant` must not be mistaken for "arrived" either, or a pending transition
// would be dropped without ever being drawn.
proptest! {
    #[test]
    fn arrival_is_reported_honestly_and_a_non_positive_delta_never_moves_the_camera(
        target in arb_scroll(),
        offset in 0.0f32..=2.0,
        dt in prop_oneof![1.0e-9f32..=0.5, Just(0.0), Just(-1.0)],
    ) {
        let mut cam = Camera::new(0.0);
        cam.target = target;
        cam.position = target + offset;
        cam.velocity = 0.0;
        let gap = (cam.position - cam.target).abs();
        prop_assert_eq!(!cam.needs_update(), gap <= 0.5, "the settle envelope moved");

        if dt <= 0.0 {
            // No progress, and the pending-transition report is unchanged.
            let animating = cam.needs_update();
            let before = (cam.position.to_bits(), cam.target.to_bits(), cam.velocity.to_bits());
            prop_assert_eq!(cam.step(dt), animating, "a non-positive delta changed the camera's own verdict");
            prop_assert_eq!(
                (cam.position.to_bits(), cam.target.to_bits(), cam.velocity.to_bits()),
                before,
                "a non-positive delta moved the camera"
            );
        } else if !cam.step(dt) {
            prop_assert_eq!(cam.position, target, "a settled camera did not land exactly");
        }
    }
}

// The settle envelope's own boundary.
//
// `arrival_is_reported_honestly_and_a_non_positive_delta_never_moves_the_camera`
// draws its offset uniformly from `0.0..=2.0`, so it lands exactly on the
// threshold only by luck, and the inclusive/exclusive choice at `gap == 0.5`
// and `|velocity| == 0.01` is therefore unobserved — `>` and `>=` agree on
// every input that test generates. That boundary is a real decision: it is what
// separates "one more frame of animation" from "install the exact endpoint", and
// it is the predicate the frame scheduler polls.
//
// These are probed by bit pattern rather than by sampling, because the values
// that matter are the neighbours of a threshold, and a uniform distribution over
// reals almost never produces one.
const SETTLE_POSITION: f32 = 0.5; // CAMERA_SETTLE_POSITION, px
const SETTLE_VELOCITY: f32 = 0.01; // CAMERA_SETTLE_VELOCITY, px/s

/// The `f32` `k` representable steps away from `v`, by construction rather than
/// by arithmetic — `0.5` is exact but `0.01` is not, so `0.01 + 1e-9` is just
/// `0.01` and only the bit pattern names its neighbour.
fn ulp_away(v: f32, k: i32) -> f32 {
    f32::from_bits((v.to_bits() as i32 + k) as u32)
}

/// A camera holding a non-finite field must look *animated* to whoever is
/// scheduling it, whatever else it holds.
///
/// This is not a restatement of the recovery path: `step` repairs a poisoned
/// camera, but the frame scheduler polls `needs_update` to decide whether to
/// ask for that step at all. A camera that reports itself settled is never
/// stepped, so it is never repaired — the poison simply persists, and whatever
/// divides by it produces garbage. The comparison is `|position - target| >
/// 0.5`, and every comparison against a NaN gap is *false*, so the
/// non-finite check is the only thing standing between a NaN camera and a
/// scheduler that thinks it is done.
///
/// The velocity is deliberately at rest here, so the verdict cannot be reached
/// through the motion test at all: the finiteness check is the sole thing
/// deciding the answer. The poison is NaN rather than an infinity for the same
/// reason — an infinite *target* still makes `|position - target| > 0.5` true,
/// so the verdict would be reached by accident and the finiteness check would go
/// unexercised. A NaN gap compares false against everything, which is exactly
/// the case the check exists to catch.
#[test]
fn a_camera_poisoned_in_any_single_field_still_looks_animated() {
    for poisoned in ["position", "target", "velocity"] {
        let mut cam = Camera::new(0.0);
        // At rest, and finite in every field this case does not poison.
        cam.position = 1_000.0;
        cam.target = 1_000.0;
        cam.velocity = 0.0;
        match poisoned {
            "position" => cam.position = f32::NAN,
            "target" => cam.target = f32::NAN,
            _ => cam.velocity = f32::NAN,
        }
        assert!(
            cam.needs_update(),
            "a camera poisoned in {poisoned} reported itself settled, so the \
             scheduler would never ask for the step that repairs it"
        );
    }
}

#[test]
fn a_camera_exactly_at_both_settle_thresholds_is_settled() {
    for k in [-2i32, -1, 0] {
        let gap = ulp_away(SETTLE_POSITION, k);
        let vel = ulp_away(SETTLE_VELOCITY, k);
        for (d, v) in [(gap, vel), (gap, 0.0), (0.0, vel)] {
            let mut cam = Camera::new(0.0);
            cam.position = d;
            cam.target = 0.0;
            cam.velocity = v;
            assert!(
                !cam.needs_update(),
                "a camera at gap={d} ({:?}) and |v|={v} ({:?}) is inside the \
                 envelope, so it must report settled",
                d.to_bits(),
                v.to_bits()
            );
        }
    }
}

#[test]
fn one_representable_step_outside_a_settle_threshold_still_animates() {
    for k in [1i32, 2] {
        let gap = ulp_away(SETTLE_POSITION, k);
        let vel = ulp_away(SETTLE_VELOCITY, k);
        for (d, v) in [(gap, vel), (gap, 0.0), (0.0, vel)] {
            let mut cam = Camera::new(0.0);
            cam.position = d;
            cam.target = 0.0;
            cam.velocity = v;
            assert!(
                cam.needs_update(),
                "a camera at gap={d} ({:?}) or |v|={v} ({:?}) is outside the \
                 envelope, so it must still animate",
                d.to_bits(),
                v.to_bits()
            );
        }
    }
}

// The velocity half of the predicate, on its own, and with the sign the caller
// actually produces: `step` publishes a signed velocity, and `abs` is what makes
// the verdict direction-independent.
#[test]
fn the_settle_verdict_does_not_depend_on_the_direction_of_motion() {
    for v in [
        ulp_away(SETTLE_VELOCITY, -1),
        SETTLE_VELOCITY,
        ulp_away(SETTLE_VELOCITY, 1),
    ] {
        for sign in [1.0f32, -1.0] {
            let mut cam = Camera::new(0.0);
            cam.velocity = v * sign;
            assert_eq!(
                cam.needs_update(),
                v > SETTLE_VELOCITY,
                "|v|={v} travelling {} must have the same verdict as the other way",
                if sign > 0.0 { "right" } else { "left" }
            );
        }
    }
}

//
// The closed form (`k` saturating to 1 for a long frame) exists precisely
// because the Euler step it replaced overshot for long frames: an overshooting
// column boost would make the ribbon breathe instead of settling.
proptest! {
    #[test]
    fn spring_smooth_converges_without_poisoning_or_overshooting(
        start in prop_oneof![arb_animatable(), arb_poisoned()],
        target in prop_oneof![arb_animatable(), arb_poisoned()],
        dt in prop_oneof![0.0f32..=0.5, 0.0f32..=0.001, Just(0.0), Just(-1.0), arb_poisoned()],
    ) {
        let mut cur = start;
        for _ in 0..8 {
            let prev = cur;
            let before = cur.to_bits();
            let moving = spring_smooth(&mut cur, target, dt);
            if !dt.is_finite() {
                prop_assert_eq!(cur.to_bits(), before, "an invalid frame delta moved the value");
                prop_assert!(!moving, "an invalid frame delta reported motion");
                continue;
            }
            prop_assert!(cur.is_finite(), "spring_smooth poisoned the value (target {})", target);
            // The interval claim is about the values this function animates: a
            // column boost and a zoom factor, both O(1). A `cur` twenty-odd
            // orders of magnitude larger than the target is not a boost or a
            // zoom, and f32 simply cannot add a step that small to it.
            if !(dt > 0.0 && target.is_finite() && prev.is_finite() && animatable_magnitude(prev)) {
                continue;
            }
            // Between the two, never past them: no overshoot means the distance
            // to the target can only shrink, and a reported settle is exact.
            let (lo, hi) = if prev <= target { (prev, target) } else { (target, prev) };
            let tol = hi.abs().max(1.0) * 1e-6;
            prop_assert!(
                cur >= lo - tol && cur <= hi + tol,
                "the spring left the interval [{}, {}] (prev {}, target {})",
                lo,
                hi,
                prev,
                target
            );
            if !moving {
                prop_assert_eq!(cur, target, "a settled spring is not exactly on its target");
            }
        }
    }
}

/// The magnitudes `spring_smooth` is actually asked to animate: a column boost
/// and a zoom factor, both O(1).
fn arb_animatable() -> impl Strategy<Value = f32> {
    prop_oneof![
        0.0f32..=4.0,
        -4.0f32..=0.0,
        Just(0.0),
        Just(1.0),
        Just(-1.0)
    ]
}

/// Whether a value is still in the range of an animated boost or zoom factor.
fn animatable_magnitude(v: f32) -> bool {
    v.abs() <= 1024.0
}
