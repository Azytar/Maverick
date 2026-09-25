//! Properties of `ControlHub`, the only channel between the control-socket
//! threads and the WM's single event-loop thread.
//!
//! The hub is where Maverick's back-pressure lives. Everything here has to hold
//! under an arbitrary arrival order, because the producers are connection
//! threads that the WM never throttles: a command queue that can overfill
//! grows the WM's memory, and a subscriber list that overshoots its cap parks
//! every remaining handler thread on a stream nobody feeds.

use maverick_sys::hub::{ControlCommand, ControlHub, CMD_CAP, SUB_CAP};
use proptest::prelude::*;
use std::sync::{Arc, Barrier};

/// Cap values worth exercising: the real one, none at all, and one past it, so
/// the boundary `subs.len() >= max` is crossed in both directions. Caps well
/// above the real one are included because the cap is a parameter, and a large
/// cap means a large batch of registrations race for the last free slots.
fn subscriber_cap() -> impl Strategy<Value = usize> {
    prop_oneof![
        3 => any::<usize>().prop_map(|n| n % (maverick_sys::control::MAX_SUBSCRIBERS + 3)),
        1 => Just(maverick_sys::control::MAX_SUBSCRIBERS),
        1 => Just(0),
        2 => 8usize..=64,
    ]
}

/// Command kinds, with the dispatch payload kept as a string so FIFO order can
/// be checked by content (`ControlCommand` has no `PartialEq`: a `Query` carries
/// its reply channel).
fn command() -> impl Strategy<Value = ControlCommand> {
    prop_oneof![
        2 => Just(ControlCommand::Quit),
        2 => Just(ControlCommand::Restart),
        2 => Just(ControlCommand::Reload),
        3 => "[a-z0-9 -]{0,12}".prop_map(ControlCommand::Dispatch),
    ]
}

/// A line the WM can hand to `emit`: event JSON, but the hub promises verbatim
/// delivery of whatever string it is given.
fn event_line() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            3 => any::<char>(),
            1 => Just('\n'),
            1 => Just('\u{0}'),
            1 => Just('"'),
        ],
        0..16,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

// Registering can never push the live sink count past the cap, and every
// attempt up to the cap is admitted: a subscriber is only refused once the
// budget is actually spent, never early.
proptest! {
    #[test]
    fn subscriber_cap_is_never_exceeded(max in subscriber_cap(), attempts in 0usize..40) {
        let hub = ControlHub::new();
        let mut admitted = 0usize;
        for _ in 0..attempts {
            if hub.try_subscribe(max).is_some() {
                admitted += 1;
            }
            prop_assert!(
                hub.subscriber_count() <= max,
                "{} sinks registered under a cap of {}",
                hub.subscriber_count(),
                max
            );
        }
        prop_assert_eq!(admitted, attempts.min(max), "cap must admit exactly min(attempts, max)");
        prop_assert_eq!(hub.subscriber_count(), admitted);
    }
}

// The cap is a capacity decision, not a counter read: concurrent connections all
// pass a naive `count < max` check and then register, overshooting the budget
// and starving short commands of handler slots. Whatever order the attempts
// interleave in, exactly `min(attempts, max)` may be admitted and the
// registered list may never exceed the cap.
//
// Every thread hammers the registration path many times over: the window
// between reading the count and pushing the sink is only a few instructions
// wide, so one registration per thread would catch the bug only on a machine
// that happens to deschedule in exactly that window.
proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]
    #[test]
    fn subscriber_cap_holds_under_concurrent_registration(
        max in subscriber_cap(),
        threads in 8usize..=64,
        rounds in 32usize..=256,
    ) {
        let hub = ControlHub::new();
        let gate = Arc::new(Barrier::new(threads));
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let hub = hub.clone();
            let gate = gate.clone();
            handles.push(std::thread::spawn(move || {
                let mut admitted = 0usize;
                // Connections arrive together, then keep arriving.
                gate.wait();
                for _ in 0..rounds {
                    if hub.try_subscribe(max).is_some() {
                        admitted += 1;
                    }
                }
                admitted
            }));
        }
        let admitted: usize = handles
            .into_iter()
            .map(|h| h.join().expect("registration thread"))
            .sum();
        prop_assert_eq!(admitted, (threads * rounds).min(max), "cap of {}", max);
        prop_assert!(hub.subscriber_count() <= max, "cap of {} overshot", max);
        prop_assert_eq!(hub.subscriber_count(), admitted);
    }
}

// The command queue is the WM's only unbounded-input valve: it must refuse at
// exactly the capacity, and what it does accept has to arrive in the order it
// was pushed, because the WM executes them as a script.
proptest! {
    #[test]
    fn command_queue_is_bounded_and_preserves_order(cmds in proptest::collection::vec(command(), 0..(CMD_CAP + 5))) {
        let hub = ControlHub::new();
        let mut accepted = 0usize;
        for c in &cmds {
            if hub.push_command(c.clone()) {
                accepted += 1;
            }
        }
        prop_assert_eq!(accepted, cmds.len().min(CMD_CAP), "queue must accept exactly CMD_CAP");
        let drained = hub.drain_commands();
        prop_assert_eq!(drained.len(), accepted);
        for (got, want) in drained.iter().zip(&cmds[..accepted]) {
            let same = match (got, want) {
                (ControlCommand::Quit, ControlCommand::Quit)
                | (ControlCommand::Restart, ControlCommand::Restart)
                | (ControlCommand::Reload, ControlCommand::Reload) => true,
                (ControlCommand::Dispatch(a), ControlCommand::Dispatch(b)) => a == b,
                _ => false,
            };
            prop_assert!(same, "command queue reordered: {got:?} arrived where {want:?} was sent");
        }
        // Draining is a full take: the WM must not see a command twice.
        prop_assert!(hub.drain_commands().is_empty());
    }
}

// A subscriber that never reads may not stall the WM thread or grow without
// bound: the queue is capped, the overflow is counted rather than queued, and
// the lines that did fit keep their order.
proptest! {
    #[test]
    fn a_slow_subscriber_drops_overflow_without_blocking(emitted in 0usize..(SUB_CAP + 20)) {
        let hub = ControlHub::new();
        let rx = hub.subscribe();
        for i in 0..emitted {
            hub.emit(format!("{{\"n\":{i}}}"));
        }
        prop_assert_eq!(
            hub.dropped_events(),
            emitted.saturating_sub(SUB_CAP),
            "only the overflow past the per-subscriber cap may be dropped"
        );
        prop_assert_eq!(hub.subscriber_count(), 1, "a slow subscriber is kept, not pruned");
        for i in 0..emitted.min(SUB_CAP) {
            prop_assert_eq!(rx.try_recv().expect("queued line"), format!("{{\"n\":{i}}}"));
        }
    }
}

// Emitted lines reach subscribers exactly as written — a `subscribe` client
// parses them as JSON, so any rewriting on the way through would corrupt the
// payload. A subscriber that has gone away is dropped on the next `emit`, and
// one that is still there keeps its slot.
proptest! {
    #[test]
    fn events_reach_live_subscribers_verbatim_and_dead_ones_are_pruned(
        lines in proptest::collection::vec(event_line(), 1..8),
    ) {
        let hub = ControlHub::new();
        let rx0 = hub.subscribe();
        let rx1 = hub.subscribe();
        let rx2 = hub.subscribe();
        prop_assert_eq!(hub.subscriber_count(), 3);
        drop(rx0);
        drop(rx2);
        for line in &lines {
            hub.emit(line.clone());
        }
        prop_assert_eq!(hub.subscriber_count(), 1, "both disconnected sinks must be pruned");
        for line in &lines {
            let got = rx1.try_recv().expect("live subscriber keeps its events");
            prop_assert_eq!(&got, line);
        }
    }
}

// The snapshot is the WM's cached view of its own state, read by the server
// thread: every clone must see the last published value, verbatim, and the
// fresh hub must start from the documented empty object.
proptest! {
    #[test]
    fn published_state_is_shared_by_every_clone(payload in "[ -~\\n]{0,64}") {
        let hub = ControlHub::new();
        prop_assert_eq!(hub.snapshot(), "{}", "a fresh hub serves an empty object");
        let clone = hub.clone();
        hub.publish_state(payload.clone());
        prop_assert_eq!(&hub.snapshot(), &payload);
        prop_assert_eq!(&clone.snapshot(), &payload, "clones must share the state cache");
        // A later publish replaces the value outright rather than appending.
        hub.publish_state("{}");
        prop_assert_eq!(clone.snapshot(), "{}");
    }
}
