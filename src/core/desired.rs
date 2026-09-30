//! Desired vs. applied — the pure intent layer of the render pipeline.
//!
//! `DesiredWindow`/`DesiredState` are the explicit, per-monitor snapshot of
//! where every managed window *should* be after `layout::arrange` and
//! `present::present_into`. Pure data: `WindowId` + `Rect` + border. No X11
//! handles and no `&State` references.
//!
//! The reconciler owns the other half of the split: it diffs `DesiredState`
//! against `AppliedState` (what X11 actually has) and emits only the
//! `ConfigureWindow` deltas. The backend owns `AppliedState` and applies them.
//!
//! Invariants: `from_placements` is the only path that builds the explicit
//! form, so the pipeline stays greppable.

use crate::types::{Rect, WindowId};

/// One window's pure desired geometry, produced by core layout + present.
///
/// Contains NO X11 handles, NO references to `State`, and NO `AppliedState` — it
/// is the *intent* side of the `DesiredState` vs `AppliedState` split. The
/// reconciler compares this against what X11 already has and only configures
/// what changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesiredWindow {
    pub window: WindowId,
    pub rect: Rect,
    pub border: u32,
}

/// The explicit, pure desired state for one monitor's arrange cycle.
///
/// This is the single desired representation in the pipeline:
/// `State` → `layout::arrange` → `Placements` (internal scratch) →
/// `present::present_into` → `DesiredState` (explicit) → Reconciler →
/// `AppliedState` → X11. The split keeps intent (`DesiredState`, owned here)
/// separate from reality (`AppliedState`, owned by the backend), so the
/// reconciler is the only place that diffs and emits `ConfigureWindow`.
#[derive(Debug, Default)]
pub struct DesiredState {
    pub windows: Vec<DesiredWindow>,
}

impl DesiredState {
    /// Explicit conversion from the internal `Placements` tuple-vec.
    /// This is the ONLY place the tuple-vec becomes the explicit Desired representation.
    pub fn from_placements(placements: &[(WindowId, Rect, u32)]) -> DesiredState {
        let windows = placements
            .iter()
            .map(|&(w, r, b)| DesiredWindow {
                window: w,
                rect: r,
                border: b,
            })
            .collect();
        DesiredState { windows }
    }
}
