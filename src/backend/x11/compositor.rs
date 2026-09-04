// maverick/src/backend/x11/compositor.rs
//
// Thin feature gate for the compositor. With `compositor-opengl` enabled, the
// full OpenGL/GLX implementation (`compositor_gl`) is compiled in and its public
// API is re-exported here. Without the feature, the WM core still compiles: it
// sees a zero-overhead placeholder so `Option<Compositor>` in `WindowManager`
// has a consistent type, and every `.compositor.as_mut()` call site simply
// observes `None` / a do-nothing implementation. The compositor never runs,
// never touches X extensions, and never spawns a render/animator loop — pure
// X11 WM with minimal CPU/RAM.

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
        #[inline(always)]
        pub fn set_wallpaper(&mut self, _wp: &crate::core::wallpaper::WallpaperSpec) {}
        #[inline(always)]
        pub fn set_transforms(&mut self, _placements: &[(Window, Rect, u32)]) {}
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
        #[inline(always)]
        pub fn tracked_window_count(&self) -> usize {
            0
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

    /// Substeps the animation delta for stable spring integration. In the
    /// no-compositor build this yields nothing so `tick_animations_multi` is
    /// never called.
    pub fn substep_bounds(_dt: f32) -> Vec<f32> {
        Vec::new()
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
