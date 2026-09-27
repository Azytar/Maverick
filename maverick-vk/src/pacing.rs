//! In-flight frame bookkeeping, with no Vulkan call in it.
//!
//! # Why this exists
//!
//! `vkResetFences` takes the frame-in-flight fence back to unsignalled and
//! `vkQueueSubmit` signals it again when the submitted batch finishes. Between
//! those two calls the fence is unsignalled, and a `vkWaitForFences` issued in
//! that window waits for a signal that no submission is going to send.
//!
//! The window is unavoidable in a frame, because the steps between the reset
//! and the submit are exactly the steps that can fail: `vkAcquireNextImageKHR`
//! returns `VK_ERROR_OUT_OF_DATE_KHR` whenever the window has been resized
//! since the last frame — the ordinary outcome of a resize, not an exotic one —
//! and `vkQueueSubmit` can return `VK_ERROR_OUT_OF_HOST_MEMORY`. A frame that
//! returns early from either leaves the fence unsignalled, and because a
//! binary fence cannot be re-signalled by the host, every later frame would
//! block on a wait nothing can satisfy. `Vulkan::acquire_and_present` passes
//! `u64::MAX` as its timeout, so that mistake is a hang rather than an error.
//!
//! So the fence is reset immediately before the submit that will signal it, and
//! whether a wait is owed is tracked here instead of inferred from the fence's
//! current state — which the host cannot read without a query, and which does
//! not distinguish "nobody has submitted yet" from "the submit failed" anyway.
//!
//! # The invariant
//!
//! Exactly one submission may be outstanding against the fence at a time, which
//! is what `VUID-vkQueueSubmit-fence-00064` requires ("fence must not be
//! associated with any other queue command that has not yet completed execution
//! on that queue"), and a wait is issued only while one is outstanding, which is
//! what keeps the wait from outliving its signal. The state is a single bit
//! because those two facts are the same fact: a submission that is outstanding is
//! the only thing that can signal the fence, and nothing else can.
//!
//! The handle is stored so the frame loop cannot pass a different fence to the
//! submit than the one it waits on, but it is never dereferenced, compared for
//! identity or otherwise inspected — which is why the state machine can be
//! exercised against fabricated handles with no device present.

use ash::vk;

/// The frame-in-flight fence, plus whether a submission against it is still
/// outstanding.
///
/// See the module docs for the invariant. Construct with [`FrameFence::new`]
/// immediately after creating the fence, and pair every call to
/// [`FrameFence::take_completion`] that returns `true` with exactly one
/// [`FrameFence::submitted`], on the path where `vkQueueSubmit` succeeded.
pub(crate) struct FrameFence {
    handle: vk::Fence,
    submit_outstanding: bool,
}

impl FrameFence {
    /// Take ownership of a fence that was created with
    /// `VK_FENCE_CREATE_SIGNALED_BIT`.
    ///
    /// The fence being signalled means the first frame has nothing to wait for,
    /// so no submission is recorded as outstanding yet.
    pub(crate) fn new(handle: vk::Fence) -> Self {
        Self {
            handle,
            submit_outstanding: false,
        }
    }

    /// The handle to pass to `vkWaitForFences`, `vkResetFences`,
    /// `vkQueueSubmit` and `vkDestroyFence`.
    pub(crate) fn handle(&self) -> vk::Fence {
        self.handle
    }

    /// Collect the completion the previous frame's submission owes this one,
    /// reporting whether a `vkWaitForFences` is owed.
    ///
    /// Returning `false` means no submission has been made since the last time
    /// this was called: the first frame, or a frame that failed before its
    /// submit was handed the fence. In either case the fence carries no signal
    /// from this crate and waiting on it would never return.
    ///
    /// The debt is settled even if the `vkWaitForFences` that honours it then
    /// fails, which is why the flag is cleared here rather than by the caller.
    pub(crate) fn take_completion(&mut self) -> bool {
        let owed = self.submit_outstanding;
        self.submit_outstanding = false;
        owed
    }

    /// Record that `vkQueueSubmit` was handed [`Self::handle`] and succeeded, so
    /// the fence carries a signal and the next frame owes a wait.
    ///
    /// Only for the success path. A failed submit enqueues nothing, so the fence
    /// keeps whatever state it had and the next frame owes nothing — recording
    /// one here would be a wait with no signal behind it.
    pub(crate) fn submitted(&mut self) {
        self.submit_outstanding = true;
    }
}

// The bookkeeping is the one part of the frame loop that is pure, so it is
// exercised here rather than in `tests/`: `FrameFence` is crate-private and
// making it public to reach it from an integration test would commit this crate
// to an API it does not otherwise need. `#[cfg(test)]` keeps the test out of
// the library build entirely, so `proptest` stays a dev-dependency. It is also
// the only place the invariant is checked at all: on a real device it is
// enforced by a GPU hang, which no always-runnable test could observe.
#[cfg(test)]
mod tests {
    use super::FrameFence;
    use ash::vk::{Fence, Handle};
    use proptest::prelude::*;

    /// A handle to hand the state machine in place of a real `VkFence`. The
    /// bookkeeping never dereferences or compares the handle, so any value
    /// stands in for a live fence.
    fn fake_fence(raw: u64) -> Fence {
        Fence::from_raw(raw)
    }

    /// How a frame ended, from the fence's point of view. The three cases are
    /// the three points a frame can give up at, and the middle one is the one
    /// that matters: it is the only state in which the fence is unsignalled and
    /// nothing is going to signal it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        /// Gave up before the reset — a failed `vkAcquireNextImageKHR`, a failed
        /// `vkBeginCommandBuffer` or `vkEndCommandBuffer`. The fence is
        /// untouched.
        FailedBeforeReset,
        /// Gave up between the reset and the submit — `vkResetFences` or
        /// `vkQueueSubmit` reported an error. The fence is unsignalled and no
        /// submission will signal it.
        FailedAfterReset,
        /// `vkQueueSubmit` succeeded, so the fence carries a signal.
        Submitted,
    }

    fn outcome() -> impl Strategy<Value = Outcome> {
        prop_oneof![
            Just(Outcome::FailedBeforeReset),
            Just(Outcome::FailedAfterReset),
            Just(Outcome::Submitted),
        ]
    }

    /// The fence as the driver sees it, kept alongside the state machine so the
    /// two can be held against each other. `signalled` is what a
    /// `vkWaitForFences` would find; `held` is how many submissions have been
    /// given the fence and not yet waited for, which the spec allows to be at
    /// most one.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DriverView {
        signalled: bool,
        held: bool,
    }

    impl DriverView {
        /// A fence created with `VK_FENCE_CREATE_SIGNALED_BIT`, with no
        /// submission against it.
        fn new() -> Self {
            Self {
                signalled: true,
                held: false,
            }
        }

        /// What the driver does when the frame resets the fence before its
        /// submit: the fence goes unsignalled. Resetting is legal only while no
        /// submission is using the fence.
        fn reset(&mut self) {
            assert!(
                !self.held,
                "vkResetFences on a fence a submission still owns \
                 (VUID-vkResetFences-pFences-01123)"
            );
            self.signalled = false;
        }

        /// What the driver does when the submitted batch finishes: the fence
        /// signal operation completes and the fence is signalled again.
        fn submit_completes(&mut self) {
            self.signalled = true;
            self.held = true;
        }
    }

    /// The top of a frame: settle the completion the previous frame owes, if
    /// there is one, against what the driver would actually find.
    ///
    /// Returns whether a `vkWaitForFences` was issued. A wait does not change
    /// the fence's state; it only retires this frame's debt to it, which is why
    /// the driver's `held` goes false here and `signalled` does not.
    ///
    /// The `assert!`s are the whole point of the harness. The first holds the
    /// bookkeeping against the driver's own record of what it was asked to do,
    /// and the second is the hang: a wait is only legal while the fence carries a
    /// signal, and `u64::MAX` turns an illegal one into a process that never
    /// returns.
    fn collect(frame_fence: &mut FrameFence, driver: &mut DriverView) -> bool {
        let owed = frame_fence.take_completion();
        assert_eq!(
            owed, driver.held,
            "the bookkeeping and the driver disagree about whether a completion is outstanding"
        );
        if owed {
            assert!(
                driver.signalled,
                "the frame waited on a fence no submission ever signalled, so the wait would \
                 block forever"
            );
            driver.held = false;
        }
        owed
    }

    /// One frame and the driver's view of it afterwards: settle the previous
    /// completion, then reset the fence and either submit it or give up.
    fn run_frame(frame_fence: &mut FrameFence, driver: &mut DriverView, outcome: Outcome) {
        collect(frame_fence, driver);
        match outcome {
            Outcome::FailedBeforeReset => {}
            Outcome::FailedAfterReset => driver.reset(),
            Outcome::Submitted => {
                driver.reset();
                driver.submit_completes();
                frame_fence.submitted();
            }
        }
    }

    proptest! {
        /// Whatever a run of frames does, the fence the frame after it waits on
        /// is one a submission signalled — so `vkWaitForFences` with this
        /// crate's `u64::MAX` timeout cannot outlive its signal — and no frame
        /// submits against a fence an earlier submission still owns.
        ///
        /// `FailedAfterReset` is the outcome that needs the bookkeeping: it
        /// leaves the fence unsignalled with nothing left to signal it, and the
        /// next frame has to be able to tell that apart from a fence whose
        /// submit is still running. A frame that cannot tell the two apart waits
        /// on a signal that will never arrive, and the `assert!` on
        /// `driver.signalled` is where that shows up.
        #[test]
        fn a_failed_frame_never_leaves_the_fence_unwaitable(
            outcomes in prop::collection::vec(outcome(), 0..64)
        ) {
            let mut frame_fence = FrameFence::new(fake_fence(1));
            let mut driver = DriverView::new();

            for &outcome in &outcomes {
                run_frame(&mut frame_fence, &mut driver, outcome);
            }
            // The frame after the last one. Whatever the run left behind, the
            // wait it is owed — if any — is a wait the fence can satisfy.
            if frame_fence.take_completion() {
                prop_assert!(
                    driver.signalled,
                    "the run {outcomes:?} ended owing a completion against a fence nothing \
                     will signal, so the next frame waits forever"
                );
            }        }

        /// The bookkeeping is exactly the driver's view of the same fence, step
        /// for step, so the equality in `collect` is not vacuous: after any run
        /// of frames the two still agree. A drift in either direction is a bug
        /// on its own — too eager to wait is the hang, too reluctant is a
        /// submission whose command buffer may still be pending.
        #[test]
        fn the_record_matches_what_the_driver_did(
            outcomes in prop::collection::vec(outcome(), 0..64)
        ) {
            let mut frame_fence = FrameFence::new(fake_fence(1));
            let mut driver = DriverView::new();

            for &outcome in &outcomes {
                run_frame(&mut frame_fence, &mut driver, outcome);
            }
            let owed = frame_fence.take_completion();
            let expected = driver.held;
            prop_assert_eq!(
                owed, expected,
                "the bookkeeping and the driver disagree about whether a completion is outstanding"
            );
        }
    }

    #[test]
    fn a_fresh_fence_owes_nothing() {
        let mut frame_fence = FrameFence::new(fake_fence(7));
        assert_eq!(frame_fence.handle(), fake_fence(7));
        assert!(
            !frame_fence.take_completion(),
            "the fence is created signalled, so the first frame has nothing to wait for"
        );
    }

    #[test]
    fn a_submitted_frame_owes_exactly_one_completion() {
        let mut frame_fence = FrameFence::new(fake_fence(1));
        let mut driver = DriverView::new();

        run_frame(&mut frame_fence, &mut driver, Outcome::Submitted);
        assert!(collect(&mut frame_fence, &mut driver));
        assert!(
            !frame_fence.take_completion(),
            "the completion is collected once; a second collection would wait for a signal that \
             has already been consumed"
        );
    }

    /// The regression this module was written for: `vkResetFences` ran before
    /// the acquire, so a frame whose acquire returned `VK_ERROR_OUT_OF_DATE_KHR`
    /// — which is what a resize produces — returned with the fence reset and
    /// nothing submitted, and the next frame blocked in `vkWaitForFences` with
    /// a `u64::MAX` timeout against a fence no submit would ever signal.
    #[test]
    fn a_frame_that_fails_after_the_previous_submit_leaves_a_waitable_fence() {
        let mut frame_fence = FrameFence::new(fake_fence(1));
        let mut driver = DriverView::new();

        // A frame that presents.
        run_frame(&mut frame_fence, &mut driver, Outcome::Submitted);
        // The next frame's acquire reports a resize. It waits for the previous
        // submit — which signalled the fence, so the wait returns — and then
        // gives up without touching the fence.
        run_frame(&mut frame_fence, &mut driver, Outcome::FailedBeforeReset);
        assert!(
            !collect(&mut frame_fence, &mut driver),
            "a frame that gave up before its submit leaves the next one nothing to wait for"
        );
        // A frame that presents again, so there is something to wait for.
        run_frame(&mut frame_fence, &mut driver, Outcome::Submitted);
        assert!(collect(&mut frame_fence, &mut driver));

        // The failure mode itself: reset the fence, then give up. The fence is
        // now unsignalled with nothing outstanding, and the next frame has to
        // report that it owes nothing rather than waiting for a signal that can
        // never arrive.
        run_frame(&mut frame_fence, &mut driver, Outcome::FailedAfterReset);
        assert!(
            !driver.signalled,
            "the failed frame left the fence reset, with no submission to signal it"
        );
        assert!(
            !collect(&mut frame_fence, &mut driver),
            "a frame that reset the fence and then failed owes nothing, and waiting anyway \
             would hang"
        );
    }
}
