// maverick/src/core/framebench.rs
//
// A measuring instrument, not a feature: a per-thread heap-allocation counter,
// so "this change removed the per-frame allocations" is a number in a test
// instead of a claim in a commit message.
//
// It exists because the whole per-frame projection the compositor runs
// (`layout::arrange` → `present::present_into`, driven by
// `compositor::live_placements`) is a pure function of `State`. It needs no X
// server, no GL context and no window manager, so it can be measured from an
// ordinary `cargo test` — see `compositor::frame_alloc_tests`.
//
// Test-only: `#[global_allocator]` and the counter are compiled out of the
// shipped binary entirely, so this costs production nothing.
//
// The counter is thread-local on purpose. `cargo test` runs test functions on
// several threads at once, and a global counter would report whatever the rest
// of the suite happened to allocate concurrently — a flaky assertion that would
// get deleted within a week. Counting per thread makes each measurement
// independent, so these tests need no `--test-threads=1`.

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

/// The system allocator, plus a per-thread tally.
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
    use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements, RibbonScratch};
    use crate::core::present::present_into;
    use crate::types::{Client, Column, Focus, Monitor, Rect, State, WindowId};

    /// A workspace with `n` single-window columns on one 1920x1080 monitor —
    /// the shape of a scrolling ribbon mid-animation.
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
                boost: 0.0,
            });
        }
        state.monitors[0].workspaces[0].focus = Focus { column_idx: 0 };
        state.monitors[0].focused = Some(1);
        // Mid-flight camera: position != target is what an animation frame
        // looks like, and it is the only state in which this path runs.
        state.monitors[0].workspaces[0].camera.position = 137.0;
        state.monitors[0].workspaces[0].camera.target = 900.0;
        state
    }

    /// Allocations performed by one animation frame's worth of projection.
    fn allocs_per_frame(n_windows: u32) -> u64 {
        let state = ribbon(n_windows);
        let cfg = Cfg::default();
        let registry = LayoutRegistry::new();
        let mut out: Placements = Placements::new();
        let mut raise: Vec<WindowId> = Vec::new();
        let mut scratch = RibbonScratch::default();

        // Warm up: let every reusable buffer reach its steady-state capacity.
        // Whatever the first few frames allocate is start-up cost, not the
        // per-frame cost we care about.
        for _ in 0..8 {
            arrange(&state, 0, &cfg, &registry, Phase::Live, &mut out, &mut scratch);
            present_into(&state, &state.monitors[0], &mut out, &mut raise);
        }

        let counter = CountAllocs::start();
        for _ in 0..16 {
            arrange(&state, 0, &cfg, &registry, Phase::Live, &mut out, &mut scratch);
            present_into(&state, &state.monitors[0], &mut out, &mut raise);
        }
        let total = counter.finish();
        // Report per frame, rounding up, so "1" really means "at least one
        // allocation every frame" and 0 means none at all.
        total.div_ceil(16)
    }

    /// The headline invariant from the compositor plan: a normal animation
    /// frame must not touch the heap.
    ///
    /// This is the whole justification for the projection buffers being
    /// caller-owned. If it ever fails, some buffer went back to being built
    /// from scratch every frame — which at 144 Hz with several monitors is a
    /// steady allocator drumbeat for values that never change shape.
    #[test]
    fn an_animation_frame_allocates_nothing() {
        for n in [1, 5, 10, 50] {
            let per_frame = allocs_per_frame(n);
            assert_eq!(
                per_frame, 0,
                "{n} windows: {per_frame} allocation(s) per animation frame; \
                 the live projection path must reuse its buffers"
            );
        }
    }
}
