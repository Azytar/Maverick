//! Compatibility shim that re-exports the pure domain model and hosts the
//! single layout-dependent helper that cannot live in `maverick-core`.
//!
//! `maverick-core` owns all pure types (`State`, `Client`, `Monitor`,
//! `Workspace`, `Column`, `Camera`, `Rect`, …). This module exists so existing
//! import paths (`crate::types::State`) keep compiling, and so the one
//! layout-dependent predicate below has a home without coupling the core to
//! layout or config.
//!
//! See [`StateExt`] for why the predicate lives here instead of in the core.


pub use maverick_core::types::*;


/// Extension trait for the layout-dependent "covering fullscreen" predicate.
///
/// Implemented for [`State`] but defined here (not in `maverick-core`) because
/// the predicate delegates to `crate::core::layout::fs_ctx`, which is
/// layout-specific: it reads `Workspace::layout`, `Workspace::overview` and the
/// column tree. Moving it into the core would drag the ribbon/column
/// implementation and layout-specific config into a crate that must stay pure
/// and testable without an X server, while call sites still get method syntax
/// through a single `use crate::types::StateExt`.
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
    /// A caller that composes the two predicates — deciding whether to publish
    /// `_NET_WM_BYPASS_COMPOSITOR` — must enforce `candidates.len() == 1` and
    /// `covers_screen` itself.
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
        // Column + Normal-policy: the fullscreen ribbon tile. `fs_ctx` owns that
        // definition (gaps and camera included) and deliberately excludes
        // `FullscreenPolicy::True` overlays, which `presented_overlay_owner`
        // reports instead — the two helpers stay disjoint, so a caller can union
        // them without double-counting a window.
        if ws.layout == LayoutKind::Column {
            let fs = crate::core::layout::fs_ctx(&self.clients, ws, mon.screen);
            if fs.win.is_some() {
                return fs.win;
            }
        }
        None
    }
}
