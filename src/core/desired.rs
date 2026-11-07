//! Desired vs. applied — the pure intent layer of the render pipeline.
//!
//! `DesiredWindow`/`DesiredState` describe where every managed window
//! *should* be after `layout::arrange` and `present::present_into`. Production
//! borrows the reusable `Placements` buffer directly: no X11 handles, no
//! `&State`, and no second per-frame heap copy.
//!
//! The reconciler owns the other half of the split: it diffs `DesiredState`
//! against `AppliedState` (what X11 actually has) and emits only the
//! `ConfigureWindow` deltas. The backend owns `AppliedState` and applies them.
//!
//! Invariants: `from_placements` is the production constructor, so the
//! semantic boundary stays explicit and greppable without duplicating geometry.

use std::borrow::Cow;

use crate::types::{Rect, WindowId};

type Placement = (WindowId, Rect, u32);

/// One window's pure desired geometry, projected from the placement buffer.
///
/// Contains no X11 handles, no references to `State`, and no `AppliedState`.
/// It is a cheap copied view of one placement tuple, not stored in a second
/// production collection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesiredWindow {
    pub window: WindowId,
    pub rect: Rect,
    pub border: u32,
}

/// Desired state for one monitor's arrange cycle.
///
/// Production uses the borrowed variant, keeping `Placements` as the single
/// owned projection buffer:
/// `State -> layout::arrange -> Placements -> present::present_into ->
/// DesiredState<'_> -> Reconciler -> AppliedState -> X11`.
///
/// Tests may use an owned form to construct deliberately-invalid desired
/// sequences (for example duplicate windows) without weakening the production
/// no-copy path.
#[derive(Debug)]
pub struct DesiredState<'a> {
    placements: Cow<'a, [Placement]>,
}

impl<'a> DesiredState<'a> {
    /// Borrow the already-materialized placement buffer without copying it.
    pub fn from_placements(placements: &'a [Placement]) -> Self {
        Self {
            placements: Cow::Borrowed(placements),
        }
    }

    #[cfg(test)]
    pub fn from_windows(windows: Vec<DesiredWindow>) -> DesiredState<'static> {
        DesiredState {
            placements: Cow::Owned(
                windows
                    .into_iter()
                    .map(|w| (w.window, w.rect, w.border))
                    .collect(),
            ),
        }
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
