//! Compositor feature gate — selects the real GL compositor or a zero-cost stub.
//!
//! With `compositor-opengl` enabled, the full OpenGL/GLX implementation
//! (`compositor_gl`) is compiled in and its public API is re-exported here.
//! Without the feature, the WM core still compiles: it sees a placeholder so
//! `Option<Compositor>` in `WindowManager` has a consistent type and every
//! `.compositor.as_mut()` call site observes `None` / no-ops. The compositor
//! never runs, never touches X extensions, and never spawns a render loop —
//! pure X11 path with minimal CPU/RAM.
//!
//! # Ownership & lifecycle
//!
//! `WindowManager` holds `Option<Compositor>`; `Compositor::init` is the only
//! constructor and returns `None` on any GL/compositor failure. The stub's
//! `Compositor::init` always returns `None` so the type still exists.
//!
//! # Bypass
//!
//! `DirtyReason` bitflags and `FrameScheduler` stubs exist so the event loop
//! compiles regardless of whether the compositor is enabled. The real versions
//! live in `compositor_gl.rs`.
//!
//! # Safety
//!
//! No `unsafe` in this gate; the GL module's `unsafe` is guarded by
//! `maverick_x11::open_x` (`Display*` live while `Rc<XConn>` holds the
//! `xcb_connection_t` with `should_drop=false`).

#[cfg(feature = "compositor-opengl")]
#[path = "compositor_gl.rs"]
mod compositor_gl;

#[cfg(feature = "compositor-opengl")]
pub(crate) use compositor_gl::*;

// --- Placeholder API when no compositor backend is compiled in ---------------

#[cfg(not(feature = "compositor-opengl"))]
#[allow(dead_code, clippy::inline_always)]
mod placeholder {
    use std::rc::Rc;

    use crate::types::*;
    use maverick_x11::{XConn, XDisplay};
    use x11rb::protocol::xproto::Window;

    /// Zero-field placeholder. All methods are no-ops so the WM core compiles
    /// and runs identically; the compositor-related fields are never driven.
    #[derive(Default)]
    pub struct Compositor {
        pub float_trace: bool,
        pub comp_trace: bool,
    }

    impl Compositor {
        pub fn init(
            _conn: Rc<XConn>,
            _dpy: XDisplay,
            _root: Window,
            _screen_num: usize,
            _check_win: Window,
            _cfg: &crate::config::Cfg,
        ) -> Option<Self> {
            None
        }
        #[inline(always)]
        pub fn needs_frame(&self) -> bool {
            false
        }
        #[inline(always)]
        pub fn dirty_reasons_bits(&self) -> u8 {
            0
        }
        #[inline(always)]
        pub fn dirty_reasons(&self) -> DirtyReason {
            DirtyReason::NONE
        }
        #[inline(always)]
        pub fn invalidate(&mut self) {}
        #[inline(always)]
        pub fn on_destroy(&mut self, _window: Window) {}
        #[inline(always)]
        pub fn on_unmap(&mut self, _window: Window) {}
        #[inline(always)]
        pub fn on_configure(
            &mut self,
            _window: Window,
            _x: i32,
            _y: i32,
            _w: u32,
            _h: u32,
            _border: u32,
        ) {
        }
        #[inline(always)]
        pub fn on_restack(&mut self, _window: Window, _above: Option<Window>) {}
        #[inline(always)]
        pub fn on_opacity(&mut self, _window: Window, _opacity: f32) {}
        #[inline(always)]
        pub fn on_create(&mut self, _window: Window) {}
        #[inline(always)]
        pub fn on_map(&mut self, _window: Window) {}
        #[inline(always)]
        pub fn on_damage(&mut self, _window: Window) {}
        /// No-op without the compositor feature: the native border already
        /// carries the color, there is no GL stroke to keep in sync.
        #[inline(always)]
        pub fn on_border_color(&mut self, _window: Window, _pixel: u32) {}
        #[inline(always)]
        pub fn set_outputs(&mut self, _outputs: &[Rect]) {}
        #[inline(always)]
        pub fn set_hidden(&mut self, _window: Window, _hidden: bool) {}
        #[inline(always)]
        pub fn engage_bypass(&mut self, _mon: usize, _win: Window) {}
        #[inline(always)]
        pub fn disengage_bypass(&mut self, _mon: usize) {}
        #[inline(always)]
        pub fn disengage_all_bypass(&mut self) {}
        #[inline(always)]
        pub fn tick_wallpaper(&mut self, _dt: f32) {}
        #[inline(always)]
        pub fn wallpaper_animating(&self) -> bool {
            false
        }
        /// No presentation transitions exist without the compositor feature,
        /// so the loop never stays awake for one.
        #[inline(always)]
        pub fn presentation_animating(&self) -> bool {
            false
        }
        #[inline(always)]
        pub fn set_wallpaper(&mut self, _wp: &crate::core::wallpaper::WallpaperSpec) {}
        #[inline(always)]
        pub fn set_transforms(&mut self, _placements: &[(Window, Rect, u32)]) {}
        #[inline(always)]
        pub fn prepare_frame(
            &mut self,
            _state: &mut crate::types::State,
            _cfg: &crate::config::Cfg,
            _registry: &crate::core::layout::LayoutRegistry,
            _anim_per_mon: &[bool],
        ) {
        }
        #[inline(always)]
        pub fn set_debug_floats(&mut self, _ids: &[WindowId]) {}
        #[inline(always)]
        pub fn render(&mut self) -> bool {
            false
        }
        #[inline(always)]
        pub fn disable(&mut self) {}
        #[inline(always)]
        pub fn debug_dump(&self) -> String {
            String::new()
        }
    }

    /// Bitfield-style dirty-reason placeholder. Mirrors the real enum's
    /// `contains`/associated constants so call sites compile unchanged.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Copy)]
    pub struct DirtyReason(u8);

    impl DirtyReason {
        pub const NONE: Self = Self(0);
        pub const DAMAGE: Self = Self(1 << 0);
        pub const GEOMETRY: Self = Self(1 << 1);
        pub const SURFACE: Self = Self(1 << 2);
        pub const FOCUS: Self = Self(1 << 3);
        pub const WALLPAPER: Self = Self(1 << 4);
        #[inline(always)]
        pub fn contains(self, other: Self) -> bool {
            self.0 & other.0 != 0
        }
        #[inline(always)]
        pub fn insert(&mut self, other: Self) {
            self.0 |= other.0;
        }
        #[inline(always)]
        pub fn clear(&mut self) {
            self.0 = 0;
        }
    }

    impl std::ops::BitOr for DirtyReason {
        type Output = Self;
        #[inline(always)]
        fn bitor(self, rhs: Self) -> Self {
            Self(self.0 | rhs.0)
        }
    }
    impl std::ops::BitOrAssign for DirtyReason {
        #[inline(always)]
        fn bitor_assign(&mut self, rhs: Self) {
            self.0 |= rhs.0;
        }
    }

    /// Substeps the animation delta for stable spring integration. Must match
    /// the compositor-enabled implementation so animations still tick when the
    /// compositor is disabled (placeholder bug fix: returning empty killed the
    /// spring).
    pub fn substep_bounds(dt: f32) -> Vec<f32> {
        if !dt.is_finite() || dt <= 0.0 {
            return Vec::new();
        }
        const SUBSTEP_MS: f32 = 8.0;
        let max = SUBSTEP_MS / 1000.0;
        let n = (dt / max).ceil().max(1.0) as usize;
        let step = dt / n as f32;
        vec![step; n]
    }

    pub fn live_placements(
        _state: &crate::types::State,
        _mon_idx: usize,
        _cfg: &crate::config::Cfg,
        _registry: &crate::core::layout::LayoutRegistry,
        _out: &mut crate::core::layout::Placements,
        _raise: &mut Vec<crate::types::WindowId>,
        _scratch: &mut crate::core::layout::RibbonScratch,
    ) {
    }

    pub struct FrameScheduler;
    impl FrameScheduler {
        pub fn from_compositor(_anim: bool, _wp_anim: bool, _reasons: DirtyReason) -> Self {
            FrameScheduler
        }
    }
}

#[cfg(not(feature = "compositor-opengl"))]
pub use placeholder::*;

#[cfg(all(test, not(feature = "compositor-opengl")))]
mod placeholder_substep_tests {
    use super::placeholder::substep_bounds;

    #[test]
    fn zero_and_negative_yields_empty() {
        assert!(substep_bounds(0.0).is_empty());
        assert!(substep_bounds(-0.01).is_empty());
        assert!(substep_bounds(f32::NAN).is_empty());
        assert!(substep_bounds(f32::INFINITY).is_empty());
    }

    #[test]
    fn small_dt_single_step() {
        let v = substep_bounds(0.004);
        assert_eq!(v.len(), 1);
        assert!((v[0] - 0.004).abs() < 1e-6);
        let v = substep_bounds(0.008);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn multi_step_invariants() {
        let cases = [(0.016, 2), (0.017, 3), (0.024, 3), (0.032, 4)];
        for (dt, expect_n) in cases {
            let v = substep_bounds(dt);
            assert_eq!(v.len(), expect_n, "dt={dt}");
            let sum: f32 = v.iter().sum();
            assert!((sum - dt).abs() < 1e-6, "sum {sum} != dt {dt}");
            for &s in &v {
                assert!(s <= 0.0080001, "step {s} > 8ms");
                assert!(s > 0.0);
            }
        }
    }

    #[test]
    fn tick_consumes_substeps() {
        // Placeholder must produce steps that actually drive the spring.
        use crate::types::{Monitor, Rect};
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workspaces[0].camera.position = 0.0;
        mon.workspaces[0].camera.target = 100.0;
        let dt = 0.016;
        let pos_before = mon.workspaces[0].camera.position;
        for sub in substep_bounds(dt) {
            mon.workspaces[0].camera.step(sub);
        }
        assert!((mon.workspaces[0].camera.position - pos_before).abs() > 1e-6);
        // Empty dt must not move
        let mut mon2 = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon2.workspaces[0].camera.position = 0.0;
        mon2.workspaces[0].camera.target = 100.0;
        for sub in substep_bounds(0.0) {
            mon2.workspaces[0].camera.step(sub);
        }
        assert!((mon2.workspaces[0].camera.position - 0.0).abs() < 1e-6);
    }
}
