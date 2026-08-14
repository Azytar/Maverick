use crate::types::{Rect, WindowId};

/// One window's pure desired geometry, produced by core layout + present.
/// Contains NO X11 handles, NO GL state, NO references to `State`, NO Applied state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesiredWindow {
    pub window: WindowId,
    pub rect: Rect,
    pub border: u32,
    /// WM wants this window visible at `rect`. True for all arrange-produced windows today.
    pub mapped: bool,
}

/// The explicit, pure desired state for one monitor's arrange cycle.
/// This is the single desired representation in the pipeline:
///   State -> layout::arrange -> Placements (internal scratch)
///        -> present::present_into
///        -> DesiredState (explicit) -> Reconciler -> AppliedState -> X11
#[derive(Debug, Default)]
pub struct DesiredState {
    pub windows: Vec<DesiredWindow>,
    /// Bottom->top stacking order as produced by present_into.
    pub raise: Vec<WindowId>,
}

impl DesiredState {
    /// Explicit conversion from the internal `Placements` tuple-vec + raise list.
    /// This is the ONLY place the tuple-vec becomes the explicit Desired representation.
    pub fn from_placements(
        placements: &[(WindowId, Rect, u32)],
        raise: &[WindowId],
    ) -> DesiredState {
        let windows = placements
            .iter()
            .map(|&(w, r, b)| DesiredWindow {
                window: w,
                rect: r,
                border: b,
                mapped: true,
            })
            .collect();
        DesiredState {
            windows,
            raise: raise.to_vec(),
        }
    }
}
