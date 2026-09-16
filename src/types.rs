// maverick/src/types.rs
// Core state — niri-style columnar layout, clean coordinates, no drift.

#![allow(unused_imports)]

pub use maverick_core::types::*;
pub use maverick_core::types::{sanitize_spring, spring_smooth};

/// Extension trait for the layout-dependent "covering fullscreen" predicate.
///
/// Implemented for [`State`] but defined here (not in `maverick-core`) because
/// the predicate depends on `crate::core::layout::fs_ctx`, which is layout-
/// specific (needs `LayoutKind::Column` / `overview` / column fullscreen
/// participation). Keeping it here preserves `maverick-core`'s purity — no
/// layout or config coupling — while retaining ergonomic method syntax.
///
/// All call sites use `state.covering_fullscreen_window(idx)` and only need
/// `use crate::types::StateExt`.
pub trait StateExt {
    /// The single fullscreen window that *covers* `mon_idx`'s screen as a
    /// scrolling-ribbon tile, if any.
    ///
    /// # Semantics
    ///
    /// Delegates to `crate::core::layout::fs_ctx(&self.clients, ws, mon.screen)`
    /// and returns `fs_ctx.win` — the focused column's fullscreen participant
    /// when the active workspace is `LayoutKind::Column` and not in `overview`.
    /// Returns `None` when the workspace is not columnar, is in overview, or no
    /// column contains a `FULLSCREEN && !is_true_fullscreen()` window.
    ///
    /// # Relation to `presented_overlay_owner`
    ///
    /// This is **not** the same as [`State::presented_overlay_owner`](maverick_core::types::State::presented_overlay_owner).
    /// A column-layout normal-policy fullscreen is *covering* (it tiles across
    /// the workarea as a wide column) but is **not** a presented overlay; an
    /// exclusive overlay (`FullscreenPolicy::True`) and the `presented_maximize`
    /// owner **are** overlays but may not be *covering* in this ribbon sense.
    /// Composition policy (`compositor_policy::bypass_candidate`) unions both
    /// predicates and enforces `candidates.len() == 1` and `covers_screen`.
    ///
    /// # Purity
    ///
    /// Pure: reads `self.clients`, the active workspace's layout/overview, and
    /// column/window placement. No X11/GL.
    fn covering_fullscreen_window(&self, mon_idx: usize) -> Option<WindowId>;
}

impl StateExt for State {
    fn covering_fullscreen_window(&self, mon_idx: usize) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.workspaces.get(mon.active_ws)?;
        if ws.overview {
            return None;
        }
        // Column + Normal-policy: the fullscreen ribbon tile (fs_ctx owns this
        // definition, gaps/camera included). `FullscreenPolicy::True` overlays
        // are deliberately EXCLUDED here by `fs_ctx` — they are reported by
        // `presented_overlay_owner` instead, keeping the two helpers disjoint
        // as `compositor_policy::bypass_candidate` requires (union == 1).
        if ws.layout == LayoutKind::Column {
            let fs = crate::core::layout::fs_ctx(&self.clients, ws, mon.screen);
            if fs.win.is_some() {
                return fs.win;
            }
        }
        None
    }
}
