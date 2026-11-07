//! Desired vs. applied — the pure intent layer of the render pipeline.
//!
//! `DesiredWindow`/`DesiredState` describe where every managed window
//! *should* be after `layout::arrange` and `present::present_into`. The
//! representation is a borrowed view over the reusable `Placements` buffer:
//! no X11 handles, no `&State`, and no second per-frame heap copy.
//!
//! The reconciler owns the other half of the split: it diffs `DesiredState`
//! against `AppliedState` (what X11 actually has) and emits only the
//! `ConfigureWindow` deltas. The backend owns `AppliedState` and applies them.
//!
//! Invariants: `from_placements` is the only constructor, so the semantic
//! boundary stays explicit and greppable without duplicating the underlying
//! geometry.

use crate::types::{Rect, WindowId};

/// One window's pure desired geometry, projected from the placement buffer.
///
/// Contains NO X11 handles, NO references to `State`, and NO `AppliedState`.
/// It is a cheap copied view of one placement tuple, not stored in a second
/// collection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesiredWindow {
    pub window: WindowId,
    pub rect: Rect,
    pub border: u32,
}

/// Borrowed desired state for one monitor's arrange cycle.
///
/// `Placements` remains the single owned projection buffer:
/// `State -> layout::arrange -> Placements -> present::present_into ->
/// DesiredState<'_> -> Reconciler -> AppliedState -> X11`.
///
/// Keeping this semantic wrapper preserves the intent/applied boundary while
/// avoiding the old per-reconcile `Vec<DesiredWindow>` allocation.
#[derive(Debug)]
pub struct DesiredState<'a> {
    placements: &'a [(WindowId, Rect, u32)],
}

impl<'a> DesiredState<'a> {
    /// Borrow the already-materialized placement buffer without copying it.
    pub fn from_placements(placements: &'a [(WindowId, Rect, u32)]) -> Self {
        Self { placements }
    }

    pub fn len(&self) -> usize {
        self.placements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = DesiredWindow> + DoubleEndedIterator + '_ {
        self.placements
            .iter()
            .map(|&(window, rect, border)| DesiredWindow {
                window,
                rect,
                border,
            })
    }
}
