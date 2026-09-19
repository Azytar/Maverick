//! Synthetic `arrange` benchmark — release measurement only.
//!
//! Run: `cargo run --release --offline -- --bench-arrange` (no X needed).
//! Builds one-monitor ribbon states (`N` single-window columns, 1920x1080,
//! mid-flight camera) and times `arrange(Phase::Live) + present_into` per
//! call after warmup. Prints TSV to stdout for before/after comparison.
//!
//! Does not run the WM, touches no X connection, changes no runtime behavior.

use std::hint::black_box;
use std::time::Instant;

use crate::config::Cfg;
use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements, RibbonScratch};
use crate::core::present::present_into;
use crate::types::{Client, Column, Focus, Monitor, Rect, State, WindowId};

/// Window counts measured. Small enough to stay in L1/L2, large enough to
/// show linear scaling of the per-window projection.
const SIZES: &[u32] = &[1, 4, 8, 16, 32, 64];
/// Warmup iterations so reusable buffers reach steady-state capacity.
const WARMUP: usize = 200;
/// Timed iterations per size.
const ITERS: usize = 2000;

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
    // Mid-flight camera: the Live path actually exercised during animation.
    state.monitors[0].workspaces[0].camera.position = 137.0;
    state.monitors[0].workspaces[0].camera.target = 900.0;
    state
}

/// Time one `arrange + present_into` call steady-state, in ns/op (Measured).
fn measure_nanos_per_op(n: u32) -> (f64, usize) {
    let state = ribbon(n);
    let cfg = Cfg::default();
    let registry = LayoutRegistry::new();
    let mut out: Placements = Placements::with_capacity(128);
    let mut raise: Vec<WindowId> = Vec::with_capacity(128);
    let mut scratch = RibbonScratch::default();

    for _ in 0..WARMUP {
        arrange(
            &state,
            0,
            &cfg,
            &registry,
            Phase::Live,
            &mut out,
            &mut scratch,
        );
        present_into(&state, &state.monitors[0], &mut out, &mut raise);
        black_box(&out);
    }
    let placed = out.len();
    let start = Instant::now();
    for _ in 0..ITERS {
        arrange(
            &state,
            0,
            &cfg,
            &registry,
            Phase::Live,
            &mut out,
            &mut scratch,
        );
        present_into(&state, &state.monitors[0], &mut out, &mut raise);
        black_box(&out);
    }
    let total = start.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let per_op = total.as_nanos() as f64 / ITERS as f64;
    (per_op, placed)
}

/// Entry point for `--bench-arrange`. Prints TSV; returns process exit code.
pub fn run() -> i32 {
    println!("n_windows\tns_per_op\tplacements");
    for &n in SIZES {
        let (ns, placed) = measure_nanos_per_op(n);
        println!("{n}\t{ns:.1}\t{placed}");
    }
    println!("# Measured: ns_per_op wall time (Instant, release, steady-state).");
    println!("# Derived: ops/sec = 1e9 / ns_per_op.");
    println!("# Hypothetical: share of a 16.6ms 60fps budget — not a frame claim.");
    0
}
