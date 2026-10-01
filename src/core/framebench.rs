//! Per-thread heap-allocation counter — a measuring instrument, not a feature.
//!
//! `#[cfg(test)]` only: the shipped binary carries no counting cost. The
//! measured subject is the per-arrange projection (`layout::arrange` →
//! `present::present_into`), which is pure over `State` and so needs no X
//! connection; applying the result to X11 stays in the backend.
//!
//! Invariants: counting is thread-local, so `cargo test` parallelism cannot
//! make a measurement flaky; `realloc` counts. Warm-up allocations are excluded
//! by calling `CountAllocs::start` once the buffers have reached steady-state
//! capacity.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Allocations made by *this* thread while armed.
    ///
    /// `const`-initialised so that reading it can never itself allocate: a
    /// lazily-initialised thread-local touched from inside a global allocator
    /// would recurse into the allocator on first use.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    /// Counting is off unless a [`CountAllocs`] guard is live, so the rest of
    /// the suite pays only one thread-local read per allocation.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// The system allocator plus a per-thread allocation tally.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A reallocation counts. It is exactly what a `Vec` that outgrew its
        // reused capacity does, and that is precisely the regression these
        // measurements exist to catch — a buffer that is "reused" but re-grown
        // every frame is not reused at all.
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[inline]
fn note() {
    // `try_with`: during thread teardown the thread-local is already destroyed,
    // and panicking inside the allocator would abort the process.
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

/// Arms the per-thread allocation counter for as long as it is alive.
///
/// Reading is explicit rather than `Drop`-based ([`CountAllocs::finish`]) so
/// the measurement point is visible in the test.
pub struct CountAllocs {
    start: u64,
}

impl CountAllocs {
    /// Begin counting.
    ///
    /// Anything allocated *before* this — a reusable buffer growing to its
    /// steady-state capacity, say — is not counted. That is the point: the
    /// interesting number is the steady state, not the warm-up.
    pub fn start() -> Self {
        let start = ALLOCS.with(Cell::get);
        ARMED.with(|a| a.set(true));
        Self { start }
    }

    /// Stop counting and return how many allocations happened.
    pub fn finish(self) -> u64 {
        ARMED.with(|a| a.set(false));
        ALLOCS.with(Cell::get) - self.start
    }
}

#[cfg(test)]
mod self_tests {
    use super::CountAllocs;

    /// The counter has to actually count, or every "allocates nothing" test in
    /// the tree passes for the wrong reason forever.
    #[test]
    fn the_counter_is_not_vacuously_zero() {
        let counter = CountAllocs::start();
        let v: Vec<u8> = Vec::with_capacity(4096);
        let n = counter.finish();
        assert!(v.capacity() >= 4096);
        assert!(n >= 1, "a fresh 4 KiB Vec must register as an allocation");
    }

    /// ...and it must not count anything outside a live guard.
    #[test]
    fn counting_is_off_by_default() {
        let _untracked: Vec<u8> = Vec::with_capacity(4096);
        let counter = CountAllocs::start();
        let n = counter.finish();
        assert_eq!(n, 0, "nothing was allocated between start() and finish()");
    }
}

mod frame_alloc_tests {
    use super::CountAllocs;
    use crate::config::Cfg;
    use crate::core::layout::{arrange, Placements, RibbonScratch};
    use crate::core::present::present_into;
    use crate::types::{Client, Column, Focus, Monitor, Rect, State, WindowId};

    /// A workspace with `n` single-window columns on one 1920x1080 monitor —
    /// the shape of a scrolling ribbon.
    fn ribbon(n: u32) -> State {
        let screen = Rect::new(0, 0, 1920, 1080);
        let mut state = State::new();
        state.monitors.push(Monitor::new(screen, 1));
        for i in 0..n {
            let win = (i + 1) as WindowId;
            let mut c = Client::new(win, 0, 0);
            c.geom = Rect::new(0, 0, 400, 900);
            state.add_client(c);
            state.monitors[0].workspaces[0].columns.push(Column {
                windows: vec![win],
                focused: 0,
                weight: 0.25,
            });
        }
        state.monitors[0].workspaces[0].focus = Focus { column_idx: 0 };
        state.monitors[0].focused = Some(1);
        // A scrolled camera: a non-zero offset is what puts the projection on
        // its non-trivial path, and it is the state a real session is in
        // whenever more than one column exists.
        state.monitors[0].workspaces[0].camera.position = 900.0;
        state
    }

    /// Allocations performed by one arrange.
    fn allocs_per_frame(n_windows: u32) -> u64 {
        let state = ribbon(n_windows);
        let cfg = Cfg::default();
        let mut out: Placements = Placements::new();
        let mut scratch = RibbonScratch::default();

        // Warm up: let every reusable buffer reach its steady-state capacity.
        // Whatever the first few frames allocate is start-up cost, not the
        // per-frame cost we care about.
        for _ in 0..8 {
            arrange(&state, 0, &cfg, &mut out, &mut scratch);
            present_into(&state, &state.monitors[0], &mut out);
        }

        let counter = CountAllocs::start();
        for _ in 0..16 {
            arrange(&state, 0, &cfg, &mut out, &mut scratch);
            present_into(&state, &state.monitors[0], &mut out);
        }
        let total = counter.finish();
        // Report per frame, rounding up, so "1" really means "at least one
        // allocation every frame" and 0 means none at all.
        total.div_ceil(16)
    }

    /// The headline projection invariant: an arrange must not touch the heap.
    ///
    /// This is the whole justification for the projection buffers being
    /// caller-owned. If it ever fails, some buffer went back to being built
    /// from scratch on every turn — which, on a busy session that arranges on
    /// every focus change, is a steady allocator drumbeat for values whose
    /// shape never changes.
    #[test]
    fn an_arrange_allocates_nothing() {
        for n in [1, 5, 10, 50] {
            let per_frame = allocs_per_frame(n);
            assert_eq!(
                per_frame, 0,
                "{n} windows: {per_frame} allocation(s) per arrange; \
                 the live projection path must reuse its buffers"
            );
        }
    }
}
