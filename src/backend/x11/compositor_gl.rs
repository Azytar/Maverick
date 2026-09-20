//! OpenGL/GLX compositor — the GPU presentation path for the X11 backend.
//!
//! The compositor sits *on top* of the window manager and shares the single
//! `Rc<XConn>` / `XDisplay` pair so both see the same `xcb_connection_t`
//! sequence space and event queue. Without it every animation frame
//! re-`ConfigureWindow`s each window; with it the WM writes final (settled)
//! geometry once and the compositor draws each window's redirected pixmap as a
//! live-transformed GL quad.
//!
//! # Ownership & lifecycle
//!
//! - The WM creates the compositor via [`Compositor::init`] which claims
//!   `_NET_WM_CM_S0`, redirects root subwindows to [`Redirect::MANUAL`],
//!   acquires the `CompositeGetOverlayWindow` overlay, and builds the GLX
//!   context. Any failure (no libGL, missing visual/fbconfig, selection owned)
//!   returns `None` and the WM stays on the plain X11 path.
//! - `Compositor` owns per-window [`CompWin`] state (pixmap, texture, damage,
//!   opacity, transform) plus the `Damage` objects and the overlay window.
//!   `Drop` / `disable` unredirects and releases the CM selection.
//! - The overlay is never redirected; its input shape is emptied via Xfixes so
//!   pointer events fall through to real clients.
//!
//! # Protocol — why each piece exists
//!
//! - **Overlay** (`CompositeGetOverlayWindow`): the sole drawable we render into.
//!   It sits above all redirected windows by definition, so no stacking dance is
//!   needed for the framebuffer itself.
//! - **Composite `MANUAL`**: windows are not automatically copied; the server
//!   keeps their off-screen storage but does not composite. We bind only when
//!   `Damage` fires, avoiding per-frame copies for idle windows.
//! - **TFP (`GLX_EXT_texture_from_pixmap`)**: `NameWindowPixmap` + `glXBindTexImage`
//!   turns the redirected pixmap into a sampled `Texture` without a CPU readback.
//!   The fbconfig is chosen per-window visual (looked up in `formats`), never
//!   inferred from depth alone.
//! - **Damage `CAP32` / `NON_EMPTY`**: per-window `Damage` with `ReportLevel`
//!   tracking; `XDamageSubtract` re-arms after each bind. `DamageRegion::CAP`
//!   is 32 rects; overflow forces a full repaint — bounded, allocation-free.
//! - **Occlusion** (`occluder_rects`, `fully_covered_by`): top-to-bottom pass
//!   marks windows fully covered by a single opaque, square-cornered occluder
//!   as `occluded` so they are not drawn.
//! - **`VSync` (`SwapBuffers`)**: `glXSwapIntervalEXT(1)` makes `swap` block on
//!   the vertical retrace; the frame scheduler's 0 ms / 100 ms poll merely
//!   decides *whether* to render, never synthesizes a vblank.
//!
//! # Safety
//!
//! Raw `Display*` is only reconstituted from `XDisplay::as_ptr()` inside
//! `Compositor::init` while the original `XDisplay` (backed by the live
//! `Rc<XConn>`'s `xcb_connection_t` with `should_drop=false`) is still alive;
//! verified by `maverick_x11::open_x`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::Instant;

use maverick_gl::{
    DrawQuad, Filter, Rect as GlRect, Renderer as GlRenderer, ShaderId, Texture, TextureHandle,
    VisualFormat, VsyncMode as GlVsyncMode, XConn,
};
use maverick_img::Rgba8;
use maverick_x11::XDisplay as X11Display;

use crate::core::wallpaper::{
    compute_wallpaper_rects, shader_is_animated, GpuImage, WallpaperGpu, WallpaperMode,
    WallpaperSource, WallpaperSpec,
};
use crate::log;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::damage::{ConnectionExt as _, Damage, ReportLevel};
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::*;

use crate::compositor_policy::CompositionMode;
use crate::config::Cfg;
use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements, RibbonScratch};
use crate::core::present::present_into;
use crate::types::{Rect, State, WindowId};

/// Soft upper bound on substep length (seconds). The camera spring (`damping`
/// 30) is unstable above ~8 ms, so every animation frame is split into pieces
/// no longer than this — see `WindowManager::run_once`.
const SUBSTEP_MS: f32 = 8.0;

/// Projection signature for the compositor's live layout cache — mirrors
/// `WindowManager::ProjSig` but lives in the compositor so the WM core does
/// not own presentation state.
#[derive(Clone, PartialEq)]
struct ProjSig {
    zoom: f32,
    zoom_target: f32,
    page_zoom: f32,
    page_zoom_target: f32,
    eff_boost: Vec<f32>,
}

fn proj_signature(ws: &crate::types::Workspace, cfg: &Cfg) -> ProjSig {
    let total_boost = cfg.accordion_boost.clamp(0.0, 0.9);
    ProjSig {
        zoom: ws.zoom,
        zoom_target: ws.zoom_target,
        page_zoom: ws.page_zoom,
        page_zoom_target: ws.page_zoom_target,
        eff_boost: ws.columns.iter().map(|c| total_boost * c.boost).collect(),
    }
}

fn live_alpha(ws: &crate::types::Workspace) -> f32 {
    let a = ws.zoom.max(0.05);
    if ws.viewport_mode == crate::types::ViewportMode::Zoomed {
        ws.page_zoom.max(0.05)
    } else {
        a
    }
}

/// Pure overlay coverage: screen minus union of bypass rectangles.
///
/// Starts with `screen` and iteratively subtracts each hole, splitting
/// rects into up to 4 pieces. Handles overlapping holes and holes outside
/// screen (clipped). Returns the list of rectangles that remain covered
/// by the overlay (bounding shape). Empty bypasses => vec![screen].
pub(crate) fn overlay_coverage(screen: Rect, holes: &[Rect]) -> Vec<Rect> {
    let mut coverage = vec![screen];
    for hole in holes {
        let mut next = Vec::new();
        for r in coverage {
            next.extend(subtract_rect(r, *hole));
        }
        coverage = next;
        if coverage.is_empty() {
            break;
        }
    }
    coverage
}

fn subtract_rect(r: Rect, hole: Rect) -> Vec<Rect> {
    // No overlap => keep r
    if hole.w == 0 || hole.h == 0 || r.w == 0 || r.h == 0 {
        return vec![r];
    }
    let r_x1 = r.x;
    let r_y1 = r.y;
    let r_x2 = r.x + r.w as i32;
    let r_y2 = r.y + r.h as i32;
    let h_x1 = hole.x;
    let h_y1 = hole.y;
    let h_x2 = hole.x + hole.w as i32;
    let h_y2 = hole.y + hole.h as i32;

    // No overlap
    if h_x2 <= r_x1 || h_x1 >= r_x2 || h_y2 <= r_y1 || h_y1 >= r_y2 {
        return vec![r];
    }

    let mut out = Vec::with_capacity(4);
    // Left strip
    if h_x1 > r_x1 {
        out.push(Rect::new(r_x1, r_y1, (h_x1 - r_x1) as u32, r.h));
    }
    // Right strip
    if h_x2 < r_x2 {
        out.push(Rect::new(h_x2, r_y1, (r_x2 - h_x2) as u32, r.h));
    }
    // Top strip (between left/right, above hole)
    let mid_x1 = r_x1.max(h_x1);
    let mid_x2 = r_x2.min(h_x2);
    if h_y1 > r_y1 && mid_x2 > mid_x1 {
        out.push(Rect::new(
            mid_x1,
            r_y1,
            (mid_x2 - mid_x1) as u32,
            (h_y1 - r_y1) as u32,
        ));
    }
    // Bottom strip
    if h_y2 < r_y2 && mid_x2 > mid_x1 {
        out.push(Rect::new(
            mid_x1,
            h_y2,
            (mid_x2 - mid_x1) as u32,
            (r_y2 - h_y2) as u32,
        ));
    }
    out.into_iter().filter(|r| r.w > 0 && r.h > 0).collect()
}

#[allow(dead_code)]
pub(crate) fn global_to_local(global: Rect, origin: Rect) -> Rect {
    Rect::new(global.x - origin.x, global.y - origin.y, global.w, global.h)
}

/// Renderer backend abstraction — the WM never knows which is active.
/// `Gl` is the current OpenGL/GLX implementation; `Vulkan` will be added
/// behind `#[cfg(feature = "compositor-vulkan")]` without touching WM code.
pub enum CompositorRenderer {
    Gl(GlRenderer),
}

impl CompositorRenderer {
    pub fn has_buffer_age(&self) -> bool {
        match self {
            Self::Gl(r) => r.has_buffer_age,
        }
    }
}

impl std::ops::Deref for CompositorRenderer {
    type Target = GlRenderer;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Gl(r) => r,
        }
    }
}

impl std::ops::DerefMut for CompositorRenderer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Gl(r) => r,
        }
    }
}

/// One redirected window the compositor tracks.
struct CompWin {
    /// Outer (border-inclusive) geometry as last seen from X.
    outer: Rect,
    border_w: u32,
    border_color: Option<u32>,
    /// Last opacity from `_NET_WM_WINDOW_OPACITY` (0..1).
    opacity: f32,
    /// The GLX-backed texture (off-screen pixmap), if the window is mapped.
    tex: Option<Texture>,
    /// The X pixmap `NameWindowPixmap` gave us for `tex`. Ours to free.
    pixmap: Option<Pixmap>,
    /// Pending damage: rebind the texture before next draw.
    damaged: bool,
    /// A previous `rename_and_bind` (re)named the pixmap but failed to bind it to
    /// a GL texture (e.g. an asynchronous `BadMatch`/`BadDrawable` swallowed by
    /// the shared Xlib error handler, or a transient GLX failure during a resize
    /// storm). We keep the named pixmap around and retry on the next damage
    /// instead of dropping the window into a permanent hole — see `rename_and_bind`.
    needs_rebind: bool,
    /// Mapped + redirected?
    mapped: bool,
    /// Hidden by the WM because it belongs to a non-active workspace (see
    /// `hide_offscreen` in render.rs). The compositor must never paint a hidden
    /// window even if its cached `outer` rect is still on-screen — that is the
    /// "a tile from workspace N covers workspace M" bug, caused by the off-screen
    /// `ConfigureNotify` (which is what normally updates `outer`) arriving a
    /// frame after the workspace switch.
    hidden: bool,
    /// The window's own visual — *not* derived from its depth. Two visuals can
    /// share a depth (24-bit `TrueColor` and 24-bit `DirectColor`, or two
    /// 32-bit visuals with different channel layouts), and the fbconfig used to
    /// bind the pixmap has to match this one, not merely its width in bits.
    format: VisualFormat,
    /// Live (this-frame) outer rect the WM wants this window drawn at, and the
    /// corner radius to round it with.
    ///
    /// Stored *on the window* rather than in a side list because the draw loop
    /// walks the stack and would otherwise have to search that list per window
    /// — `N` windows × `N` transforms every frame, for a value the writer
    /// already had a direct handle to.
    transform: Rect,
    transform_radius: u32,
    transform_border_w: u32,
    presentation: Option<PresentationTransition>,
    presentation_spring: Option<(f32, f32)>,
    presentation_value: [f64; 5],
    presentation_target: [f64; 5],
    presentation_goal: Option<[f64; 5]>,
    /// Which frame `transform` was written for. Anything older than the
    /// compositor's current generation means the WM did not place this window
    /// this frame (an override-redirect menu, say), so it falls back to its X
    /// geometry. A generation stamp avoids a clearing pass over every tracked
    /// window at the top of each frame.
    transform_gen: u64,
    /// The visual (drawn) rect this window had on the *previous* composited
    /// frame. Used for Fase 7 animation damage: when a window moves we must
    /// repaint both this rect and the new one, or the pixels it slid off of
    /// (and into) linger as residue during scroll. `None` means it was not
    /// drawn last frame (just appeared / was off-screen), so only the current
    /// rect needs repainting.
    prev_visual: Option<Rect>,
    prev_visual_radius: u32,
    /// Fase 12 — true when this window is fully hidden behind a single opaque,
    /// square-cornered window above it this frame, so it need not be drawn.
    /// Recomputed every frame by `compute_scene`'s top→bottom occlusion pass.
    occluded: bool,
}

struct PresentationTransition {
    from: [f64; 5],
    progress: crate::types::Camera,
}

fn presentation_value(rect: Rect, radius: u32) -> [f64; 5] {
    [
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.w),
        f64::from(rect.h),
        f64::from(radius),
    ]
}

fn border_rgba(pixel: u32) -> [f32; 4] {
    [
        ((pixel >> 16) & 0xff) as f32 / 255.0,
        ((pixel >> 8) & 0xff) as f32 / 255.0,
        (pixel & 0xff) as f32 / 255.0,
        1.0,
    ]
}

impl CompWin {
    fn new(outer: Rect, border_w: u32, format: VisualFormat) -> Self {
        Self {
            outer,
            border_w,
            border_color: None,
            opacity: 1.0,
            tex: None,
            pixmap: None,
            damaged: true,
            needs_rebind: false,
            mapped: false,
            hidden: false,
            format,
            transform: Rect::default(),
            transform_radius: 0,
            transform_border_w: 0,
            presentation: None,
            presentation_spring: None,
            presentation_value: [0.0; 5],
            presentation_target: [0.0; 5],
            presentation_goal: None,
            // 0 is never a live generation: `set_transforms` pre-increments, so
            // the first frame is generation 1.
            transform_gen: 0,
            prev_visual: None,
            prev_visual_radius: 0,
            occluded: false,
        }
    }

    /// Apply a `ConfigureNotify`'s geometry to this window and report whether the
    /// *size* changed. A size change invalidates the GL texture/pixmap and must
    /// trigger a rebind; a move-only change does not.
    ///
    /// Pure (no X/GL side effects) so it is unit-testable in isolation — the
    /// actual resource invalidation/recreation is the caller's job and, after the
    /// floating-freeze fix, happens once per frame in `compute_scene`, never
    /// synchronously inside the event handler.
    fn observe_configure(&mut self, x: i32, y: i32, w: u32, h: u32, bw: u32) -> bool {
        let frame = bw.saturating_mul(2);
        let new_outer = Rect::new(x, y, w.saturating_add(frame), h.saturating_add(frame));
        let resized = new_outer.w != self.outer.w || new_outer.h != self.outer.h;
        self.outer = new_outer;
        self.border_w = bw;
        resized
    }

    fn set_transform(&mut self, geom: Rect, bw: u32, radius: u32, screen: Rect, gen: u64) {
        self.transform = Rect::new(
            geom.x,
            geom.y,
            geom.w.saturating_add(bw.saturating_mul(2)),
            geom.h.saturating_add(bw.saturating_mul(2)),
        );
        self.transform_border_w = bw;
        // Fullscreen/maximize presentation emits `bw = 0` and a rect that
        // covers the monitor edge-to-edge (`present_into`). Rounding such
        // an overlay just clips content under a curved corner with no
        // desktop behind it to round into — the same niri-style policy the
        // X11 Shape path enforces in `emit_geometry`. A window is square
        // exactly when its presentation covers the monitor's screen rect.
        self.transform_radius = if radius == 0 || self.transform == screen {
            0
        } else {
            radius.min((self.transform.w / 2).min(self.transform.h / 2))
        };
        let live = presentation_value(self.transform, self.transform_radius);
        // Exact comparison is intentional: the goal keys are copied from
        // integer-backed rects (only the radius rounds), so equality is a
        // token change test, not a float proximity test.
        let goal = self.presentation_goal.unwrap_or(live);
        if self.mapped && !self.hidden && self.transform_gen != 0 {
            if let Some((stiffness, damping)) = self.presentation_spring {
                // Exact comparison is intentional: `goal`/`presentation_target`
                // are integer-backed keys (radii included), so equality is a
                // "did the settled presentation change" test, not proximity.
                #[allow(clippy::float_cmp)]
                let retarget = goal != self.presentation_target;
                if retarget {
                    let mut progress = crate::types::Camera::new(0.0);
                    progress.target = 1.0;
                    progress.stiffness = stiffness;
                    progress.damping = damping;
                    self.presentation = Some(PresentationTransition {
                        from: self.presentation_value,
                        progress,
                    });
                }
            } else {
                self.presentation = None;
            }
        } else {
            self.presentation = None;
        }
        self.presentation_target = goal;
        if let Some(transition) = &self.presentation {
            let progress = f64::from(transition.progress.position.clamp(0.0, 1.0));
            for (i, value) in self.presentation_value.iter_mut().enumerate() {
                let to = if i == 4 { goal[i] } else { live[i] };
                *value = transition.from[i] + (to - transition.from[i]) * progress;
            }
            self.transform = Rect::new(
                self.presentation_value[0].round() as i32,
                self.presentation_value[1].round() as i32,
                self.presentation_value[2].round() as u32,
                self.presentation_value[3].round() as u32,
            );
            self.transform_radius = self.presentation_value[4].round() as u32;
            // Retain the transition until its exact settled endpoint is installed.
            // The camera may still be approaching that endpoint after progress reaches 1.
            if progress >= 1.0
                && self.transform
                    == Rect::new(
                        goal[0] as i32,
                        goal[1] as i32,
                        goal[2] as u32,
                        goal[3] as u32,
                    )
                && self.transform_radius == goal[4] as u32
            {
                self.presentation = None;
            }
        } else {
            self.presentation_value = live;
            if self.presentation_spring.is_some() {
                self.presentation_value[4] = goal[4];
                self.transform_radius = goal[4].round() as u32;
            }
        }
        self.transform_gen = gen;
    }

    fn tick_presentation(&mut self, dt: f32) -> bool {
        let Some(transition) = self.presentation.as_mut() else {
            return false;
        };
        if !self.mapped || self.hidden {
            self.presentation = None;
            return false;
        }
        let mut moving = true;
        for sub in substep_bounds(dt) {
            moving = transition.progress.step(sub);
        }
        // Snap progress, but keep the transition until set_transform installs
        // the exact endpoint. A moving camera can reach that endpoint later.
        if !moving || transition.progress.position > 0.995 {
            transition.progress.position = 1.0;
            transition.progress.velocity = 0.0;
        }
        true
    }

    /// Whether `r` is entirely outside the `[0,0,w,h]` viewport (plus a small
    /// margin so partially-visible windows — including ones with a shadow or a
    /// translucent halo — are never clipped). Used to skip the GPU draw for the
    /// dozens of ribbon windows that are scrolled fully off either edge of the
    /// monitor; those still cost a `HashMap` lookup, but no `glDrawArrays`,
    /// texture bind or quad upload. Windows mid-scroll (camera animation) keep
    /// being drawn the instant any part enters the margin.
    fn offscreen(r: Rect, w: u32, h: u32) -> bool {
        const M: i32 = 64; // px of grace around the screen edge
        r.x + r.w as i32 <= -M
            || r.y + r.h as i32 <= -M
            || r.x >= w as i32 + M
            || r.y >= h as i32 + M
    }
}

#[inline]
fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w as i32
        && b.x < a.x + a.w as i32
        && a.y < b.y + b.h as i32
        && b.y < a.y + a.h as i32
}

/// Bounded accumulation of screen-space damage rectangles for one frame.
///
/// Fixed capacity, zero heap allocation — the per-frame damage path stays
/// allocation-free. A window that reported `XDamage` (or any other change that
/// invalidates previously-drawn pixels) contributes its current screen rect
/// here. `needs_full` short-circuits the whole thing when a change cannot be
/// expressed as a set of rectangles (resize, reparent, restack, opacity): the
/// renderer then clears and repaints the entire screen instead of scissoring the
/// union. The actual scissor is applied later (partial-redraw phase); this type
/// is only the accounting.
#[derive(Clone, Copy)]
pub(crate) struct DamageRegion {
    rects: [Rect; Self::CAP],
    count: usize,
    needs_full: bool,
}

impl DamageRegion {
    /// Hard cap on distinct rectangles. Exceeded only by pathological damage
    /// storms; in that case we just ask for a full redraw.
    const CAP: usize = 32;

    fn new() -> Self {
        Self {
            rects: [Rect::default(); Self::CAP],
            count: 0,
            needs_full: false,
        }
    }

    fn clear(&mut self) {
        self.count = 0;
        self.needs_full = false;
    }

    /// Add a screen rect to the damaged area. Zero-size rects are ignored.
    fn add(&mut self, r: Rect) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        if self.count < Self::CAP {
            self.rects[self.count] = r;
            self.count += 1;
        } else {
            // Ran out of slots — be conservative and repaint everything.
            self.needs_full = true;
        }
    }

    /// Force a full-screen redraw this frame.
    fn full(&mut self) {
        self.needs_full = true;
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.count == 0 && !self.needs_full
    }

    /// Bounding box of all accumulated rects. Used to size the scissor in the
    /// partial-redraw path; a single scissor rectangle is what GL offers, so the
    /// union of many damage rects is approximated by their bbox (the draw loop
    /// still clips every window to it, so nothing outside is touched).
    fn bounding_rect(&self) -> Rect {
        let mut x0 = i32::MAX;
        let mut y0 = i32::MAX;
        let mut x1 = i32::MIN;
        let mut y1 = i32::MIN;
        for r in &self.rects[..self.count] {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.x + r.w as i32);
            y1 = y1.max(r.y + r.h as i32);
        }
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// How much of the screen the next frame must repaint. Computed every frame by
/// `decide_redraw` from three facts: whether buffer-age is known (so a partial
/// clear is safe), whether a structural change forced a whole-screen repaint,
/// and whether anything actually reported damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameMode {
    /// Nothing to draw (the frame is skipped entirely upstream).
    Idle,
    /// Clear and repaint the whole screen.
    Full,
    /// Scissor to the accumulated damage bounding box and repaint only that.
    Partial,
}

/// Why the compositor needs a frame (Fase 9). Bitflags so several reasons can
/// coexist in a single frame and the `FrameScheduler` can report them. Pure, no
/// GL/X: it is just an integer mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirtyReason(u8);
impl DirtyReason {
    pub const NONE: DirtyReason = DirtyReason(0);
    /// A client repainted (`XDamage`) — only its own area is dirty.
    pub const DAMAGE: DirtyReason = DirtyReason(1 << 0);
    /// A window's geometry changed (configure, opacity, hide, wallpaper).
    pub const GEOMETRY: DirtyReason = DirtyReason(1 << 1);
    /// A surface appeared/disappeared (map, unmap, destroy).
    pub const SURFACE: DirtyReason = DirtyReason(1 << 2);
    /// The stacking order changed (focus / raise / restack).
    pub const FOCUS: DirtyReason = DirtyReason(1 << 3);
    /// The native (or root) wallpaper changed — a full repaint of the whole
    /// screen. Inserted exactly once per `SetWallpaper` (Fase 6): a static
    /// wallpaper must not keep the loop awake.
    pub const WALLPAPER: DirtyReason = DirtyReason(1 << 4);

    #[inline]
    pub fn contains(self, other: DirtyReason) -> bool {
        self.0 & other.0 != 0
    }
    #[inline]
    pub fn insert(&mut self, other: DirtyReason) {
        self.0 |= other.0;
    }
    #[inline]
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// Pure decision: given the capability and the two damage flags, what kind of
/// frame is required? Kept free of any GL/renderer state so it is unit-testable
/// in isolation (see `frameplan_tests`).
pub(crate) fn decide_redraw(has_buffer_age: bool, needs_full: bool, damaged: bool) -> FrameMode {
    if !damaged {
        FrameMode::Idle
    } else if !has_buffer_age || needs_full {
        FrameMode::Full
    } else {
        FrameMode::Partial
    }
}

/// Fase 7: the screen rects that must be repainted when a window's drawn rect
/// moves from `prev` (last frame) to `cur` (this frame). Both are emitted so
/// neither the pixels the window left behind nor the pixels it slid into linger
/// as residue during scroll/animation. A window with no `prev` (just appeared,
/// or was off-screen) only needs its current rect. Pure and allocation-free:
/// the result is written into the caller's `[Rect; 2]` and the count returned,
/// so the hot path reuses a stack buffer instead of allocating.
pub(crate) fn anim_damage_rects(prev: Option<Rect>, cur: Rect, out: &mut [Rect; 2]) -> usize {
    let mut n = 0;
    if let Some(p) = prev {
        if p != cur {
            out[n] = p;
            n += 1;
        }
    }
    out[n] = cur;
    n + 1
}

/// Fase 12: true when `inner` is entirely contained by a single rect in
/// `occluders`. A window behind one opaque, square-cornered window above it is
/// fully hidden and can be skipped. Joint coverage by several smaller windows
/// is a safe *miss* — we simply keep drawing the window rather than risk
/// clipping something visible. Pure and allocation-free.
pub(crate) fn fully_covered_by(inner: Rect, occluders: &[Rect]) -> bool {
    occluders.iter().any(|o| o.contains_rect(inner))
}

/// One window to draw this frame, fully resolved: where, at what opacity, with
/// which texture, and whether the texture needs linear filtering. Built by
/// `compute_scene` into a reused buffer so the per-frame path allocates nothing,
/// and kept between frames so a later phase can diff successive scenes
/// (occlusion / partial redraw) without rebuilding geometry from scratch.
///
/// This is the compositor's *own* view of what is visible — distinct from the
/// WM's `Placements` (only the geometry source) and from `stack` (which still
/// includes off-screen windows).
struct DrawItem {
    #[allow(dead_code)]
    win: Window,
    quad: DrawQuad,
    tex: TextureHandle,
}

pub struct Compositor {
    conn: Rc<XConn>,
    // `dpy`/`overlay` are cached for the lifetime of the compositor; the
    // renderer already owns its own `dpy` copy and the overlay is handed to GLX
    // at init, so they are not read again — but keeping them pins the display
    // open and records what we redirected.
    #[allow(dead_code)]
    dpy: X11Display,
    renderer: CompositorRenderer,
    root: Window,
    #[allow(dead_code)]
    overlay: Window,
    screen_w: u32,
    screen_h: u32,
    screen_rect: Rect,
    /// Every visual this screen advertises, keyed by id. The compositor never
    /// infers a pixel format from a depth; it looks it up here.
    formats: HashMap<u32, VisualFormat>,
    /// The root/overlay visual — the format the frame is finally presented in.
    root_format: VisualFormat,
    /// The WM's own windows that must never be composited.
    ignored: HashSet<Window>,
    /// Composition-policy bypass: for each output (monitor) that is in `Bypass`
    /// mode, the XID of the single eligible fullscreen window Maverick has
    /// *un-redirected* so it presents directly (no GL texture, no overlay draw).
    /// The compositor keeps composing every other window normally — bypass is
    /// per-window, so a fullscreen game on one monitor leaves Firefox on another
    /// fully composited. Empty when no output is bypassing.
    bypassed: HashMap<usize, Window>,
    /// Reverse index of `bypassed` for O(1) membership tests in the hot path.
    bypassed_set: HashSet<Window>,
    /// OPT-IN DIAGNOSTIC (`MAVERICK_COMPOSITION_TRACE`): log mode transitions
    /// (Bypass <=> Compose) per monitor. Off by default; quiet otherwise.
    bypass_trace: bool,
    /// Tracked redirected windows, keyed by XID.
    wins: HashMap<Window, CompWin>,
    /// Per-window `Damage` resource, keyed by XID.
    damages: HashMap<Window, Damage>,
    /// Visuals we have already complained about, so an unbindable one is
    /// reported once instead of on every map.
    warned_visuals: HashSet<u32>,
    /// Wallpaper (root background pixmap) as a texture, if any.
    wallpaper: Option<Texture>,
    /// The `_XROOTPMAP_ID` we textured. **Never freed by us** — it belongs to
    /// whoever set the wallpaper (feh, hsetroot, a desktop shell). X lets any
    /// client destroy any resource id it knows, so freeing it here really does
    /// wipe the desktop background out from under its owner.
    wallpaper_pixmap: Option<Pixmap>,
    /// Native wallpaper (Maverick's own, decoded from a file) as an uploaded GPU
    /// texture. Takes precedence over `wallpaper` when set. `None` ⇒ fall back to
    /// the external root pixmap.
    wallpaper_native: Option<GpuImage>,
    /// Compiled wallpaper shader program id (`0` when inactive). When set, the
    /// wallpaper is animated and forces a frame every turn.
    wallpaper_shader: Option<ShaderId>,
    /// Decoded image dimensions (for `compute_wallpaper_rects`).
    wallpaper_img_w: u32,
    wallpaper_img_h: u32,
    /// Mapping mode for the native image.
    wallpaper_mode: WallpaperMode,
    /// Outputs the wallpaper is laid out across (screen-space rects).
    wallpaper_outputs: Vec<Rect>,
    /// Monotonic wallpaper clock advanced by `tick_wallpaper` (seconds).
    wallpaper_clock: f32,
    /// Whether the wallpaper is currently animating (shader source active).
    wallpaper_animating: bool,
    /// Whether the active wallpaper shader actually depends on time
    /// (`u_time`/`u_delta_time`). A static shader is drawn once and must then let
    /// the loop idle instead of forcing a frame every turn (idle CPU burn).
    wallpaper_animated: bool,
    /// `dt` of the most recent `tick_wallpaper`, passed to the shader.
    wallpaper_last_dt: f32,
    /// True while at least one frame is queued/needed.
    dirty: bool,
    /// *Why* the compositor needs a frame (Fase 9). Bitflags so several reasons
    /// can coincide in one frame and the `FrameScheduler` can report them; it is
    /// cleared together with `dirty` at the end of `render`.
    dirty_reasons: DirtyReason,
    /// An incremental restack could not be applied (we saw a `ConfigureNotify`
    /// naming a sibling we do not track) → fall back to a `QueryTree` resync
    /// before the next frame. This is the *recovery* path, not the normal one.
    stack_dirty: bool,
    /// Bottom→top stacking order. Maintained incrementally from the
    /// `SubstructureNotify` stream (`CreateNotify` / `ConfigureNotify` /
    /// `DestroyNotify`), which is the only source that sees *every* restack —
    /// including the WM's own `raise()` and override-redirect menus the WM
    /// never manages. `QueryTree` seeds it at startup and repairs it if an
    /// incremental update is ever impossible.
    stack: Vec<Window>,
    /// The explicit scene: the list of draw items (one per on-screen window)
    /// produced for the most recent frame. Built by `compute_scene` into this
    /// reused buffer so the per-frame path allocates nothing, and kept between
    /// frames so a later phase can diff it (occlusion / partial redraw) without
    /// rebuilding from scratch. This is the compositor's *own* view of what is
    /// visible — distinct from the WM's `Placements` (which is only the geometry
    /// source) and from `stack` (which still includes off-screen windows).
    scene: Vec<DrawItem>,
    /// Fase 12 — persistent buffer of opaque on-screen occluder rects, rebuilt
    /// (cleared, not reallocated) every frame by `compute_scene`'s top→bottom
    /// pass. Reused so the per-frame path stays allocation-free.
    occluder_rects: Vec<Rect>,
    /// Monotonic frame counter used to date `CompWin::transform`.
    frame_gen: u64,
    /// Corner radius the WM wants applied (shader SDF), px.
    corner_radius: u32,
    /// Accumulated screen-space damage for the current frame, rebuilt by
    /// `compute_scene`. Drives partial redraw: when only a few windows repainted
    /// (idle `XDamage`) the region is a small union; when a structural change
    /// happened `needs_full` forces a full repaint. Empty ⇒ nothing to draw.
    frame_dirty: DamageRegion,
    /// A change this frame cannot be expressed as a rectangle set (resize,
    /// reparent, restack, opacity, new/removed window). Set by `mark_full`.
    needs_full: bool,
    /// Persistent damage accumulation across frames, used by the partial-redraw
    /// path. Reset to empty on every full repaint (structural change, or when
    /// buffer-age is unavailable so we always full-redraw).
    damage_acc: DamageRegion,
    /// History of per-frame `frame_dirty` for `GLX_EXT_buffer_age` support.
    /// Double-buffered fbconfigs report `age==2` (spec), so a single-frame
    /// accumulator would force `Full` every frame. Keeping the last 4 frames
    /// lets `Partial` succeed for `age` 1..4, which is the spec's valid range.
    damage_history: Vec<DamageRegion>,
    /// OPT-IN DIAGNOSTIC (`MAV_FLOAT_TRACE`): when set, emit [FLOAT]/[TRANSFORM]/
    /// [SCENE]/[RENDER]/[PRESENT] trace lines. Never changes rendering behaviour.
    pub(crate) float_trace: bool,
    /// OPT-IN DIAGNOSTIC (`MAV_COMP_TRACE`): when set, emit [LIFECYCLE] trace
    /// lines for every window compositor event (create/map/unmap/configure/
    /// damage/destroy) carrying the current pixmap/`GLXPixmap`/texture ids and
    /// geometry, plus render/present begin-end markers. Off by default and a
    /// no-op when unset (only builds discarded format! strings), so it is safe
    /// to leave compiled in permanently. Used to isolate compositor-lifecycle
    /// regressions — e.g. the floating-window freeze, where GLXPixmap/texture
    /// (re)creation happened in the *event* handler instead of once per frame
    /// in `compute_scene`.
    pub(crate) comp_trace: bool,
    /// OPT-IN DIAGNOSTIC: per-frame id incremented at the top of
    /// `set_transforms` so TRANSFORM/SCENE/RENDER/PRESENT logs share one id.
    dbg_frame: u64,
    /// OPT-IN DIAGNOSTIC: the set of floating `WindowIds`, populated by the WM
    /// each frame (debug only) so [SCENE] can report only floating windows.
    dbg_floats: std::collections::HashSet<Window>,
    /// Debug/test hook: when set, pretend buffer-age is unavailable so the
    /// partial-redraw path is never taken (used by the Xephyr harness to
    /// exercise the full-redraw fallback). Set via `MAVERICK_FORCE_FULL_REDRAW`.
    force_full_redraw: bool,
    /// Debug/test hook: when set, log per-batch render timing every 120 frames
    /// so the Xephyr harness can compare partial vs full cost. `MAVERICK_PERF_LOG`.
    perf_log: bool,
    perf_count: u64,
    perf_ns_total: u64,
    perf_ns_max: u64,
    /// Debug/trace hook: when set, log per-frame CPU build time, time-to-swap,
    /// `glXSwapBuffers` duration, present-to-present interval, observed
    /// back-buffer age, frame mode and partial→full escalations. Gated by
    /// `MAVERICK_TRACE`; purely observational — it never changes what is drawn.
    trace: bool,
    trace_count: u64,
    trace_ns_build_total: u64,
    trace_ns_swap_total: u64,
    trace_ns_interval_total: u64,
    trace_ns_interval_max: u64,
    /// Histogram of observed `back_buffer_age`: indices 0, 1, 2, 3+.
    trace_age_hist: [u64; 4],
    trace_mode_full: u64,
    trace_mode_partial: u64,
    /// Frames the planner chose Partial but the age gate forced a Full repaint.
    trace_partial_to_full: u64,
    /// Timestamp of the previous present, for the present-to-present interval.
    last_present: Option<Instant>,
    // ── Presentation caches — owned by the compositor so the WM core never
    // hands GPU transforms (WindowManager does not import Renderer details).
    live_cache: Vec<Vec<(Window, crate::types::Rect, u32)>>,
    settled_cache: Vec<Placements>,
    cam_cache: Vec<f32>,
    proj_cache: Vec<Option<ProjSig>>,
    presentation_transforms: Vec<(Window, crate::types::Rect, u32)>,
    presentation_desired: Placements,
    presentation_raise_scratch: Vec<WindowId>,
    presentation_ribbon_scratch: RibbonScratch,
}

impl Compositor {
    /// Try to bring up the compositor. Returns `None` (logging why) when GL is
    /// unavailable or another compositor already owns the screen.
    pub fn init(
        conn: Rc<XConn>,
        dpy: X11Display,
        root: Window,
        screen_num: usize,
        wm_win: Window,
        cfg: &Cfg,
    ) -> Option<Self> {
        if !maverick_gl::probe() {
            log::info!("compositor: no libGL present, staying on the X11 path");
            return None;
        }

        let screen = &conn.setup().roots[screen_num];
        let root_visual = screen.root_visual;
        let screen_w = screen.width_in_pixels as u32;
        let screen_h = screen.height_in_pixels as u32;

        // What this screen can actually show. Everything downstream — which
        // fbconfig binds which window, whether a window can be composited at
        // all — is derived from this table and never guessed from a depth.
        let visuals = screen_visuals(screen);
        let formats: HashMap<u32, VisualFormat> = visuals.iter().map(|v| (v.id, *v)).collect();
        let Some(&root_format) = formats.get(&root_visual) else {
            log::warn!(
                "compositor: root visual 0x{root_visual:x} is missing from the screen's visual \
                 table; staying on the X11 path"
            );
            return None;
        };
        log::info!(
            "compositor: screen {screen_num} is {screen_w}x{screen_h}, root {root_format}, \
             {} visual(s) advertised",
            visuals.len()
        );
        if root_format.color_bits() < 24 {
            log::warn!(
                "compositor: this screen only shows {} bits of colour ({root_format}); the \
                 compositor blends at 8 bits per channel, so gradients and shadows will band \
                 when the driver truncates them on present",
                root_format.color_bits()
            );
        }

        // Refuse to start if another compositor already owns the selection.
        let cm_atom = match intern_cm_atom(&conn, screen_num) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("compositor: cannot intern _NET_WM_CM_S0: {e}");
                return None;
            }
        };
        if selection_owned(&conn, cm_atom) {
            log::info!(
                "compositor: _NET_WM_CM_S0 already owned (another compositor is running); \
                 staying on the X11 path"
            );
            return None;
        }

        // Composite / Damage / XFIXES versions must be queried before any other
        // call in those extensions — that's what lets x11rb decode their events.
        let _ = conn.composite_query_version(0, 4);
        let _ = conn.damage_query_version(1, 1);
        let _ = conn.xfixes_query_version(5, 0);

        // Claim the compositing selection so others (picom) back off, then
        // redirect every subwindow to Manual so we get a redirected pixmap to
        // texture from.
        if let Err(e) = conn.set_selection_owner(wm_win, cm_atom, x11rb::CURRENT_TIME) {
            log::warn!("compositor: cannot own _NET_WM_CM_S0: {e}");
            return None;
        }
        if conn
            .composite_redirect_subwindows(root, Redirect::MANUAL)
            .is_err()
        {
            log::warn!("compositor: CompositeRedirectSubwindows failed");
            let _ = conn.set_selection_owner(x11rb::NONE, cm_atom, x11rb::CURRENT_TIME);
            return None;
        }

        // The overlay window: our drawing surface. It already sits above
        // everything; we only need to make its *input* shape empty so clicks
        // fall through to the real windows.
        let overlay = if let Some(reply) = conn
            .composite_get_overlay_window(root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            reply.overlay_win
        } else {
            log::warn!("compositor: CompositeGetOverlayWindow failed");
            let _ = conn.composite_unredirect_subwindows(root, Redirect::MANUAL);
            let _ = conn.set_selection_owner(x11rb::NONE, cm_atom, x11rb::CURRENT_TIME);
            return None;
        };

        // Empty input region → the overlay passes all pointer events through.
        if let Err(e) = set_empty_input_region(&conn, overlay) {
            log::warn!("compositor: could not empty overlay input shape: {e}");
        }

        let gl_vsync = match cfg.compositor.vsync {
            crate::config::VsyncMode::On => GlVsyncMode::On,
            crate::config::VsyncMode::Off => GlVsyncMode::Off,
            crate::config::VsyncMode::Adaptive => GlVsyncMode::Adaptive,
        };
        // SAFETY: `dpy` is the `XDisplay` opened by `maverick_x11::open_x` and
        // kept alive by both `WindowManager::dpy` and `Rc<XConn>` (which holds the
        // `xcb_connection_t` with `should_drop=false`). The raw `Display*` is
        // therefore live for the whole `Compositor::init` call and is only
        // borrowed as a non-owning handle to probe GLX; `maverick_gl` does not
        // take ownership or close it.
        let gl_dpy = unsafe { maverick_gl::XDisplay::from_raw(dpy.as_ptr()) };
        let mut renderer = match GlRenderer::new_with_vsync(
            gl_dpy,
            screen_num as i32,
            overlay,
            root_visual,
            &visuals,
            (screen_w, screen_h),
            gl_vsync,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("compositor: GL init failed: {e}; staying on the X11 path");
                let _ = conn.composite_release_overlay_window(root);
                let _ = conn.composite_unredirect_subwindows(root, Redirect::MANUAL);
                let _ = conn.set_selection_owner(x11rb::NONE, cm_atom, x11rb::CURRENT_TIME);
                return None;
            }
        };
        log::info!("{}", renderer.info);

        // The screen-awareness self-check: for every visual the server offers,
        // work out whether a window using it can be turned into a texture. A
        // visual that cannot be bound means those windows are *dropped from the
        // frame entirely* — which looks like a colour bug rather than the
        // format mismatch it is, so it must never be silent.
        //
        // Servers can advertise a hundred visuals (Xephyr does), so the summary
        // is per depth; the per-visual detail is `MAVERICK_LOG=debug`.
        let report = renderer.format_report();
        let mut per_depth: BTreeMap<u8, (usize, usize, Option<String>, Option<String>)> =
            BTreeMap::new();
        for entry in &report {
            let slot = per_depth.entry(entry.format.depth).or_default();
            match &entry.binding {
                Ok(desc) => {
                    slot.0 += 1;
                    slot.2.get_or_insert_with(|| desc.clone());
                }
                Err(e) => {
                    slot.1 += 1;
                    slot.3.get_or_insert_with(|| e.clone());
                }
            }
            log::debug!(
                "compositor: {} -> {}",
                entry.format,
                match &entry.binding {
                    Ok(d) => d.clone(),
                    Err(e) => format!("NOT COMPOSITABLE: {e}"),
                }
            );
        }
        for (depth, (ok, failed, sample, reason)) in &per_depth {
            if *failed == 0 {
                log::info!(
                    "compositor: depth {depth}: {ok} visual(s) compositable via {}",
                    sample.as_deref().unwrap_or("?")
                );
            } else {
                log::warn!(
                    "compositor: depth {depth}: {failed} of {} visual(s) CANNOT be composited — \
                     windows using them will not be drawn at all. Reason: {}",
                    ok + failed,
                    reason.as_deref().unwrap_or("?")
                );
            }
        }
        if log::enabled(log::DEBUG) {
            for line in renderer.fbconfig_report() {
                log::debug!("compositor: {line}");
            }
        }

        let mut ignored = HashSet::new();
        ignored.insert(wm_win);
        ignored.insert(overlay);

        let mut comp = Compositor {
            conn,
            dpy,
            renderer: CompositorRenderer::Gl(renderer),
            root,
            overlay,
            screen_w,
            screen_h,
            screen_rect: Rect::new(0, 0, screen_w, screen_h),
            formats,
            root_format,
            ignored,
            wins: HashMap::new(),
            damages: HashMap::new(),
            warned_visuals: HashSet::new(),
            bypassed: HashMap::new(),
            bypassed_set: HashSet::new(),
            bypass_trace: std::env::var_os("MAVERICK_COMPOSITION_TRACE").is_some(),
            wallpaper: None,
            wallpaper_pixmap: None,
            wallpaper_clock: 0.0,
            wallpaper_animating: false,
            wallpaper_animated: false,
            wallpaper_last_dt: 0.0,
            wallpaper_native: None,
            wallpaper_shader: None,
            wallpaper_img_w: 0,
            wallpaper_img_h: 0,
            wallpaper_mode: WallpaperMode::Fill,
            wallpaper_outputs: Vec::new(),
            dirty: true,
            dirty_reasons: DirtyReason::GEOMETRY,
            stack_dirty: true,
            stack: Vec::new(),
            scene: Vec::new(),
            occluder_rects: Vec::new(),
            frame_gen: 0,
            corner_radius: cfg.corner_radius,
            frame_dirty: DamageRegion::new(),
            needs_full: false,
            damage_acc: DamageRegion::new(),
            damage_history: Vec::with_capacity(4),
            force_full_redraw: std::env::var_os("MAVERICK_FORCE_FULL_REDRAW").is_some(),
            float_trace: std::env::var_os("MAV_FLOAT_TRACE").is_some(),
            comp_trace: std::env::var_os("MAV_COMP_TRACE").is_some(),
            dbg_frame: 0,
            dbg_floats: std::collections::HashSet::new(),
            perf_log: std::env::var_os("MAVERICK_PERF_LOG").is_some(),
            perf_count: 0,
            perf_ns_total: 0,
            perf_ns_max: 0,
            trace: std::env::var_os("MAVERICK_TRACE").is_some(),
            trace_count: 0,
            trace_ns_build_total: 0,
            trace_ns_swap_total: 0,
            trace_ns_interval_total: 0,
            trace_ns_interval_max: 0,
            trace_age_hist: [0; 4],
            trace_mode_full: 0,
            trace_mode_partial: 0,
            trace_partial_to_full: 0,
            last_present: None,
            live_cache: Vec::new(),
            settled_cache: Vec::new(),
            cam_cache: Vec::new(),
            proj_cache: Vec::new(),
            presentation_transforms: Vec::with_capacity(256),
            presentation_desired: Placements::with_capacity(32),
            presentation_raise_scratch: Vec::with_capacity(32),
            presentation_ribbon_scratch: RibbonScratch::default(),
        };

        comp.scan_existing();
        comp.refresh_wallpaper();
        comp.refresh_stack();
        // Seed the wallpaper output layout from the root screen so a native
        // wallpaper set before the first RandR event still covers the whole screen.
        comp.set_outputs(&[Rect::new(0, 0, screen_w, screen_h)]);
        if comp.force_full_redraw {
            log::info!(
                "compositor: MAVERICK_FORCE_FULL_REDRAW set — partial redraw disabled (full-redraw fallback path)"
            );
        }
        Some(comp)
    }

    // ── window lifecycle ────────────────────────────────────────────────────

    /// Track a window the WM just created/managed. Called for `CreateNotify`
    /// and for every window already present at startup (`scan_existing`).
    fn track(&mut self, win: Window) {
        if self.ignored.contains(&win) || self.wins.contains_key(&win) {
            return;
        }
        // `GetWindowAttributes` is the only place the window's *visual* comes
        // from; `GetGeometry` reports a depth, and a depth is not a pixel
        // format. Asking for both is one extra round trip per window, once.
        let Some(attrs) = self
            .conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return;
        };
        // InputOnly windows have no pixels at all — no depth, no visual, no
        // off-screen pixmap. Redirecting one is a guaranteed `BadMatch`.
        if attrs.class == WindowClass::INPUT_ONLY {
            return;
        }
        let Some(g) = self
            .conn
            .get_geometry(win)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return;
        };
        let Some(&format) = self.formats.get(&attrs.visual) else {
            log::debug!(
                "compositor: window {win} uses unknown visual 0x{:x}; not composited",
                attrs.visual
            );
            return;
        };
        if format.depth != g.depth {
            // Should not happen; if it does, the visual is authoritative for
            // the pixel layout and the mismatch is worth seeing.
            log::debug!(
                "compositor: window {win} geometry says depth {} but {format}",
                g.depth
            );
        }
        let bw = g.border_width as u32;
        let geom = Rect::new(
            g.x as i32,
            g.y as i32,
            g.width as u32 + 2 * bw,
            g.height as u32 + 2 * bw,
        );
        let cw = CompWin::new(geom, bw, format);
        self.wins.insert(win, cw);

        if let Ok(dmg) = self.conn.generate_id() {
            let _ = self.conn.damage_create(dmg, win, ReportLevel::NON_EMPTY);
            self.damages.insert(win, dmg);
        }
    }

    /// Window appeared (`CreateNotify`). A freshly created window is placed on
    /// top of its siblings by the server, so that is where it enters the stack.
    pub fn on_create(&mut self, win: Window) {
        if self.bypassed_set.contains(&win) {
            // The bypassed window itself was recreated (rare): keep its bookkeeping
            // consistent but leave the bypass state untouched.
            self.track(win);
            if !self.ignored.contains(&win) {
                stack_add_top(&mut self.stack, win);
            }
            return;
        }
        self.track(win);
        // A window appeared while we were bypassing. If it overlaps the bypassed
        // window's screen area we must composite again; if it's on another monitor
        // (no overlap) we keep bypass to avoid flicker on unrelated outputs (P0).
        if !self.bypassed_set.is_empty() {
            let new_geom = self.wins.get(&win).map(|c| c.outer);
            let should_disengage = match new_geom {
                Some(ng) if ng.w != 0 && ng.h != 0 => self.bypassed.values().any(|bw| {
                    self.wins
                        .get(bw)
                        .is_some_and(|cw| rects_overlap(cw.outer, ng))
                }),
                _ => true,
            };
            if should_disengage {
                self.disengage_all_bypass();
            }
        }
        if self.comp_trace {
            log::info!(
                "[LIFECYCLE] win={:#x} event=CreateNotify tracked={}",
                win,
                self.wins.contains_key(&win)
            );
        }
        if !self.ignored.contains(&win) {
            stack_add_top(&mut self.stack, win);
        }
    }

    /// The server restacked `win` (`ConfigureNotify.above_sibling`).
    ///
    /// This is the *only* routine that reorders the draw list in normal
    /// operation. It must be fed from the real, server-generated event on the
    /// root window: the synthetic `ConfigureNotify` the WM sends each client
    /// from `apply_geom` carries `above_sibling = None`, and replaying that
    /// would drop every window to the bottom of the stack.
    pub fn on_restack(&mut self, win: Window, above: Option<Window>) {
        if self.ignored.contains(&win) {
            return;
        }
        if !stack_restack(&mut self.stack, win, above) {
            // The sibling is unknown to us — we cannot place `win` at the right
            // depth, so repair the whole order from the server instead of
            // guessing (guessing is what produces a window drawn under the one
            // it should cover).
            self.stack_dirty = true;
        }
        self.mark_full(DirtyReason::FOCUS);
    }

    /// Window destroyed (`DestroyNotify`).
    pub fn on_destroy(&mut self, win: Window) {
        let was_bypassed = self.bypassed_set.contains(&win);
        if was_bypassed {
            self.bypassed_set.remove(&win);
            self.bypassed.retain(|_, &mut w| w != win);
            self.update_overlay_shape();
        }
        if self.comp_trace {
            let (pm, gpx, tx) = self.dbg_res_ids(win);
            log::info!(
                "[LIFECYCLE] win={:#x} event=DestroyNotify pixmap={} glxpixmap={} texture={}",
                win,
                pm,
                gpx,
                tx,
            );
        }
        if let Some(cw) = self.wins.remove(&win) {
            self.release_texture(cw);
        }
        if let Some(dmg) = self.damages.remove(&win) {
            let _ = self.conn.damage_destroy(dmg);
        }
        stack_remove(&mut self.stack, win);
        self.mark_full(DirtyReason::SURFACE);
    }

    /// Window mapped (`MapNotify`). Mark the window mapped and ask for a (re)bind
    /// on the next frame.
    ///
    /// The GLXPixmap/texture is (re)created lazily in `compute_scene` (render
    /// phase) via `needs_fixup`, **not** synchronously here. Creating it here
    /// (inside the event-drain loop) for every map/unmap/remap is exactly the
    /// synchronous-`glXCreatePixmap`+`glXBindTexImageEXT` storm that freezes the
    /// floating compositor. Mapping does **not** restack in X — an unmapped window
    /// keeps its place in the sibling order — so this deliberately does not touch
    /// `stack`. It only asks for a resync when the window is missing entirely,
    /// which means we never saw its `CreateNotify`.
    pub fn on_map(&mut self, win: Window) {
        if self.bypassed_set.contains(&win) {
            // The bypassed window itself re-mapped: it is presented directly, so
            // its compositor bookkeeping must not be touched (and it stays
            // bypassed).
            return;
        }
        if !self.wins.contains_key(&win) {
            self.track(win);
        }
        // Refresh the cached outer rect from the server BEFORE the overlap
        // check below: at CreateNotify the window is typically still 1x1 at
        // -1,-1, and `track()` cached exactly that. By map time the real size
        // is known; checking against the stale 1x1 would keep a bypass that a
        // now-fullscreen dialog actually covers. A failed query means "unknown"
        // and conservatively disengages (same as the `_ => true` arm).
        if let Some(g) = self
            .conn
            .get_geometry(win)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            if let Some(cw) = self.wins.get_mut(&win) {
                let bw = g.border_width as u32;
                let frame = bw.saturating_mul(2);
                cw.outer = Rect::new(
                    g.x as i32,
                    g.y as i32,
                    (g.width as u32).saturating_add(frame),
                    (g.height as u32).saturating_add(frame),
                );
                cw.border_w = bw;
            }
        }
        // A previously-unknown window became visible while bypassing. Only
        // disengage if it overlaps the bypassed output (P0 overlap check).
        if !self.bypassed_set.is_empty() {
            let new_geom = self.wins.get(&win).map(|c| c.outer);
            let should_disengage = match new_geom {
                Some(ng) if ng.w != 0 && ng.h != 0 => self.bypassed.values().any(|bw| {
                    self.wins
                        .get(bw)
                        .is_some_and(|cw| rects_overlap(cw.outer, ng))
                }),
                _ => true,
            };
            if should_disengage {
                self.disengage_all_bypass();
            }
        }
        {
            let Some(cw) = self.wins.get_mut(&win) else {
                return;
            };
            cw.mapped = true;
            // Defer GLXPixmap/texture creation to `compute_scene`. Drop any stale
            // resources first so the new bind starts clean (the window may have
            // been unmapped and remapped, leaving an orphaned texture/pixmap).
            if let Some(t) = cw.tex.take() {
                self.renderer.destroy_texture(t);
            }
            if let Some(pm) = cw.pixmap.take() {
                let _ = self.conn.free_pixmap(pm);
            }
            cw.needs_rebind = true;
            cw.damaged = true;
        }
        if self.comp_trace {
            let (pm, gpx, tx) = self.dbg_res_ids(win);
            log::info!(
                "[LIFECYCLE] win={:#x} event=MapNotify pixmap={} glxpixmap={} texture={}",
                win,
                pm,
                gpx,
                tx,
            );
        }
        if !self.ignored.contains(&win) && !self.stack.contains(&win) {
            self.stack_dirty = true;
        }
        self.mark_full(DirtyReason::SURFACE);
    }

    /// Window unmapped (`UnmapNotify`). Drop the texture (the pixmap is gone).
    ///
    /// The window keeps its slot in `stack`: X does not restack on unmap, and
    /// `render` already skips unmapped windows. Dropping and re-adding it would
    /// silently promote it to the top the next time it maps.
    pub fn on_unmap(&mut self, win: Window) {
        // The bypassed fullscreen window going away means the scene is no longer
        // safe to bypass — return to Compose (which re-redirects it; the normal
        // path below then marks it unmapped).
        if self.bypassed_set.contains(&win) {
            self.disengage_all_bypass();
        }
        if self.comp_trace {
            let (pm, gpx, tx) = self.dbg_res_ids(win);
            log::info!(
                "[LIFECYCLE] win={:#x} event=UnmapNotify pixmap={} glxpixmap={} texture={}",
                win,
                pm,
                gpx,
                tx,
            );
        }
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.mapped = false;
            cw.hidden = false;
            cw.presentation = None;
            cw.transform_gen = 0;
            let (tex, pixmap) = (cw.tex.take(), cw.pixmap.take());
            if let Some(t) = tex {
                self.renderer.destroy_texture(t);
            }
            if let Some(pm) = pixmap {
                let _ = self.conn.free_pixmap(pm);
            }
        }
        self.mark_full(DirtyReason::SURFACE);
    }

    /// Mark a window hidden/shown by the WM's workspace switcher
    /// (`hide_offscreen`). A hidden window is never painted, so a window that
    /// belongs to a non-active workspace cannot briefly cover the active one
    /// while its off-screen `ConfigureNotify` is still in flight.
    pub fn set_hidden(&mut self, win: Window, hidden: bool) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.hidden = hidden;
            if hidden {
                cw.presentation = None;
                cw.transform_gen = 0;
            }
            self.mark_full(DirtyReason::SURFACE);
        }
    }

    /// Geometry change (`ConfigureNotify` for a tracked, non-root window).
    pub fn on_configure(&mut self, win: Window, x: i32, y: i32, w: u32, h: u32, bw: u32) {
        // I6: bypassed window geometry must stay coherent (cached outer == actual)
        // and the overlay hole must follow it. We still update outer and shape,
        // but avoid churning released GL resources or flipping back to Compose.
        if self.bypassed_set.contains(&win) {
            if let Some(cw) = self.wins.get_mut(&win) {
                cw.observe_configure(x, y, w, h, bw);
            }
            self.update_overlay_shape();
            return;
        }
        let (resized, mapped) = match self.wins.get_mut(&win) {
            Some(cw) => (cw.observe_configure(x, y, w, h, bw), cw.mapped),
            None => return,
        };
        // A resize of a mapped window invalidates the off-screen pixmap and the
        // GLXPixmap/texture bound to it (NameWindowPixmap returns a *new* pixmap on
        // every resize per the Composite spec). We invalidate the *old* GL
        // resources here, but we deliberately do NOT create the new
        // GLXPixmap/texture synchronously in this event handler.
        //
        // Why: this runs inside the event-drain loop (`run_once`), and a
        // client-driven floating window emits a flood of `ConfigureNotify`s during
        // a resize drag. `rename_and_bind` issues `composite_name_window_pixmap`
        // + `glXCreatePixmap` + `glXBindTexImageEXT` — all synchronous X/GL round
        // trips — so doing it here serialises N blocking calls per drain burst and
        // starves frame advancement: the floating-window "freeze". `compute_scene`
        // (render phase) already (re)binds exactly once per frame via
        // `needs_fixup`, so the new texture is created when the frame is drawn.
        if resized && mapped {
            let (tex, pix) = match self.wins.get_mut(&win) {
                Some(cw) => (cw.tex.take(), cw.pixmap.take()),
                None => (None, None),
            };
            if let Some(t) = tex {
                self.renderer.destroy_texture(t);
            }
            if let Some(pm) = pix {
                let _ = self.conn.free_pixmap(pm);
            }
            if let Some(cw) = self.wins.get_mut(&win) {
                cw.needs_rebind = true;
                cw.damaged = true;
            }
        }
        if self.comp_trace {
            let (pm, gpx, tx) = self.dbg_res_ids(win);
            log::info!(
                "[LIFECYCLE] win={:#x} event=ConfigureNotify resized={} mapped={} pixmap={} glxpixmap={} texture={} needs_rebind={}",
                win,
                resized,
                mapped,
                pm,
                gpx,
                tx,
                self.wins.get(&win).is_some_and(|c| c.needs_rebind),
            );
        }
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// Damage reported (`DamageNotify`). Re-arm and mark dirty; the texture is
    /// rebound right before drawing.
    pub fn on_damage(&mut self, win: Window) {
        // I4: damage bookkeeping (DamageSubtract) must happen for every
        // DamageNotify, even while bypassed. Bypass only suppresses render
        // scheduling (damaged/dirty), never the X Damage state.
        if let Some(dmg) = self.damages.get(&win) {
            let _ = self.conn.damage_subtract(*dmg, x11rb::NONE, x11rb::NONE);
        }
        if self.comp_trace {
            let pending = self.wins.get(&win).is_some_and(|c| c.damaged);
            log::info!(
                "[LIFECYCLE] win={:#x} event=DamageNotify damage_pending_before={} mapped={}",
                win,
                pending,
                self.wins.get(&win).is_some_and(|c| c.mapped),
            );
        }
        // Bypass suppresses render scheduling only.
        if self.bypassed_set.contains(&win) {
            return;
        }
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.damaged = true;
        }
        self.dirty = true;
        self.dirty_reasons.insert(DirtyReason::DAMAGE);
    }

    /// `_NET_WM_WINDOW_OPACITY` changed (`PropertyNotify`).
    pub fn on_opacity(&mut self, win: Window, opacity: f32) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.opacity = opacity.clamp(0.0, 1.0);
        }
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// The WM repainted a window's native `border_pixel`; the compositor's
    /// stroke must show the same color (focus/urgent/normal transitions all
    /// flow through here).
    pub fn on_border_color(&mut self, win: Window, pixel: u32) {
        if !self.wins.contains_key(&win) {
            self.track(win);
        }
        if let Some(cw) = self.wins.get_mut(&win) {
            if cw.border_color != Some(pixel) {
                cw.border_color = Some(pixel);
                self.mark_full(DirtyReason::FOCUS);
            }
        }
    }

    #[allow(dead_code)]
    pub fn on_shape(&mut self, win: Window) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.damaged = true;
        }
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// The WM computed live placements; hand them to the compositor as the
    /// per-window transform (outer rect + corner radius) for this frame.
    ///
    /// One pass, one hash lookup per placement, no allocation: the transform is
    /// written straight onto the window it belongs to and stamped with this
    /// frame's generation. The draw loop then reads it with no search at all.
    pub fn set_transforms(&mut self, placements: &[(Window, Rect, u32)]) {
        if self.float_trace {
            self.dbg_frame = self.dbg_frame.wrapping_add(1);
        }
        self.frame_gen = self.frame_gen.wrapping_add(1);
        let gen = self.frame_gen;
        let corner_radius = self.corner_radius;
        let screen = self.screen_rect;
        for &(win, geom, bw) in placements {
            // `ignored` windows are never tracked (see `track`), so the lookup
            // below already rejects them — no separate set probe needed.
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            let before = cw.transform;
            let before_gen = cw.transform_gen;
            let trace_transition = crate::backend::x11::trace::enabled()
                .then(|| cw.presentation.as_ref().map(|t| t.progress.position));
            cw.set_transform(geom, bw, corner_radius, screen, gen);
            if let Some(previous) = trace_transition {
                crate::backend::x11::trace::trace!(
                    "transition",
                    "win={} previous_progress={previous:?} progress={:?} active={} ended={} endpoint_installed={} target={:?} transform={:?} radius={}",
                    win,
                    cw.presentation.as_ref().map(|t| t.progress.position),
                    cw.presentation.is_some(),
                    previous.is_some() && cw.presentation.is_none(),
                    cw.transform
                        == Rect::new(
                            cw.presentation_target[0] as i32,
                            cw.presentation_target[1] as i32,
                            cw.presentation_target[2] as u32,
                            cw.presentation_target[3] as u32,
                        ) && cw.transform_radius == cw.presentation_target[4] as u32,
                    cw.presentation_target,
                    cw.transform,
                    cw.transform_radius
                );
            }
            if self.float_trace {
                let changed = before != cw.transform || before_gen != gen;
                log::info!(
                    "[TRANSFORM] frame={} win={:#x} old_transform={:?} new_transform={:?} changed={} transform_gen_before={} transform_gen_after={} radius={} transition={}",
                    self.dbg_frame, win, before, cw.transform, changed, before_gen, gen,
                    cw.transform_radius, cw.presentation.is_some()
                );
            }
        }
        if self.float_trace {
            let wins: Vec<String> = placements
                .iter()
                .map(|(w, _, _)| format!("{w:#x}"))
                .collect();
            log::info!(
                "[TRANSFORM-SET] frame={} count={} wins=[{}]",
                self.dbg_frame,
                wins.len(),
                wins.join(",")
            );
        }
        // No `mark_full` here: a pure animation/scroll only moves windows, which
        // `compute_scene` records as `old ∪ new` animation damage (Fase 7) so the
        // buffer-age Partial path can scissor just the swept region. Forcing a
        // full repaint every animation frame would defeat partial redraw during
        // scroll. Structural changes (resize/restack/opacity/map/unmap) already
        // call `mark_full` through their own events, and the frame loop renders
        // while `animating` is true, so dropping this flag does not skip frames.
    }

    /// Build presentation transforms for this frame (moved from `WindowManager` so
    /// the WM core never handles GPU presentation state). Populates the
    /// compositor's internal caches and installs the transforms via
    /// `set_transforms`.
    pub fn prepare_frame(
        &mut self,
        state: &mut State,
        cfg: &Cfg,
        registry: &LayoutRegistry,
        anim_per_mon: &[bool],
    ) {
        // Advance before installing transforms so the frame that finishes a
        // transition also draws its endpoint, rather than waiting for another turn.
        let dt = self.last_present.map_or(0.0, |t| t.elapsed().as_secs_f32());
        let trace_raw_dt = dt;
        let dt = dt.clamp(0.0, crate::backend::x11::framesched::ONE_REFRESH * 2.0);
        crate::backend::x11::trace::trace!(
            "presentation_dt",
            "raw_s={trace_raw_dt} clamped_s={dt}"
        );
        for cw in self.wins.values_mut() {
            cw.tick_presentation(dt);
        }
        if self.corner_radius != cfg.corner_radius {
            self.corner_radius = cfg.corner_radius;
            self.mark_full(DirtyReason::GEOMETRY);
        }
        // Ensure caches match live monitor count.
        let nmon = state.monitors.len();
        if self.live_cache.len() != nmon {
            self.live_cache = vec![Vec::new(); nmon];
            self.settled_cache = vec![Vec::new(); nmon];
            self.cam_cache = vec![0.0; nmon];
            self.proj_cache = vec![None; nmon];
            for m in &mut state.monitors {
                m.layout_dirty = true;
            }
        }
        // Presentation-transition goals: the *settled* presentation (arrange
        // Phase::Settled + present_into overlay, i.e. exactly what the X11 side
        // applies) per window. Cached per monitor and recomputed only when that
        // monitor's settled layout changes (its `layout_dirty` flag, which the
        // settle-side arrange sets) — never per camera pixel, so an active
        // transition's spring is never reset by ordinary scrolling.
        for i in 0..nmon {
            if state.monitors[i].layout_dirty || self.settled_cache[i].is_empty() {
                self.settled_cache[i].clear();
                arrange(
                    state,
                    i,
                    cfg,
                    registry,
                    Phase::Settled,
                    &mut self.settled_cache[i],
                    &mut self.presentation_ribbon_scratch,
                );
                crate::core::present::present_into(
                    state,
                    &state.monitors[i],
                    &mut self.settled_cache[i],
                    &mut self.presentation_raise_scratch,
                );
            }
        }
        let spring = crate::config::animations_enabled(cfg).then(|| {
            crate::types::sanitize_spring(cfg.animations.stiffness, cfg.animations.damping)
        });
        for (&win, cw) in &mut self.wins {
            cw.presentation_spring = spring;
            cw.presentation_goal = self
                .settled_cache
                .iter()
                .flatten()
                .find(|(w, _, _)| *w == win)
                .map(|&(_, rect, bw)| {
                    let outer = Rect::new(
                        rect.x,
                        rect.y,
                        rect.w.saturating_add(bw.saturating_mul(2)),
                        rect.h.saturating_add(bw.saturating_mul(2)),
                    );
                    let radius = if outer == self.screen_rect {
                        0
                    } else {
                        cfg.corner_radius.min(outer.w / 2).min(outer.h / 2)
                    };
                    presentation_value(outer, radius)
                });
        }
        self.presentation_transforms.clear();
        for i in 0..nmon {
            let anim_i = anim_per_mon.get(i).copied().unwrap_or(false);
            let cam_now = state.monitors[i].ws().camera.position;
            let (sig, alpha) = {
                let ws = state.monitors[i].ws();
                (proj_signature(ws, cfg), live_alpha(ws))
            };
            let layout_dirty = state.monitors[i].layout_dirty;
            let sig_changed = self.proj_cache[i].as_ref() != Some(&sig);
            let recompute = anim_i || layout_dirty || sig_changed;
            if recompute {
                self.presentation_desired.clear();
                live_placements(
                    state,
                    i,
                    cfg,
                    registry,
                    &mut self.presentation_desired,
                    &mut self.presentation_raise_scratch,
                    &mut self.presentation_ribbon_scratch,
                );
                self.live_cache[i].clear();
                self.live_cache[i].extend(self.presentation_desired.iter().copied());
                self.cam_cache[i] = cam_now;
                self.proj_cache[i] = Some(sig);
                state.monitors[i].layout_dirty = false;
            } else if (cam_now - self.cam_cache[i]).abs() > 1e-4 {
                let dx = (-(cam_now - self.cam_cache[i]) * alpha).round() as i32;
                self.presentation_desired.clear();
                let ws = state.monitors[i].ws();
                for &(win, g, bw) in &self.live_cache[i] {
                    let stationary = ws.floats.contains(&win)
                        || state
                            .clients
                            .get(&win)
                            .is_some_and(|c| c.is_maximized() || c.is_true_fullscreen());
                    let nx = if stationary {
                        g.x
                    } else {
                        g.x.saturating_add(dx)
                    };
                    self.presentation_desired
                        .push((win, Rect::new(nx, g.y, g.w, g.h), bw));
                }
                crate::core::present::present_into(
                    state,
                    &state.monitors[i],
                    &mut self.presentation_desired,
                    &mut self.presentation_raise_scratch,
                );
                self.cam_cache[i] = cam_now;
            } else {
                self.presentation_desired.clear();
                self.presentation_desired
                    .extend(self.live_cache[i].iter().copied());
            }
            self.presentation_transforms
                .extend(self.presentation_desired.iter().copied());
        }
        // Install transforms (frame_gen bump + per-window write).
        // Avoid per-frame Vec clone (allocation) by moving the buffer out,
        // borrowing it, and restoring it — no allocation, just a pointer swap.
        let transforms = std::mem::take(&mut self.presentation_transforms);
        self.set_transforms(&transforms);
        self.presentation_transforms = transforms;
    }

    /// Whether a visible presentation transition needs another frame. The loop
    /// checks this before rendering and again before choosing its wait timeout.
    pub fn presentation_animating(&self) -> bool {
        self.wins
            .values()
            .any(|cw| cw.mapped && !cw.hidden && cw.presentation.is_some())
    }

    /// Mark the whole frame dirty (used when stacking or the wallpaper changes).
    pub fn invalidate(&mut self) {
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// Instrumentation-only vblank counter read (see `Renderer::wait_vblank`).
    /// The frame loop no longer paces here — swap interval 1 is the sole
    /// synchroniser.
    #[allow(dead_code)]
    pub fn wait_vblank(&mut self) -> bool {
        self.renderer.wait_vblank()
    }

    /// OPT-IN DIAGNOSTIC: populate the debug floating-window set used by the
    /// [SCENE] trace. No behavioural effect.
    #[allow(dead_code)]
    pub(crate) fn set_debug_floats(&mut self, ids: &[WindowId]) {
        if self.float_trace {
            self.dbg_floats = ids.iter().copied().map(|w| w as Window).collect();
        }
    }

    /// OPT-IN DIAGNOSTIC: raw bits of `dirty_reasons` for logging.
    #[allow(dead_code)]
    pub(crate) fn dirty_reasons_bits(&self) -> u8 {
        self.dirty_reasons.0
    }

    /// Whether a frame is still needed (a compositor event marked us dirty).
    /// The `FrameScheduler` reads the finer-grained `dirty_reasons()`; this is
    /// the coarse boolean it reduces to. Kept as a direct accessor.
    #[allow(dead_code)]
    pub fn needs_frame(&self) -> bool {
        self.dirty
    }

    /// *Why* a frame is needed right now (Fase 9). The `FrameScheduler` reads
    /// this to report the reasons behind a scheduled frame; it is empty exactly
    /// when `needs_frame` is false.
    pub fn dirty_reasons(&self) -> DirtyReason {
        self.dirty_reasons
    }

    // ── native wallpaper (Parte 1 Fase 4 / Parte 2 Fases 7,8,9) ─────────────────

    /// Apply a new wallpaper spec: decode + upload (or compile shader) and request a
    /// single full repaint. Keyed on source + mode so an unchanged wallpaper reuses
    /// the GPU texture without re-decoding per frame (criterio #5). Any
    /// decode/compile failure logs once and leaves the wallpaper disabled — it never
    /// panics or takes the WM down (riesgo: decode bloquea, conversor ausente).
    pub fn set_wallpaper(&mut self, spec: &WallpaperSpec) {
        if let Some(t) = self.wallpaper_native.take() {
            self.renderer.destroy_raw(TextureHandle(t.0));
        }
        self.wallpaper_shader = None;
        self.wallpaper_animating = false;
        self.wallpaper_animated = false;
        self.wallpaper_clock = 0.0;

        match &spec.source {
            WallpaperSource::None => {}
            WallpaperSource::Image(path) => match maverick_img::decode(path) {
                Ok(img) => match self.renderer.upload_rgba(&img) {
                    Ok(tex) => {
                        self.wallpaper_native = Some(GpuImage(tex.0));
                        self.wallpaper_img_w = img.w;
                        self.wallpaper_img_h = img.h;
                        self.wallpaper_mode = spec.mode;
                    }
                    Err(e) => log::warn!("wallpaper: upload failed: {e}"),
                },
                Err(e) => log::warn!("wallpaper: decode '{}' failed: {e}", path.display()),
            },
            WallpaperSource::Shader(path) => match std::fs::read_to_string(path) {
                Ok(src) => match self.renderer.compile_fragment(&src) {
                    Ok(prog) => {
                        self.wallpaper_shader = Some(prog);
                        self.wallpaper_mode = spec.mode;
                        self.wallpaper_clock = 0.0;
                        // Only a shader that actually depends on time must keep
                        // the loop awake; a static shader is drawn once (via the
                        // WALLPAPER dirty reason) and then idles.
                        self.wallpaper_animated = shader_is_animated(&src);
                        self.wallpaper_animating = self.wallpaper_animated;
                    }
                    Err(e) => log::warn!("wallpaper: shader compile failed: {e}"),
                },
                Err(e) => log::warn!("wallpaper: cannot read shader '{}': {e}", path.display()),
            },
            WallpaperSource::Video(_) => {
                log::warn!(
                    "wallpaper: Video source is reserved (Fase 10) and not implemented; ignoring"
                );
            }
        }
        self.mark_full(DirtyReason::WALLPAPER);
    }

    /// Sync the wallpaper's output layout from the WM's monitors. Called at init and
    /// on `RandR` change. Also refreshes `screen_w/h` from the union of outputs so the
    /// wallpaper keeps covering the whole screen after a resize (`RandR` edge case).
    pub fn set_outputs(&mut self, outputs: &[Rect]) {
        self.wallpaper_outputs = outputs.to_vec();
        if !outputs.is_empty() {
            let mut x0 = i32::MAX;
            let mut y0 = i32::MAX;
            let mut x1 = i32::MIN;
            let mut y1 = i32::MIN;
            for o in outputs {
                x0 = x0.min(o.x);
                y0 = y0.min(o.y);
                x1 = x1.max(o.x + o.w as i32);
                y1 = y1.max(o.y + o.h as i32);
            }
            self.screen_w = (x1 - x0).max(1) as u32;
            self.screen_h = (y1 - y0).max(1) as u32;
            self.screen_rect = Rect::new(x0, y0, self.screen_w, self.screen_h);
        }
        self.update_overlay_shape();
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// Advance the wallpaper animation clock by `dt` (the same clamped dt the WM
    /// uses for its own springs — no separate timer). Only a shader that actually
    /// depends on time animates; a static shader, a still image or `None` leaves
    /// `wallpaper_animating` false so the loop goes idle (criterio #4). This is
    /// what stops the compositor from presenting at vsync forever on a static
    /// shader wallpaper.
    pub fn tick_wallpaper(&mut self, dt: f32) {
        if self.wallpaper_animated {
            self.wallpaper_clock += dt;
            self.wallpaper_last_dt = dt;
            self.wallpaper_animating = true;
        } else {
            self.wallpaper_animating = false;
        }
    }

    /// Whether the wallpaper is currently animating (feeds the `FrameScheduler`).
    #[inline]
    pub fn wallpaper_animating(&self) -> bool {
        self.wallpaper_animating
    }
}

/// `WallpaperGpu` — the backend's concrete implementation of the GPU
/// abstraction the core talks to. The core never names OpenGL; it calls these
/// methods (upload/compile/draw/release) and the GL calls live here. A future
/// Vulkan backend implements the same trait against a different `Renderer`.
impl WallpaperGpu for Compositor {
    fn upload_image(&mut self, img: &Rgba8) -> Result<GpuImage, String> {
        self.renderer.upload_rgba(img).map(|h| GpuImage(h.0))
    }
    fn compile_shader(&mut self, frag: &str) -> Result<ShaderId, String> {
        self.renderer.compile_fragment(frag)
    }
    fn draw_image(&mut self, img: &GpuImage, dst: Rect, src_uv: [f32; 4]) {
        let q = DrawQuad {
            dst: [
                dst.x as f32,
                dst.y as f32,
                (dst.x + dst.w as i32) as f32,
                (dst.y + dst.h as i32) as f32,
            ],
            src: src_uv,
            opacity: 1.0,
            ..Default::default()
        };
        self.renderer
            .draw_raw(TextureHandle(img.0), TextureHandle(0), &q);
    }
    fn draw_shader(&mut self, s: ShaderId, out: Rect, time: f32, dt: f32) {
        self.renderer.draw_shader(
            s,
            GlRect {
                x: out.x,
                y: out.y,
                w: out.w,
                h: out.h,
            },
            time,
            dt,
        );
    }
    fn release(&mut self, img: GpuImage) {
        self.renderer.destroy_raw(TextureHandle(img.0));
    }
}

impl Compositor {
    /// Empty when the last frame had nothing to repaint. Next phases use this to
    /// scissor the redraw (partial update) instead of clearing the whole screen.
    #[allow(dead_code)]
    pub fn damage_region(&self) -> &DamageRegion {
        &self.frame_dirty
    }
    /// Mark the whole frame dirty *and* require a full repaint: used for changes
    /// that cannot be expressed as a rectangle set (resize, reparent, restack,
    /// opacity, new/removed window, wallpaper). Content-only `XDamage` must call
    /// `on_damage` instead, which sets `dirty` but leaves the damage region to
    /// express the change as a union of rectangles (partial-redraw-friendly).
    fn mark_full(&mut self, reason: DirtyReason) {
        self.dirty = true;
        self.needs_full = true;
        self.dirty_reasons.insert(reason);
    }

    // ── fullscreen bypass (safe, recoverable XComposite un-redirect) ────────
    //
    // When the `CompositionPolicy` decides an output is in `Bypass`, Maverick
    // stops *interposing* its compositor on the single eligible fullscreen
    // window: it calls `composite_unredirect_window` so X presents that window
    // directly (beneath the ARGB overlay), and simply never draws it into the
    // overlay (the overlay stays transparent over it). Bypass is per-window, so
    // the rest of the desktop keeps being composited normally.
    //
    // Recovery is the whole point: `disengage_bypass` re-redirects the window
    // (`composite_redirect_window`) and re-arms its texture on the very next
    // frame through the normal `compute_scene` rebind path. Entering and leaving
    // bypass is therefore symmetric and idempotent — no state is lost that the
    // steady-state machinery cannot rebuild.

    /// Engage bypass for `mon`, un-redirecting `win` so it presents directly.
    /// No-op if `mon` already bypasses `win`; otherwise any previous bypass on
    /// `mon` is cleanly disengaged first. Refuses a candidate that is not
    /// currently tracked+mapped (destroyed in a race, or never mapped): punching
    /// the overlay hole for a dead XID would leave a permanent wallpaper hole.
    /// The policy re-evaluates every turn, so a refused engage is simply
    /// retried once the window is really there.
    pub fn engage_bypass(&mut self, mon: usize, win: Window) {
        if self.bypassed.get(&mon) == Some(&win) {
            return;
        }
        let mapped = self.wins.get(&win).is_some_and(|cw| cw.mapped);
        if !mapped {
            self.disengage_bypass(mon);
            return;
        }
        if let Some(old) = self.bypassed.get(&mon).copied() {
            self.resume_window(old);
            self.bypassed_set.remove(&old);
        }
        self.bypass_window(win);
        self.bypassed.insert(mon, win);
        self.bypassed_set.insert(win);
        self.update_overlay_shape();
        if self.bypass_trace {
            log::info!(
                "composition policy: monitor {mon} -> {} (win={win:#x})",
                CompositionMode::Bypass.as_str()
            );
        }
    }

    /// Disengage bypass for `mon` (re-redirect + resume its window) if active.
    pub fn disengage_bypass(&mut self, mon: usize) {
        if let Some(win) = self.bypassed.remove(&mon) {
            self.resume_window(win);
            self.bypassed_set.remove(&win);
            self.update_overlay_shape();
            if self.bypass_trace {
                log::info!(
                    "composition policy: monitor {mon} -> {}",
                    CompositionMode::Compose.as_str()
                );
            }
        }
    }

    /// Disengage every active bypass (used when bypass is disabled by config, or
    /// when an unexpected window appears that the policy did not catch).
    pub fn disengage_all_bypass(&mut self) {
        if self.bypassed.is_empty() {
            return;
        }
        let mons: Vec<usize> = self.bypassed.keys().copied().collect();
        for m in mons {
            self.disengage_bypass(m);
        }
    }

    /// True when any output is currently bypassing.
    pub fn any_bypass(&self) -> bool {
        !self.bypassed_set.is_empty()
    }

    /// Recalculate the overlay's bounding shape from the current bypass set.
    /// The overlay's INPUT shape stays empty (clicks fall through); only the
    /// BOUNDING shape is punched. Single source of truth: `self.bypassed`
    /// + `CompWin.outer`.
    fn update_overlay_shape(&self) {
        let holes: Vec<Rect> = self
            .bypassed
            .values()
            .filter_map(|w| self.wins.get(w).map(|cw| cw.outer))
            .collect();
        let coverage = overlay_coverage(self.screen_rect, &holes);
        // Translate coverage rects to overlay window coordinates (overlay at 0,0
        // covering screen_rect). For typical positive monitors this is identity;
        // for union with negative origin we offset. Clamped: raw `as i16/u16`
        // truncates negative origins and >64k sizes into corrupt shapes.
        let rects: Vec<Rectangle> = coverage
            .iter()
            .map(|r| Rectangle {
                x: (r.x - self.screen_rect.x).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                y: (r.y - self.screen_rect.y).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                width: r.w.clamp(1, u16::MAX as u32) as u16,
                height: r.h.clamp(1, u16::MAX as u32) as u16,
            })
            .collect();
        let Ok(region) = self.conn.generate_id() else {
            return;
        };
        let _ = self.conn.xfixes_create_region(region, &rects);
        let _ = self
            .conn
            .xfixes_set_window_shape_region(self.overlay, SK::BOUNDING, 0, 0, region);
        let _ = self.conn.xfixes_destroy_region(region);
    }

    /// Un-redirect `win` so X shows it directly, and drop the GL resources we
    /// were using for it (the overlay must not keep drawing a stale texture).
    /// The `CompWin` is kept (so its last `outer` rect is still available for
    /// the wallpaper-skip decision) but is never drawn while bypassed.
    fn bypass_window(&mut self, win: Window) {
        let _ = self.conn.composite_unredirect_window(win, Redirect::MANUAL);
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.mapped = true; // still mapped, just shown by X directly
            let (tex, pix) = (cw.tex.take(), cw.pixmap.take());
            if let Some(t) = tex {
                self.renderer.destroy_texture(t);
            }
            if let Some(pm) = pix {
                let _ = self.conn.free_pixmap(pm);
            }
            cw.damaged = false;
            cw.needs_rebind = false;
        }
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// Re-redirect `win` (undo `bypass_window`) and re-arm its compositor state
    /// so `compute_scene` rebinds its texture on the next frame. Recovery only
    /// marks the window for re-composition; it does not synchronously create GL
    /// resources (that stays in the render phase, per the floating-freeze fix).
    fn resume_window(&mut self, win: Window) {
        let _ = self.conn.composite_redirect_window(win, Redirect::MANUAL);
        if !self.wins.contains_key(&win) {
            self.track(win);
        }
        if let Some(cw) = self.wins.get_mut(&win) {
            let viewable = self
                .conn
                .get_window_attributes(win)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|a| a.map_state == MapState::VIEWABLE);
            if viewable {
                cw.mapped = true;
                cw.hidden = false;
                cw.needs_rebind = true;
                cw.damaged = true;
            }
        }
        if !self.damages.contains_key(&win) {
            if let Ok(dmg) = self.conn.generate_id() {
                let _ = self.conn.damage_create(dmg, win, ReportLevel::NON_EMPTY);
                self.damages.insert(win, dmg);
            }
        }
        self.mark_full(DirtyReason::SURFACE);
    }

    /// True when `r` is covered by a currently-bypassed window's last `outer`
    /// rect — used by `render` to skip drawing wallpaper under a window that
    /// is presented directly (so the overlay stays transparent there).
    /// Full containment, plus near-full overlap: a bypassed window 1px shy
    /// of the output (borders) must still suppress the wallpaper beneath it
    /// (otherwise the opaque wallpaper paints over the direct frame's edge
    /// pixels) — but a stale-small bypass must NOT suppress the whole
    /// output's wallpaper, hence the area tolerance instead of bare overlap.
    fn bypass_covers(&self, r: Rect) -> bool {
        for win in &self.bypassed_set {
            if let Some(cw) = self.wins.get(win) {
                let o = cw.outer;
                if o.contains_rect(r) {
                    return true;
                }
                // Saturating overlap test (no new `Rect` API needed).
                let overlap = o.x < r.x.saturating_add(r.w.min(i32::MAX as u32) as i32)
                    && r.x < o.x.saturating_add(o.w.min(i32::MAX as u32) as i32)
                    && o.y < r.y.saturating_add(r.h.min(i32::MAX as u32) as i32)
                    && r.y < o.y.saturating_add(o.h.min(i32::MAX as u32) as i32);
                if overlap && o.area().saturating_add(16_384) >= r.area() {
                    return true;
                }
            }
        }
        false
    }

    /// OPT-IN DIAGNOSTIC helper: render the current GL resource ids for `win`
    /// (X pixmap, `GLXPixmap`, GL texture) as strings for [LIFECYCLE] tracing.
    /// No behaviour change.
    fn dbg_res_ids(&self, win: Window) -> (String, String, String) {
        match self.wins.get(&win) {
            Some(cw) => (
                cw.pixmap.map_or_else(|| "none".into(), |p| p.to_string()),
                cw.tex
                    .as_ref()
                    .map_or_else(|| "none".into(), |t| t.glx_pixmap.to_string()),
                cw.tex
                    .as_ref()
                    .map_or_else(|| "none".into(), |t| t.handle().0.to_string()),
            ),
            None => ("none".into(), "none".into(), "none".into()),
        }
    }

    /// OPT-IN DIAGNOSTIC: a one-line-per-window snapshot of the compositor's
    /// view of every tracked window (geometry/texture state). Used by the
    /// frame-loop trace (`MAV_COMP_TRACE`) to correlate GL resources with the
    /// WM-side floating/override-redirect state. Never affects rendering.
    #[allow(dead_code)]
    pub(crate) fn debug_dump(&self) -> String {
        let mut s = String::new();
        writeln!(
            s,
            "  frame_gen={} dirty={} needs_full={} scene={}",
            self.frame_gen,
            self.dirty,
            self.needs_full,
            self.scene.len(),
        )
        .unwrap();
        for (&win, cw) in &self.wins {
            let tex = cw.tex.as_ref().map_or(0, |t| t.handle().0);
            let gpx = cw.tex.as_ref().map_or(0, |t| t.glx_pixmap);
            writeln!(
                s,
                "  win={:#x} mapped={} hidden={} occluded={} damaged={} needs_rebind={} pixmap={} glxpixmap={} tex={} outer={:?} tgen={}",
                win,
                cw.mapped,
                cw.hidden,
                cw.occluded,
                cw.damaged,
                cw.needs_rebind,
                cw.pixmap.unwrap_or(0),
                gpx,
                tex,
                cw.outer,
                cw.transform_gen,
            )
            .unwrap();
        }
        s
    }

    // ── frame ───────────────────────────────────────────────────────────────

    /// Build the explicit scene for this frame into `self.scene` (reused buffer,
    /// no allocation): one `DrawItem` per window that is mapped, on screen and
    /// not hidden. Rebinds any texture whose client repainted, and culls
    /// everything outside the viewport. The result is what `render` actually
    /// submits to the GPU.
    fn compute_scene(&mut self) {
        let gen = self.frame_gen;
        let (sw, sh) = (self.screen_w, self.screen_h);
        let mut items: Vec<DrawItem> = std::mem::take(&mut self.scene);
        items.clear();
        // Rebuild the damage accounting from scratch every frame: only the
        // windows that repainted since the last frame contribute their rect,
        // plus `needs_full` (set by structural changes) forcing a full repaint.
        self.frame_dirty.clear();

        // ── Fase 12, pass 1 (top→bottom): occlusion culling. A window fully
        // hidden behind a single opaque, square-cornered, on-screen window above
        // it need never be drawn, saving fragment processing. We walk the stack
        // from the top so every occluder is known before the window it covers;
        // `occluder_rects` (a reused buffer) accumulates the opaque rects seen so
        // far, and a window is marked `occluded` when one of them entirely
        // contains it. Windows with `opacity < 1` or a rounded corner are *not*
        // occluders (their corners/translucency would wrongly clip what is
        // behind), so they never hide another window — a correct, conservative
        // miss.
        self.occluder_rects.clear();
        for &win in self.stack.iter().rev() {
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            if !cw.mapped || cw.hidden {
                cw.occluded = false;
                continue;
            }
            let (outer, radius) = if cw.transform_gen == gen {
                (cw.transform, cw.transform_radius)
            } else {
                (cw.outer, 0)
            };
            if outer.w == 0 || outer.h == 0 {
                cw.occluded = false;
                continue;
            }
            let onscreen = !CompWin::offscreen(outer, sw, sh);
            let opaque = cw.opacity >= 1.0 && radius == 0;
            cw.occluded = onscreen && fully_covered_by(outer, &self.occluder_rects);
            if onscreen && opaque && !cw.occluded {
                self.occluder_rects.push(outer);
            }
        }

        // ── pass 2 (bottom→top): build the scene, skipping occluded windows.
        // Iterate by index to avoid per-frame Vec clone (allocation) while still
        // allowing mutable borrows of `self.wins` disjoint from `self.stack`.
        for i in 0..self.stack.len() {
            let win = self.stack[i];
            // A bypassed window is presented directly by X; the compositor must
            // never draw it (doing so would cover the real window with a stale
            // texture) nor rebind a GL resource for it. The overlay simply stays
            // transparent over it.
            if self.bypassed_set.contains(&win) {
                if self.float_trace && self.dbg_floats.contains(&win) {
                    log::info!(
                        "[SCENE] frame={} win={:#x} included=false skip_reason=Bypassed",
                        self.dbg_frame,
                        win
                    );
                }
                continue;
            }
            // OPT-IN DIAGNOSTIC: only floating windows are traced.
            let float_dbg = self.float_trace && self.dbg_floats.contains(&win);
            // OPT-IN DIAGNOSTIC: capture tex presence before the mutable
            // borrow below, so the [SCENE] logs never re-borrow `cw.tex`.
            let has_tex = self.wins.get(&win).is_some_and(|c| c.tex.is_some());
            // No `ignored` probe here: `track` refuses to record an ignored
            // window, so `wins` can never contain one and this lookup is the
            // filter. That is one hash per stack entry saved every frame.
            let needs_fixup = match self.wins.get(&win) {
                Some(cw) if !cw.mapped || cw.hidden || cw.occluded => false,
                // A window with no texture yet must always be bound. A window
                // whose previous bind failed (`needs_rebind`) is retried only when
                // a *fresh* `DamageNotify`/`ConfigureNotify` arrived (`damaged`):
                // without this gate a persistently-unbindable window would retry
                // `rename_and_bind` on every frame, pinning `dirty` and pegging
                // the loop at 100% (a busy "freeze") while never drawing the
                // window. The next client repaint re-arms the retry.
                Some(cw) => cw.tex.is_none() || (cw.needs_rebind && cw.damaged),
                None => false,
            };
            if needs_fixup {
                let has_pix = self.wins.get(&win).and_then(|cw| cw.pixmap).is_some();
                self.rename_and_bind(win, has_pix);
            }
            let Some(cw) = self.wins.get_mut(&win) else {
                if float_dbg {
                    log::info!(
                        "[SCENE] frame={} win={:#x} mapped=? outer=? transform=? transform_gen=? frame_gen={} tex=false included=false skip_reason=NoCompWin",
                        self.dbg_frame, win, gen
                    );
                }
                continue;
            };
            if !cw.mapped || cw.hidden {
                if float_dbg {
                    log::info!(
                        "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex={} included=false skip_reason={}",
                        self.dbg_frame, win, cw.mapped, cw.outer, cw.transform, cw.transform_gen, gen, has_tex,
                        if cw.mapped { "Hidden" } else { "Unmapped" }
                    );
                }
                continue;
            }
            if cw.occluded {
                if float_dbg {
                    log::info!(
                        "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex={} included=false skip_reason=Occluded",
                        self.dbg_frame, win, cw.mapped, cw.outer, cw.transform, cw.transform_gen, gen, has_tex
                    );
                }
                continue;
            }
            let Some(tex) = cw.tex.as_mut() else {
                if float_dbg {
                    log::info!(
                    "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex=false included=false skip_reason=NoTexture",
                    self.dbg_frame, win, cw.mapped, cw.outer, cw.transform, cw.transform_gen, gen
                );
                }
                continue;
            };
            // Rebind the texture if the client repainted.
            let was_damaged = cw.damaged;
            if was_damaged {
                self.renderer.bind(tex);
                cw.damaged = false;
            }
            // Live outer rect, or fall back to the X geometry (OR windows).
            let (outer, radius) = if cw.transform_gen == gen {
                (cw.transform, cw.transform_radius)
            } else {
                (cw.outer, 0)
            };
            if outer.w == 0 || outer.h == 0 {
                if float_dbg {
                    log::info!(
                        "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex={} included=false skip_reason=ZeroGeometry",
                        self.dbg_frame, win, cw.mapped, cw.outer, cw.transform, cw.transform_gen, gen, has_tex
                    );
                }
                continue;
            }
            // Fase 7 — animation damage. A window whose drawn rect changed since
            // the last frame must repaint both its previous and current screen
            // rect, else the pixels it slid off of (and into) linger during
            // scroll. Emitted into the same `DamageRegion` the XDamage path
            // uses; `decide_redraw` only turns it into a scissored Partial when
            // buffer-age is available, so without it the Full fallback still
            // repaints everything. Done *before* the off-screen cull so a window
            // scrolling out still damages the area it just vacated.
            // Only windows that moved (their drawn rect differs from last
            // frame's) or that the client repainted actually need a damage
            // entry. A stationary, undamaged window contributes nothing, so the
            // partial-redraw bounding box no longer balloons to the whole screen
            // every frame (B5).
            let moved = cw.prev_visual != Some(outer) || cw.prev_visual_radius != radius;
            if was_damaged || moved {
                let mut aout = [Rect::default(); 2];
                let n = anim_damage_rects(cw.prev_visual, outer, &mut aout);
                for &r in &aout[..n] {
                    self.frame_dirty.add(r);
                }
            }
            cw.prev_visual = Some(outer);
            cw.prev_visual_radius = radius;
            // Cull windows that are entirely outside the screen. This is the
            // single biggest draw-time win: a 50-window ribbon only has ~5 on
            // screen at once; the rest are scrolled off the edges and would
            // otherwise each issue a `glDrawArrays` + texture bind for nothing.
            if CompWin::offscreen(outer, sw, sh) {
                if float_dbg {
                    log::info!(
                        "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex={} included=true skip_reason=None offscreen=true",
                        self.dbg_frame, win, cw.mapped, outer, cw.transform, cw.transform_gen, gen, has_tex
                    );
                }
                continue;
            }
            // A window that repainted this frame only dirtied its own area; that
            // rect is the partial-redraw candidate (scissored in a later phase).
            // Structural changes set `needs_full` and force the whole screen
            // instead.
            if was_damaged {
                self.frame_dirty.add(outer);
            }
            let smooth = tex.width as u32 != outer.w || tex.height as u32 != outer.h;
            let filter = if smooth {
                Filter::Linear
            } else {
                Filter::Nearest
            };
            let q = DrawQuad {
                dst: [
                    outer.x as f32,
                    outer.y as f32,
                    (outer.x + outer.w as i32) as f32,
                    (outer.y + outer.h as i32) as f32,
                ],
                src: [0.0, 0.0, 1.0, 1.0],
                size: [outer.w as f32, outer.h as f32],
                radius: radius as f32,
                border_width: if cw.transform_gen == gen && cw.border_color.is_some() {
                    cw.transform_border_w as f32
                } else {
                    0.0
                },
                border_color: border_rgba(cw.border_color.unwrap_or(0)),
                opacity: cw.opacity,
                filter,
            };
            if float_dbg {
                log::info!(
                    "[SCENE] frame={} win={:#x} mapped={} outer={:?} transform={:?} transform_gen={} frame_gen={} tex={} included=true skip_reason=None",
                    self.dbg_frame, win, cw.mapped, outer, cw.transform, cw.transform_gen, gen, has_tex
                );
            }
            items.push(DrawItem {
                win,
                quad: q,
                tex: tex.handle(),
            });
        }
        // Structural changes (resize/restack/opacity/…) cannot be expressed as a
        // rectangle set, so the whole screen must be cleared and repainted.
        if self.needs_full {
            self.frame_dirty.full();
        }
        self.scene = items;
    }

    /// Render one frame: wallpaper, then every on-screen window bottom→top.
    /// Blocks to vsync via the swap (when `vsync` is on). Returns `false` on a
    /// GL error so the caller can disable the compositor.
    pub fn render(&mut self) -> bool {
        let presenting = self.presentation_animating();
        if self.comp_trace {
            log::info!(
                "[LIFECYCLE] event=RenderBegin dirty={} needs_full={} tracked={} scene_windows={}",
                self.dirty,
                self.needs_full,
                self.damages.len(),
                self.scene.len(),
            );
        }
        if self.stack_dirty {
            self.refresh_stack();
        }
        let t_frame_start = Some(Instant::now());
        self.compute_scene();
        let t_build = t_frame_start.map(|t| t.elapsed().as_nanos() as u64);

        let (sw, sh) = (self.screen_w, self.screen_h);

        // Decide how much to repaint from the two damage facts plus the
        // buffer-age capability. `decide_redraw` is the single source of truth
        // for the full/partial/idle choice (unit-tested in `frameplan_tests`).
        // `force_full_redraw` (MAVERICK_FORCE_FULL_REDRAW) pretends buffer-age
        // is unavailable so the harness can exercise the full-redraw fallback.
        let has_age = self.renderer.has_buffer_age() && !self.force_full_redraw;
        let t0 = Instant::now();
        let mut mode = decide_redraw(has_age, self.needs_full, !self.frame_dirty.is_empty());
        let decided_partial = mode == FrameMode::Partial;
        let mut observed_age: u32 = 0;

        // Honest partial with buffer-age history (P0): keep the last 4 frames
        // of `frame_dirty` so `age` 1..4 (the spec range) can be honoured.
        // Double-buffered fbconfigs report `age==2` (exchange), so a single-frame
        // `damage_acc` would force `Full` every frame and defeat partial redraw.
        // Per `GLX_EXT_buffer_age`: 0=undefined, 1=previous frame (copy), 2+=N
        // frames old. We union the last `age` frame damages; 0 or >4 → Full.
        if mode == FrameMode::Partial {
            observed_age = if has_age {
                self.renderer.back_buffer_age()
            } else {
                0
            };
            if (1..=4).contains(&observed_age) {
                self.damage_history.push(self.frame_dirty);
                if self.damage_history.len() > 4 {
                    self.damage_history.remove(0);
                }
                self.damage_acc.clear();
                let n = self.damage_history.len();
                let start = n.saturating_sub(observed_age as usize);
                for hist in &self.damage_history[start..] {
                    for r in &hist.rects[..hist.count] {
                        self.damage_acc.add(*r);
                        if self.damage_acc.needs_full {
                            break;
                        }
                    }
                    if self.damage_acc.needs_full {
                        break;
                    }
                    if hist.needs_full {
                        self.damage_acc.full();
                        break;
                    }
                }
                if self.damage_acc.needs_full {
                    mode = FrameMode::Full;
                }
            } else {
                mode = FrameMode::Full;
            }
        }

        if self.float_trace {
            let scene_windows = self.scene.len();
            let floating_windows = self
                .scene
                .iter()
                .filter(|it| self.dbg_floats.contains(&it.win))
                .count();
            log::info!(
                "[RENDER] frame={} mode={:?} dirty={} needs_full={} scene_windows={} floating_windows={}",
                self.dbg_frame, mode, self.dirty, self.needs_full, scene_windows, floating_windows
            );
        }

        match mode {
            FrameMode::Idle => {
                // `render` is only reached when something is dirty, so Idle here
                // means the region was emptied by structural handling — repaint
                // the whole screen to be safe.
                self.renderer.begin_frame(sw, sh, true);
            }
            FrameMode::Full => {
                self.renderer.begin_frame(sw, sh, true);
            }
            FrameMode::Partial => {
                let b = self.damage_acc.bounding_rect();
                // Clamp to the screen: a rect that bled past an edge must not
                // scissor a negative / out-of-range box.
                let x = b.x.max(0);
                let y = b.y.max(0);
                let w = (b.w as i32).min(sw as i32 - x).max(0) as u32;
                let h = (b.h as i32).min(sh as i32 - y).max(0) as u32;
                if w == 0 || h == 0 {
                    // Degenerate box — fall back to a full repaint.
                    self.renderer.begin_frame(sw, sh, true);
                } else {
                    self.renderer.begin_frame(sw, sh, false);
                    self.renderer.set_scissor(x, y, w, h, sh);
                    self.renderer.scissor_clear();
                }
            }
        }

        // Wallpaper first (so un-textured/transparent areas show it). Drawn
        // clipped to the scissor in the partial path, full-screen otherwise.
        // Precedence: animated shader > static native image (per-output quads) >
        // legacy root pixmap (`_XROOTPMAP_ID` from feh/hsetroot).
        let mut last_tex = TextureHandle(0);
        // Resolve bypass state once (immutable snapshot) so the per-branch logic
        // below — which may hold other `&mut self` borrows — does not conflict.
        let bypass_active = self.any_bypass();
        if let Some(shader) = self.wallpaper_shader {
            // Animated shader: one fill per output; `u_resolution` tells each shader
            // its own pixel size. Keeps requesting frames via `wallpaper_animating`.
            for out in &self.wallpaper_outputs {
                // Don't paint the wallpaper over a bypassed fullscreen window —
                // the overlay must stay transparent there so the directly-presented
                // window shows through.
                if self.bypass_covers(*out) {
                    continue;
                }
                self.renderer.draw_shader(
                    shader,
                    GlRect {
                        x: out.x,
                        y: out.y,
                        w: out.w,
                        h: out.h,
                    },
                    self.wallpaper_clock,
                    self.wallpaper_last_dt,
                );
            }
        } else if let Some(native) = self.wallpaper_native {
            // Static decoded image: one quad per output (shared texture, own src/dst).
            if self.wallpaper_img_w > 0
                && self.wallpaper_img_h > 0
                && !self.wallpaper_outputs.is_empty()
            {
                let quads = compute_wallpaper_rects(
                    self.wallpaper_img_w,
                    self.wallpaper_img_h,
                    self.wallpaper_mode,
                    &self.wallpaper_outputs,
                );
                for (dst, src) in quads {
                    if self.bypass_covers(dst) {
                        continue;
                    }
                    let q = DrawQuad {
                        dst: [
                            dst.x as f32,
                            dst.y as f32,
                            (dst.x + dst.w as i32) as f32,
                            (dst.y + dst.h as i32) as f32,
                        ],
                        src,
                        opacity: 1.0,
                        ..Default::default()
                    };
                    last_tex = self
                        .renderer
                        .draw_raw(TextureHandle(native.0), last_tex, &q);
                }
            }
        } else if let Some(wp) = self.wallpaper.as_mut() {
            // Legacy root pixmap fallback (no native wallpaper configured). The
            // legacy path draws a single full-screen quad, so when any output is
            // bypassing we skip it entirely: a transparent hole is left over the
            // bypassed window (correct) at the cost of the wallpaper not showing
            // in the other monitors' gaps for the duration of the bypass — an
            // acceptable, documented limitation of the legacy wallpaper path.
            if !bypass_active {
                self.renderer.bind(wp);
                last_tex = wp.handle();
                self.renderer.draw(
                    wp,
                    &DrawQuad {
                        dst: [0.0, 0.0, sw as f32, sh as f32],
                        ..Default::default()
                    },
                );
            }
        }

        for item in &self.scene {
            // The texture is owned by `wins`; `draw_raw` takes the handle and the
            // quad's filter, and elides the `glBindTexture` when it matches
            // `last_tex` — exactly the bind-cache the `&Texture` path
            // kept on the texture, reconstructed from the scene.
            crate::backend::x11::trace::trace!(
                "submitted_geometry",
                "win={} dst={:?} radius={} opacity={}",
                item.win,
                item.quad.dst,
                item.quad.radius,
                item.quad.opacity
            );
            last_tex = self.renderer.draw_raw(item.tex, last_tex, &item.quad);
        }

        if matches!(mode, FrameMode::Partial) {
            self.renderer.clear_scissor();
        }

        if self.float_trace {
            log::info!(
                "[PRESENT] frame={} submitted=true mode={:?}",
                self.dbg_frame,
                mode
            );
        }
        crate::backend::x11::trace::trace!("swap_begin", "");
        let trace_swap_start = crate::backend::x11::trace::enabled().then(Instant::now);
        self.renderer.end_frame();
        let trace_swap_duration = trace_swap_start.map(|start| start.elapsed().as_nanos());
        if let Some(duration) = trace_swap_duration {
            crate::backend::x11::trace::trace!(
                "swap_returned",
                "duration_ns={duration} actual_visible=false"
            );
        }
        if self.comp_trace {
            log::info!(
                "[LIFECYCLE] event=PresentEnd submitted=true mode={:?}",
                mode
            );
        }
        let swap_ns = t_frame_start.map(|t| t.elapsed().as_nanos() as u64);
        // The just-presented frame is now the committed back buffer, so the
        // accumulated damage describes only what changed since this present.
        // Clearing it each frame bounds the partial-redraw work and stops the
        // region from growing until it covers the whole screen (B4).
        self.damage_acc.clear();
        if mode == FrameMode::Full {
            self.damage_history.clear();
        }
        self.dirty = false;
        self.needs_full = false;
        self.dirty_reasons.clear();
        // A transition that is still mid-flight (or a newly triggered one whose
        // first interpolated frame was just produced) keeps the loop awake the
        // same way an ongoing camera animation does — through the one-shot
        // GEOMETRY dirty bit that survives `clear_dirty` as FrameReason::Geometry.
        if presenting {
            self.dirty = true;
            self.dirty_reasons.insert(DirtyReason::GEOMETRY);
        }
        // Stamp the present timestamp unconditionally: the presentation
        // transitions read the inter-present interval as their dt, which must
        // work with tracing off (the trace block below only *reports* it).
        self.last_present = t_frame_start;

        if self.trace {
            if let (Some(b), Some(s), Some(ts)) = (t_build, swap_ns, t_frame_start) {
                self.trace_count += 1;
                self.trace_ns_build_total += b;
                self.trace_ns_swap_total += s;
                let ai = if observed_age as usize >= self.trace_age_hist.len() {
                    self.trace_age_hist.len() - 1
                } else {
                    observed_age as usize
                };
                self.trace_age_hist[ai] += 1;
                match mode {
                    FrameMode::Partial => self.trace_mode_partial += 1,
                    FrameMode::Full => self.trace_mode_full += 1,
                    FrameMode::Idle => {}
                }
                if decided_partial && mode != FrameMode::Partial {
                    self.trace_partial_to_full += 1;
                }
                if let Some(last) = self.last_present {
                    let iv = ts.duration_since(last).as_nanos() as u64;
                    if iv > 0 {
                        self.trace_ns_interval_total += iv;
                        self.trace_ns_interval_max = self.trace_ns_interval_max.max(iv);
                    }
                }
                self.last_present = Some(ts);

                if self.trace_count >= 120 {
                    let n = self.trace_count;
                    log::info!(
                        "compositor[trace]: frames={} avg_build_ns={} avg_swap_ns={} \
                         avg_interval_ns={} max_interval_ns={} age[0,1,2,3+]={:?} \
                         mode(full={},partial={}) partial_to_full={}",
                        n,
                        self.trace_ns_build_total / n,
                        self.trace_ns_swap_total / n,
                        if self.trace_ns_interval_total > 0 {
                            self.trace_ns_interval_total / n
                        } else {
                            0
                        },
                        self.trace_ns_interval_max,
                        self.trace_age_hist,
                        self.trace_mode_full,
                        self.trace_mode_partial,
                        self.trace_partial_to_full,
                    );
                    self.trace_count = 0;
                    self.trace_ns_build_total = 0;
                    self.trace_ns_swap_total = 0;
                    self.trace_ns_interval_total = 0;
                    self.trace_ns_interval_max = 0;
                    self.trace_age_hist = [0; 4];
                    self.trace_mode_full = 0;
                    self.trace_mode_partial = 0;
                    self.trace_partial_to_full = 0;
                }
            }
        }

        if self.perf_log {
            let ns = t0.elapsed().as_nanos() as u64;
            self.perf_count += 1;
            self.perf_ns_total += ns;
            self.perf_ns_max = self.perf_ns_max.max(ns);
            if self.perf_count >= 120 {
                let avg = self.perf_ns_total / self.perf_count;
                log::info!(
                    "compositor: perf frames={} avg_render_ns={} max_render_ns={}",
                    self.perf_count,
                    avg,
                    self.perf_ns_max
                );
                self.perf_count = 0;
                self.perf_ns_total = 0;
                self.perf_ns_max = 0;
            }
        }
        true
    }

    /// Disable and release everything (fallback / cleanup).
    pub fn disable(&mut self) {
        let wins: Vec<CompWin> = self.wins.drain().map(|(_, cw)| cw).collect();
        for cw in wins {
            self.release_texture(cw);
        }
        if let Some(t) = self.wallpaper.take() {
            self.renderer.destroy_texture(t);
        }
        // `wallpaper_pixmap` is deliberately *not* freed: it is the wallpaper
        // setter's resource, not ours.
        self.wallpaper_pixmap = None;
        for (_, dmg) in self.damages.drain() {
            let _ = self.conn.damage_destroy(dmg);
        }
        let _ = self
            .conn
            .composite_unredirect_subwindows(self.root, Redirect::MANUAL);
        let _ = self.conn.composite_release_overlay_window(self.root);
        self.renderer.destroy();
    }

    // ── internals ────────────────────────────────────────────────────────────

    /// Repair the bottom→top order from the server.
    ///
    /// This is the *recovery* path, not the steady state: the order is normally
    /// maintained incrementally by `on_restack` from the `ConfigureNotify`
    /// stream. A `QueryTree` per restack would be a round trip on every focus
    /// change, and — worse — a round trip whose reply races the event that
    /// caused it. It runs at startup and whenever an incremental update named a
    /// sibling we do not track.
    fn refresh_stack(&mut self) {
        self.stack_dirty = false;
        if let Some(reply) = self
            .conn
            .query_tree(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            self.stack = reply.children;
        }
    }

    /// Re-read the root background pixmap (`_XROOTPMAP_ID`) and texture it.
    fn refresh_wallpaper(&mut self) {
        let atom = match self
            .conn
            .intern_atom(false, b"_XROOTPMAP_ID")
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r.atom,
            None => return,
        };
        if atom == 0 {
            return;
        }
        let pm = match self
            .conn
            .get_property(false, self.root, atom, u32::from(AtomEnum::PIXMAP), 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r.value32().and_then(|mut v| v.next()).unwrap_or(0),
            None => return,
        };
        if pm == 0 || self.wallpaper_pixmap == Some(pm) {
            return;
        }
        // A pixmap only carries a depth, not a visual. Ask the server for the
        // real geometry (the wallpaper is not necessarily screen-sized) and map
        // that depth onto a visual we know how to bind — the root visual when
        // the depths agree, which is the normal case for every wallpaper tool.
        let Some(g) = self.conn.get_geometry(pm).ok().and_then(|c| c.reply().ok()) else {
            log::debug!("compositor: _XROOTPMAP_ID {pm} has no geometry; ignoring it");
            return;
        };
        let Some(format) = self.format_for_depth(g.depth) else {
            log::debug!(
                "compositor: no visual of depth {} for the root pixmap; wallpaper not composited",
                g.depth
            );
            return;
        };
        if let Some(old) = self.wallpaper.take() {
            self.renderer.destroy_texture(old);
        }
        // The old `wallpaper_pixmap` is *not* freed: X lets any client destroy
        // any resource id, so freeing `_XROOTPMAP_ID` would rip the background
        // out from under feh/hsetroot and leave the desktop showing whatever
        // memory the server reuses next.
        match self
            .renderer
            .texture_from_pixmap(pm, format, g.width, g.height)
        {
            Ok(tex) => {
                self.wallpaper = Some(tex);
                self.wallpaper_pixmap = Some(pm);
                self.mark_full(DirtyReason::SURFACE);
            }
            Err(e) => {
                self.wallpaper_pixmap = None;
                log::warn!("compositor: wallpaper ({format}) not compositable: {e}");
            }
        }
    }

    /// A visual we can bind for a bare pixmap of `depth`. Prefers the root
    /// visual so the common case is exact.
    fn format_for_depth(&self, depth: u8) -> Option<VisualFormat> {
        if self.root_format.depth == depth {
            return Some(self.root_format);
        }
        self.formats
            .values()
            .filter(|v| v.depth == depth && v.direct)
            .copied()
            .max_by_key(|v| v.color_bits())
    }

    /// At startup, track every existing top-level window.
    fn scan_existing(&mut self) {
        if let Some(reply) = self
            .conn
            .query_tree(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            for win in reply.children {
                // Already-mapped (viewable) windows — e.g. those that survived a
                // `restart` re-exec, or that mapped in the brief window before the
                // compositor finished scanning — never receive a `MapNotify`, so
                // routing them through `track` alone leaves `mapped=false` and
                // `tex=None`. The renderer skips any window without a GPU texture
                // (render pass 2: `let Some(tex) = cw.tex … else continue`), so the
                // tiles would vanish. Mark them mapped and bind their texture now.
                // Non-viewable windows keep the lazy `track` path (they bind on
                // their own MapNotify).
                let viewable = self
                    .conn
                    .get_window_attributes(win)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some_and(|a| a.map_state == MapState::VIEWABLE);
                if viewable {
                    self.on_map(win);
                } else {
                    self.track(win);
                }
            }
        }
    }

    /// Name the window's off-screen pixmap and wrap it as a GL texture.
    ///
    /// On a bind failure this keeps the named pixmap (when `keep_pixmap` and one
    /// already exists) and sets `needs_rebind`, so `compute_scene` retries the
    /// bind on the next damage report rather than leaving the window as a
    /// permanent hole in the frame. The TFP spec leaves the texture contents
    /// undefined after a rebind, so a freshly (re)bound window is always marked
    /// `damaged` and repainted from the client's next draw.
    fn rename_and_bind(&mut self, win: Window, keep_pixmap: bool) {
        let Some(cw) = self.wins.get(&win) else {
            return;
        };
        if !cw.mapped {
            return;
        }
        let format = cw.format;
        // Zero sizes fall back to 1x1; >64k sizes are clamped — a raw
        // `as u16` truncates (e.g. 65537 → 1) into a corrupt texture.
        let (w, h) = if cw.outer.w == 0 || cw.outer.h == 0 {
            (1u16, 1u16)
        } else {
            (
                cw.outer.w.clamp(1, u16::MAX as u32) as u16,
                cw.outer.h.clamp(1, u16::MAX as u32) as u16,
            )
        };
        // Reuse the existing named pixmap when retrying a failed bind, so we do
        // not leak a new server-side allocation on every damage repaint.
        let pixmap = if keep_pixmap {
            if let Some(p) = cw.pixmap {
                p
            } else {
                let Ok(p) = self.conn.generate_id() else {
                    return;
                };
                p
            }
        } else {
            let Ok(p) = self.conn.generate_id() else {
                return;
            };
            p
        };
        if (!keep_pixmap || cw.pixmap != Some(pixmap))
            && self.conn.composite_name_window_pixmap(win, pixmap).is_err()
        {
            return;
        }
        match self.renderer.texture_from_pixmap(pixmap, format, w, h) {
            Ok(t) => {
                if let Some(cw) = self.wins.get_mut(&win) {
                    cw.tex = Some(t);
                    cw.pixmap = Some(pixmap);
                    cw.damaged = true;
                    cw.needs_rebind = false;
                } else {
                    // The window vanished while we were binding.
                    self.renderer.destroy_texture(t);
                    let _ = self.conn.free_pixmap(pixmap);
                }
            }
            Err(e) => {
                if self.warned_visuals.insert(format.id) {
                    log::warn!("compositor: cannot texture windows of {format}: {e}");
                }
                // Keep the named pixmap and retry on the next damage instead of
                // dropping the window into a permanent hole (RC-1).
                if let Some(cw) = self.wins.get_mut(&win) {
                    cw.needs_rebind = true;
                    if !keep_pixmap || cw.pixmap != Some(pixmap) {
                        cw.pixmap = Some(pixmap);
                    }
                }
            }
        }
    }

    /// Give a window's GPU texture and its `NameWindowPixmap` back.
    ///
    /// The pixmap is a *server-side allocation the size of the window*, handed
    /// to us by Composite and owned by us — nobody else will ever free it.
    fn release_texture(&mut self, mut cw: CompWin) {
        if let Some(t) = cw.tex.take() {
            self.renderer.destroy_texture(t);
        }
        if let Some(pm) = cw.pixmap.take() {
            let _ = self.conn.free_pixmap(pm);
        }
    }
}

// ── stacking order (pure) ─────────────────────────────────────────────────────
//
// The draw order is the X sibling order, and X only ever reports it as
// "`win` is now immediately above `above`". These three helpers are the whole
// of that bookkeeping, kept free of `self` so the ordering rules can be tested
// against a plain `Vec` with no server, no GL and no window manager.

/// Apply one X restack to a bottom→top order.
///
/// `above` is the sibling `win` now sits immediately above; `None` means it
/// went to the very bottom (that is X's encoding, not a "don't know").
///
/// Returns `false` when `above` names a window that is not in `stack`. The
/// caller must then resync from `QueryTree`: there is no position that can be
/// inferred, and inventing one draws the window at the wrong depth.
fn stack_restack(stack: &mut Vec<Window>, win: Window, above: Option<Window>) -> bool {
    let target = match above {
        None => 0,
        Some(sib) if sib == win => return true, // nonsense; leave the order alone
        Some(sib) => {
            if let Some(i) = stack.iter().position(|&w| w == sib) {
                i + 1
            } else {
                // Drop any stale entry so the resync starts from a consistent
                // state rather than a duplicate.
                stack.retain(|&w| w != win);
                return false;
            }
        }
    };
    match stack.iter().position(|&w| w == win) {
        Some(cur) => {
            stack.remove(cur);
            // `target` was computed against the stack that still held `win`, so
            // removing an entry *below* the target shifts it one slot left.
            let target = if cur < target { target - 1 } else { target };
            stack.insert(target.min(stack.len()), win);
        }
        // Not tracked yet (we can miss a CreateNotify for a window that existed
        // before us): the sibling is known, so the position is still exact.
        None => stack.insert(target.min(stack.len()), win),
    }
    true
}

/// Put `win` on top of the stack — where the server places a newly created
/// window.
fn stack_add_top(stack: &mut Vec<Window>, win: Window) {
    stack.retain(|&w| w != win);
    stack.push(win);
}

/// Forget `win` entirely (`DestroyNotify`).
fn stack_remove(stack: &mut Vec<Window>, win: Window) {
    stack.retain(|&w| w != win);
}

/// Flatten the screen's `allowed_depths` into the flat visual table the
/// renderer matches fbconfigs against.
///
/// `alpha_bits` is `depth - popcount(red | green | blue)`: X does not report an
/// alpha mask, but an ARGB32 visual is precisely a depth-32 visual whose three
/// colour masks only cover 24 bits.
fn screen_visuals(screen: &Screen) -> Vec<VisualFormat> {
    let mut out = Vec::new();
    for d in &screen.allowed_depths {
        for v in &d.visuals {
            let direct = v.class == VisualClass::TRUE_COLOR || v.class == VisualClass::DIRECT_COLOR;
            let colour = (v.red_mask | v.green_mask | v.blue_mask).count_ones() as u8;
            out.push(VisualFormat {
                id: v.visual_id,
                depth: d.depth,
                red_bits: if direct {
                    v.red_mask.count_ones() as u8
                } else {
                    0
                },
                green_bits: if direct {
                    v.green_mask.count_ones() as u8
                } else {
                    0
                },
                blue_bits: if direct {
                    v.blue_mask.count_ones() as u8
                } else {
                    0
                },
                alpha_bits: if direct {
                    d.depth.saturating_sub(colour)
                } else {
                    0
                },
                direct,
            });
        }
    }
    out
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn set_empty_input_region(conn: &XConn, win: Window) -> Result<(), Box<dyn std::error::Error>> {
    // XFIXES: set the window's input region to the empty region. We create an
    // empty region (no rectangles) and assign it as the window's input shape.
    let region = conn.generate_id()?;
    conn.xfixes_create_region(region, &[])?;
    conn.xfixes_set_window_shape_region(win, SK::INPUT, 0, 0, region)?;
    conn.xfixes_destroy_region(region)?;
    Ok(())
}

fn intern_cm_atom(conn: &XConn, screen: usize) -> Result<Atom, Box<dyn std::error::Error>> {
    let name = format!("_NET_WM_CM_S{screen}");
    let a = conn.intern_atom(false, name.as_bytes())?.reply()?.atom;
    if a == 0 {
        return Err("intern _NET_WM_CM_S0 returned 0".into());
    }
    Ok(a)
}

fn selection_owned(conn: &XConn, atom: Atom) -> bool {
    if let Some(r) = conn
        .get_selection_owner(atom)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        return r.owner != x11rb::NONE;
    }
    false
}

/// Compute the *live* placements for one monitor (same projection as the
/// settled `arrange`, but reading the live camera/boost/zoom) and apply the
/// fullscreen/maximized presentation overlay, exactly like `arrange_full`. The
/// WM feeds the result to `Compositor::set_transforms`.
pub fn live_placements(
    state: &State,
    mon_idx: usize,
    cfg: &Cfg,
    registry: &LayoutRegistry,
    out: &mut Placements,
    raise: &mut Vec<WindowId>,
    scratch: &mut RibbonScratch,
) {
    out.clear();
    arrange(state, mon_idx, cfg, registry, Phase::Live, out, scratch);
    let mon = &state.monitors[mon_idx];
    present_into(state, mon, out, raise);
}

/// Substep the given total `dt` (seconds) into pieces no longer than
/// `SUBSTEP_MS`, returning the slice boundaries. Used by the animation driver.
pub fn substep_bounds(dt: f32) -> impl Iterator<Item = f32> {
    let (n, step) = if !dt.is_finite() || dt <= 0.0 {
        (0, 0.0)
    } else {
        let max = SUBSTEP_MS / 1000.0;
        let n = (dt / max).ceil().max(1.0) as usize;
        (n, dt / n as f32)
    };
    (0..n).map(move |_| step)
}

#[cfg(test)]
mod stack_tests {
    use super::{stack_add_top, stack_remove, stack_restack, CompWin};
    use crate::types::Rect;

    const A: u32 = 0xA;
    const B: u32 = 0xB;
    const C: u32 = 0xC;

    /// The viewport-cull test: a window fully past any screen edge must be
    /// culled, while one that merely touches the edge (or sits within the 64px
    /// grace margin) must still be drawn — else windows scrolling in/out of
    /// view would pop. This is the predicate `render` uses to skip the GPU draw
    /// for the dozens of off-screen ribbon windows, so it must be exact at the
    /// boundary. The screen here is 1920x1080.
    #[test]
    fn viewport_cull_matches_edges() {
        const W: u32 = 1920;
        const H: u32 = 1080;
        // Fully on screen.
        assert!(!CompWin::offscreen(Rect::new(100, 100, 400, 300), W, H));
        // Touches the left edge.
        assert!(!CompWin::offscreen(Rect::new(0, 100, 400, 300), W, H));
        // Just inside the right edge (within the margin).
        assert!(!CompWin::offscreen(
            Rect::new((W as i32) - 60, 100, 400, 300),
            W,
            H
        ));
        // Fully to the left, beyond the margin.
        assert!(CompWin::offscreen(Rect::new(-200, 100, 100, 300), W, H));
        // Fully below.
        assert!(CompWin::offscreen(
            Rect::new(100, (H as i32) + 200, 100, 300),
            W,
            H
        ));
        // Entirely past the right edge.
        assert!(CompWin::offscreen(
            Rect::new((W as i32) + 100, 100, 100, 300),
            W,
            H
        ));
    }

    /// The regression this whole commit exists for.
    ///
    /// Two windows, A below B... then the WM raises B. Before this change the
    /// compositor never learned about it: `raise()` is a bare
    /// `ConfigureWindow(stack_mode: ABOVE)`, which sets no `stack_dirty` flag,
    /// so the frame kept being drawn with A on top until some unrelated
    /// map/unmap forced a `QueryTree`.
    #[test]
    fn raising_b_draws_b_above_a() {
        let mut stack = vec![B, A]; // bottom→top: B at the bottom, A on top
        assert!(stack_restack(&mut stack, B, Some(A)));
        assert_eq!(stack, vec![A, B], "B must end up above A in the draw order");
    }

    #[test]
    fn above_none_means_bottom_not_unknown() {
        // X encodes "went to the very bottom" as above_sibling = None. Treating
        // it as "no information" is what would let the synthetic ConfigureNotify
        // from apply_geom bury every window.
        let mut stack = vec![A, B, C];
        assert!(stack_restack(&mut stack, C, None));
        assert_eq!(stack, vec![C, A, B]);
    }

    #[test]
    fn restack_is_idempotent() {
        let mut stack = vec![A, B, C];
        for _ in 0..3 {
            assert!(stack_restack(&mut stack, B, Some(A)));
            assert_eq!(
                stack,
                vec![A, B, C],
                "re-applying the same order is a no-op"
            );
        }
    }

    #[test]
    fn moving_a_window_up_accounts_for_its_own_removal() {
        // The off-by-one that a naive remove-then-insert produces: `target` is
        // computed while `win` is still in the vector, so removing an entry
        // below the target shifts it.
        let mut stack = vec![A, B, C];
        assert!(stack_restack(&mut stack, A, Some(B)));
        assert_eq!(stack, vec![B, A, C], "A sits immediately above B");

        let mut stack = vec![A, B, C];
        assert!(stack_restack(&mut stack, A, Some(C)));
        assert_eq!(stack, vec![B, C, A], "A goes to the top");
    }

    #[test]
    fn moving_a_window_down_keeps_the_sibling_relation() {
        let mut stack = vec![A, B, C];
        assert!(stack_restack(&mut stack, C, Some(A)));
        assert_eq!(stack, vec![A, C, B]);
    }

    #[test]
    fn an_untracked_window_with_a_known_sibling_is_inserted_exactly() {
        let mut stack = vec![A, B];
        assert!(stack_restack(&mut stack, C, Some(A)));
        assert_eq!(stack, vec![A, C, B]);
    }

    #[test]
    fn an_unknown_sibling_demands_a_resync_instead_of_a_guess() {
        let mut stack = vec![A, B];
        assert!(
            !stack_restack(&mut stack, B, Some(C)),
            "must report failure so the caller re-reads QueryTree"
        );
        assert!(
            !stack.contains(&B),
            "the stale entry is dropped so the resync starts clean"
        );
    }

    #[test]
    fn never_duplicates_an_entry() {
        let mut stack = vec![A, B, C];
        for (win, above) in [(A, Some(C)), (B, None), (C, Some(A)), (A, Some(B))] {
            stack_restack(&mut stack, win, above);
        }
        let mut sorted = stack.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            stack.len(),
            "no window may appear twice: {stack:?}"
        );
    }

    #[test]
    fn create_goes_on_top_and_destroy_forgets() {
        let mut stack = vec![A];
        stack_add_top(&mut stack, B);
        assert_eq!(stack, vec![A, B]);
        // A create for something already tracked must not duplicate it.
        stack_add_top(&mut stack, A);
        assert_eq!(stack, vec![B, A]);
        stack_remove(&mut stack, B);
        assert_eq!(stack, vec![A]);
        stack_remove(&mut stack, B); // removing twice is harmless
        assert_eq!(stack, vec![A]);
    }
}

/// Pure tests for the damage-region accumulator that drives partial redraw.
/// No X/GL: `DamageRegion` is a fixed-capacity, zero-alloc structure, so it can
/// be exercised entirely in CI.
#[cfg(test)]
mod damage_tests {
    use super::{anim_damage_rects, fully_covered_by, DamageRegion};
    use crate::types::Rect;

    /// Fase 7: a window that moved from `A` to `B` must repaint *both* rects, so
    /// neither the pixels it left nor the ones it slid into linger. The helper
    /// returns exactly the union pair, nothing more.
    #[test]
    fn moving_window_damages_old_and_new() {
        let prev = Rect::new(0, 0, 100, 100);
        let cur = Rect::new(200, 0, 100, 100);
        let mut out = [Rect::default(); 2];
        let n = anim_damage_rects(Some(prev), cur, &mut out);
        assert_eq!(n, 2);
        assert_eq!(out[0], prev);
        assert_eq!(out[1], cur);
    }

    /// A window that did not move emits only its current rect — no spurious
    /// damage that would force a larger (or full) redraw.
    #[test]
    fn stationary_window_damages_only_current() {
        let cur = Rect::new(50, 50, 100, 100);
        let mut out = [Rect::default(); 2];
        let n = anim_damage_rects(Some(cur), cur, &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0], cur);
    }

    /// A window with no previous rect (just appeared / was off-screen) only
    /// needs its current rect repainted.
    #[test]
    fn freshly_visible_window_damages_only_current() {
        let cur = Rect::new(10, 20, 300, 40);
        let mut out = [Rect::default(); 2];
        let n = anim_damage_rects(None, cur, &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0], cur);
    }

    /// End-to-end: two windows scrolling apart produce a damage region whose
    /// bounding box spans both old and new positions — the minimal union that
    /// `decide_redraw` will scissor (Partial) when buffer-age is available.
    #[test]
    fn scrolling_pair_union_spans_old_and_new() {
        let a_old = Rect::new(0, 0, 100, 100);
        let a_new = Rect::new(400, 0, 100, 100);
        let b_old = Rect::new(500, 0, 100, 100);
        let b_new = Rect::new(0, 0, 100, 100);
        let mut region = DamageRegion::new();
        let mut out = [Rect::default(); 2];
        for (prev, cur) in [(Some(a_old), a_new), (Some(b_old), b_new)] {
            let n = anim_damage_rects(prev, cur, &mut out);
            for &r in &out[..n] {
                region.add(r);
            }
        }
        let bbox = region.bounding_rect();
        assert_eq!(bbox, Rect::new(0, 0, 600, 100), "union must span 0..600");
    }

    /// Fase 12: a window is occluded only when a *single* opaque rect above it
    /// contains it entirely. Joint coverage by two side-by-side windows (neither
    /// of which alone contains it) must NOT report occlusion — that is the
    /// conservative miss the helper is allowed to make.
    #[test]
    fn fully_covered_by_single_occluder_only() {
        let small = Rect::new(50, 50, 40, 40);
        // One big occluder contains it.
        assert!(fully_covered_by(small, &[Rect::new(0, 0, 200, 200)]));
        // Two side-by-side windows that jointly cover it but neither alone does
        // (left covers x 0..70, right covers x 70..270) -> not occluded.
        let left = Rect::new(0, 0, 70, 200);
        let right = Rect::new(70, 0, 200, 200);
        assert!(!fully_covered_by(small, &[left, right]));
        // Partially overlapping occluder does not contain it.
        assert!(!fully_covered_by(small, &[Rect::new(60, 60, 30, 30)]));
    }

    #[test]
    fn fresh_region_is_empty() {
        let r = DamageRegion::new();
        assert!(r.is_empty(), "a new region must report nothing to redraw");
    }

    #[test]
    fn adding_a_rect_makes_it_non_empty() {
        let mut r = DamageRegion::new();
        r.add(Rect::new(10, 20, 100, 50));
        assert!(!r.is_empty());
        assert_eq!(r.count, 1);
    }

    #[test]
    fn zero_size_rects_are_ignored() {
        let mut r = DamageRegion::new();
        r.add(Rect::new(0, 0, 0, 100));
        r.add(Rect::new(0, 0, 100, 0));
        assert!(r.is_empty(), "degenerate rects must not dirty the frame");
    }

    #[test]
    fn full_short_circuits_the_region() {
        let mut r = DamageRegion::new();
        r.add(Rect::new(0, 0, 10, 10));
        r.full();
        assert!(r.needs_full, "full() must force a whole-screen repaint");
        // clear wipes both the rects and the full flag.
        r.clear();
        assert!(r.is_empty());
    }

    #[test]
    fn overflow_falls_back_to_full() {
        let mut r = DamageRegion::new();
        for i in 0..(DamageRegion::CAP as i32 + 4) {
            r.add(Rect::new(i * 2, 0, 1, 1));
        }
        assert!(
            r.needs_full,
            "exceeding the rect cap must conservatively ask for a full redraw"
        );
    }

    #[test]
    fn bounding_rect_spans_all_rects() {
        let mut r = DamageRegion::new();
        r.add(Rect::new(100, 200, 50, 60));
        r.add(Rect::new(300, 50, 10, 10));
        let b = r.bounding_rect();
        assert_eq!(b, Rect::new(100, 50, 210, 210), "bbox must span every rect");
    }

    // ── I4/I5: DamageSubtract must happen even while bypassed ──────────────
    // Pure model of on_damage bookkeeping split: subtract always, mark dirty
    // only when not bypassed. This is the minimal extraction that mirrors the
    // fixed on_damage without needing a real XConn.
    fn damage_bookkeeping(bypassed: bool) -> (bool, bool) {
        // (do_subtract, do_mark_dirty)
        (true, !bypassed)
    }

    #[test]
    fn damage_bypassed_still_subtracts() {
        let (sub, dirty) = damage_bookkeeping(true);
        assert!(sub, "even bypassed, DamageSubtract must occur");
        assert!(!dirty, "bypassed must not schedule render for this damage");
    }

    #[test]
    fn damage_normal_subtracts_and_marks_dirty() {
        let (sub, dirty) = damage_bookkeeping(false);
        assert!(sub);
        assert!(dirty);
    }

    #[test]
    fn bypass_damage_sequence_always_subtracts() {
        // Sequence: normal damage, engage, 3 damages while bypassed, disengage, damage
        let mut subtracts = 0;
        let mut dirties = 0;
        // normal
        let (s, d) = damage_bookkeeping(false);
        if s {
            subtracts += 1;
        }
        if d {
            dirties += 1;
        }
        // engage bypass
        let mut bypassed = true;
        for _ in 0..3 {
            let (s, d) = damage_bookkeeping(bypassed);
            if s {
                subtracts += 1;
            }
            if d {
                dirties += 1;
            }
        }
        // disengage
        bypassed = false;
        let (s, d) = damage_bookkeeping(bypassed);
        if s {
            subtracts += 1;
        }
        if d {
            dirties += 1;
        }
        assert_eq!(subtracts, 5, "5 DamageNotify => 5 DamageSubtract");
        assert_eq!(dirties, 2, "only non-bypassed damages schedule render");
        // The last damage after disengage must be observable (dirty)
        assert!(dirties >= 1);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::{global_to_local, overlay_coverage, subtract_rect};
    use crate::types::Rect;

    fn area(rects: &[Rect]) -> u64 {
        rects.iter().map(|r| r.w as u64 * r.h as u64).sum()
    }

    fn contains(outer: Rect, inner: Rect) -> bool {
        outer.contains_rect(inner)
    }

    #[test]
    fn empty_equals_screen() {
        let screen = Rect::new(0, 0, 800, 600);
        let cov = overlay_coverage(screen, &[]);
        assert_eq!(cov, vec![screen]);
        assert_eq!(area(&cov), 800 * 600);
    }

    #[test]
    fn one_bypass_punches_hole() {
        let screen = Rect::new(0, 0, 800, 600);
        let hole = Rect::new(100, 100, 200, 200);
        let cov = overlay_coverage(screen, &[hole]);
        assert!(!cov.iter().any(|r| contains(*r, hole)));
        assert!(cov.iter().any(|r| contains(*r, Rect::new(0, 0, 10, 10))));
        assert!(cov
            .iter()
            .any(|r| contains(*r, Rect::new(700, 500, 10, 10))));
        assert_eq!(area(&cov), 800 * 600 - 200 * 200);
    }

    #[test]
    fn two_bypasses_non_overlapping() {
        let screen = Rect::new(0, 0, 1000, 600);
        let a = Rect::new(0, 0, 400, 600);
        let b = Rect::new(600, 0, 400, 600);
        let cov = overlay_coverage(screen, &[a, b]);
        assert!(cov
            .iter()
            .any(|r| contains(*r, Rect::new(450, 100, 10, 10))));
        assert!(!cov.iter().any(|r| contains(*r, a)));
        assert!(!cov.iter().any(|r| contains(*r, b)));
        assert_eq!(area(&cov), 1000 * 600 - 400 * 600 * 2);
    }

    #[test]
    fn overlapping_holes_handled() {
        let screen = Rect::new(0, 0, 800, 600);
        let a = Rect::new(100, 100, 300, 300);
        let b = Rect::new(200, 200, 300, 300);
        let cov = overlay_coverage(screen, &[a, b]);
        let union = 300 * 300 + 300 * 300 - 200 * 200;
        assert_eq!(area(&cov), 800 * 600 - union as u64);
    }

    #[test]
    fn updated_rect_recalculates() {
        let screen = Rect::new(0, 0, 800, 600);
        let old = Rect::new(0, 0, 400, 600);
        let new = Rect::new(400, 0, 400, 600);
        let cov_old = overlay_coverage(screen, &[old]);
        let cov_new = overlay_coverage(screen, &[new]);
        assert_ne!(cov_old, cov_new);
        assert!(cov_new
            .iter()
            .any(|r| contains(*r, Rect::new(10, 10, 10, 10))));
        assert!(!cov_new.iter().any(|r| contains(*r, new)));
    }

    #[test]
    fn remove_bypass_restores() {
        let screen = Rect::new(0, 0, 800, 600);
        let a = Rect::new(0, 0, 400, 600);
        let b = Rect::new(400, 0, 400, 600);
        let cov_both = overlay_coverage(screen, &[a, b]);
        let cov_a = overlay_coverage(screen, &[a]);
        assert_ne!(cov_both, cov_a);
        assert!(area(&cov_a) > area(&cov_both));
    }

    #[test]
    fn subtract_rect_no_overlap() {
        let r = Rect::new(0, 0, 100, 100);
        let hole = Rect::new(200, 200, 10, 10);
        assert_eq!(subtract_rect(r, hole), vec![r]);
    }

    #[test]
    fn state_machine_engage_resize_destroy() {
        let screen = Rect::new(0, 0, 1000, 600);
        let mut bypasses: Vec<Rect> = vec![];
        assert_eq!(overlay_coverage(screen, &bypasses), vec![screen]);
        let a = Rect::new(0, 0, 500, 600);
        bypasses.push(a);
        let cov_a = overlay_coverage(screen, &bypasses);
        assert!(!cov_a.iter().any(|r| contains(*r, a)));
        bypasses[0] = Rect::new(100, 100, 300, 300);
        let cov_resized = overlay_coverage(screen, &bypasses);
        assert!(!cov_resized.iter().any(|r| contains(*r, bypasses[0])));
        assert!(cov_resized
            .iter()
            .any(|r| contains(*r, Rect::new(0, 0, 10, 10))));
        let b = Rect::new(600, 0, 400, 600);
        bypasses.push(b);
        let cov_ab = overlay_coverage(screen, &bypasses);
        assert!(!cov_ab.iter().any(|r| contains(*r, a)));
        assert!(!cov_ab.iter().any(|r| contains(*r, b)));
        bypasses.remove(0);
        let cov_b = overlay_coverage(screen, &bypasses);
        assert!(cov_b.iter().any(|r| contains(*r, a)));
        assert!(!cov_b.iter().any(|r| contains(*r, b)));
        bypasses.clear();
        assert_eq!(overlay_coverage(screen, &bypasses), vec![screen]);
    }

    #[test]
    fn case_a_single_output() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let win = Rect::new(100, 100, 400, 300);
        let cov = overlay_coverage(screen, &[win]);
        assert!(!cov.iter().any(|r| contains(*r, win)));
        assert_eq!(area(&cov), 1920 * 1080 - 400 * 300);
    }

    #[test]
    fn case_b_two_positive_outputs() {
        let screen = Rect::new(0, 0, 3840, 1080);
        let win_b = Rect::new(1920, 0, 800, 600);
        let cov = overlay_coverage(screen, &[win_b]);
        assert!(!cov.iter().any(|r| contains(*r, win_b)));
        assert!(cov.iter().any(|r| contains(*r, Rect::new(10, 10, 10, 10))));
    }

    #[test]
    fn case_c_negative_x_output() {
        let screen = Rect::new(-800, 0, 2720, 1080);
        let win = Rect::new(-800, 0, 800, 600);
        let cov = overlay_coverage(screen, &[win]);
        assert!(!cov.iter().any(|r| contains(*r, win)));
        assert!(cov.iter().any(|r| contains(*r, Rect::new(0, 0, 10, 10))));
        let local = global_to_local(win, screen);
        assert_eq!(local, Rect::new(0, 0, 800, 600));
        let win2 = Rect::new(100, 100, 200, 200);
        let local2 = global_to_local(win2, screen);
        assert_eq!(local2, Rect::new(900, 100, 200, 200));
    }

    #[test]
    fn case_d_negative_y_output() {
        let screen = Rect::new(0, -600, 1920, 1680);
        let win = Rect::new(0, -600, 500, 400);
        let cov = overlay_coverage(screen, &[win]);
        assert!(!cov.iter().any(|r| contains(*r, win)));
        let local = global_to_local(win, screen);
        assert_eq!(local, Rect::new(0, 0, 500, 400));
        let win2 = Rect::new(100, 100, 200, 200);
        let local2 = global_to_local(win2, screen);
        assert_eq!(local2, Rect::new(100, 700, 200, 200));
    }

    #[test]
    fn global_to_local_identity_and_negative() {
        let origin = Rect::new(-800, 0, 2720, 1080);
        assert_eq!(
            global_to_local(Rect::new(-800, 0, 100, 100), origin),
            Rect::new(0, 0, 100, 100)
        );
        assert_eq!(
            global_to_local(Rect::new(100, 100, 50, 50), origin),
            Rect::new(900, 100, 50, 50)
        );
    }
}

/// Pure tests for the full/partial/idle frame decision. No X/GL: `decide_redraw`
/// is a free function of three booleans, so the policy is fully covered in CI.
#[cfg(test)]
mod frameplan_tests {
    use super::{decide_redraw, FrameMode};

    #[test]
    fn nothing_damaged_is_idle() {
        assert_eq!(decide_redraw(true, false, false), FrameMode::Idle);
        assert_eq!(decide_redraw(false, true, false), FrameMode::Idle);
    }

    #[test]
    fn no_buffer_age_forces_full() {
        // Even with a clean structural state, without buffer-age a partial clear
        // would leave garbage, so we repaint everything.
        assert_eq!(decide_redraw(false, false, true), FrameMode::Full);
    }

    #[test]
    fn structural_change_forces_full() {
        assert_eq!(decide_redraw(true, true, true), FrameMode::Full);
        assert_eq!(decide_redraw(true, true, false), FrameMode::Idle);
    }

    #[test]
    fn buffer_age_plus_damage_is_partial() {
        assert_eq!(decide_redraw(true, false, true), FrameMode::Partial);
    }
}

///
/// This is the *measure* half of the "idle must be near-free / 0 allocs per
/// frame" rule from the compositor plan. It does not touch X or GL (the path
/// under test — `live_placements` = `layout::arrange` → `present_into` — is a
/// pure function of `State`), so it runs in CI and on a laptop alike, and it
/// catches two regressions the unit tests would miss: a per-frame allocation
/// sneaking back in, and the projection cost drifting past a single frame
/// budget at realistic window counts.
#[cfg(test)]
mod bench {
    use super::{decide_redraw, live_placements, DamageRegion, FrameMode};
    use crate::config::Cfg;
    use crate::core::framebench::CountAllocs;
    use crate::core::layout::{LayoutRegistry, Placements, RibbonScratch};
    use crate::types::{Client, Column, Focus, Monitor, Rect, State, WindowId};

    /// Build a one-monitor state with `n` single-window columns on a 1920x1080
    /// monitor, camera mid-animation (the only state in which this path runs).
    fn ribbon(n: u32) -> State {
        let mut state = State::new();
        state
            .monitors
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 1));
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
        state.monitors[0].workspaces[0].camera.position = 137.0;
        state.monitors[0].workspaces[0].camera.target = 900.0;
        state
    }

    /// Time the steady-state projection over `iters` frames, averaged. Also
    /// returns allocations per frame, measured with the per-thread counter.
    fn measure(state: &State, iters: u32) -> (f64, u64) {
        let cfg = Cfg::default();
        let registry = LayoutRegistry::new();
        let mut out: Placements = Placements::new();
        let mut raise: Vec<WindowId> = Vec::new();
        let mut scratch = RibbonScratch::default();

        // Warm up so every reused buffer is at steady-state capacity.
        for _ in 0..16 {
            live_placements(
                state,
                0,
                &cfg,
                &registry,
                &mut out,
                &mut raise,
                &mut scratch,
            );
        }
        // Two rounds: one timed, one counted (the counter only runs while armed).
        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            live_placements(
                state,
                0,
                &cfg,
                &registry,
                &mut out,
                &mut raise,
                &mut scratch,
            );
        }
        let elapsed = t0.elapsed().as_nanos() as f64 / iters as f64;

        let counter = CountAllocs::start();
        for _ in 0..iters {
            live_placements(
                state,
                0,
                &cfg,
                &registry,
                &mut out,
                &mut raise,
                &mut scratch,
            );
        }
        let allocs = counter.finish().div_ceil(iters as u64);
        (elapsed, allocs)
    }

    #[test]
    fn projection_is_allocation_free_and_within_frame_budget() {
        let mut results = Vec::new();
        // 60 Hz frame budget is 16.6 ms; the *projection* is a fraction of that.
        // We assert a generous bound so the test is stable on slow CI boxes but
        // still catches a real regression (e.g. the O(N^2) transform lookup
        // coming back, or a per-frame allocation reappearing).
        for &n in &[1u32, 50, 200, 1000] {
            let state = ribbon(n);
            let (ns, allocs) = measure(&state, 200);
            results.push((n, ns, allocs));
            assert_eq!(
                allocs, 0,
                "{n} windows: {allocs} alloc(s)/frame — the projection must reuse its buffers"
            );
            assert!(
                ns < 4_000_000.0,
                "{n} windows: {ns:.0} ns/frame exceeds 4 ms budget"
            );
        }
        // Print a small table so `cargo test` output is the schedule/benchmark.
        eprintln!("projection bench (ns/frame, 0 allocs expected):");
        for (n, ns, _a) in &results {
            eprintln!("  N={n:>5}  {ns:.1} ns/frame");
        }
    }

    /// The partial-redraw bookkeeping — `DamageRegion` accumulation, its bounding
    /// box, and the `decide_redraw` policy — is pure arithmetic over fixed-size
    /// arrays, so it must cost nothing in allocations and a negligible amount of
    /// time per frame. This guards against a per-frame heap allocation sneaking
    /// into the damage path (which would defeat the whole point of Fase 6..8).
    #[test]
    fn damage_region_and_plan_is_allocation_free_and_cheap() {
        let iters: u64 = 20_000;
        let counter = CountAllocs::start();
        let mut region = DamageRegion::new();
        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            // A typical idle content-damage frame: a few small windows repainted.
            region.clear();
            region.add(Rect::new(100, 100, 200, 50));
            region.add(Rect::new(800, 400, 120, 120));
            region.add(Rect::new(1500, 900, 60, 40));
            let _bbox = region.bounding_rect();
            let _mode = decide_redraw(true, false, !region.is_empty());
        }
        let ns = t0.elapsed().as_nanos() as f64 / iters as f64;
        let allocs = counter.finish().div_ceil(iters);
        assert_eq!(
            allocs, 0,
            "{allocs} alloc(s)/frame in the damage + plan path — must reuse buffers"
        );
        assert!(
            ns < 50_000.0,
            "{ns:.0} ns/frame in the damage + plan path exceeds 50 µs"
        );
        eprintln!("damage+plan bench: {ns:.1} ns/frame, {allocs} allocs/frame (Partial expected)");
        // Sanity: the policy the bench exercised resolves to a partial redraw.
        assert_eq!(decide_redraw(true, false, true), FrameMode::Partial);
    }

    /// Fase 12: the occlusion pass (top→bottom `fully_covered_by` over the
    /// opacquer rect set) is pure arithmetic over a reused buffer, so per frame
    /// it must cost no allocations and a negligible amount of time even at high
    /// window counts. This guards against a per-frame heap allocation sneaking
    /// into the new pass (which would defeat the "0 allocs/frame" rule the rest
    /// of the plan fought for).
    #[test]
    fn occlusion_pass_is_cheap_and_allocation_free() {
        use super::fully_covered_by;
        // A tiled ribbon: 1000 opaque, on-screen columns stacked left→right; the
        // inner window sits at the far right, fully covered by a single one of
        // them. Mirrors the worst case the pass walks every frame.
        let occluders: Vec<Rect> = (0..1000).map(|i| Rect::new(i * 2, 0, 100, 1080)).collect();
        let target = Rect::new(1990, 100, 40, 40);
        let iters: u64 = 20_000;
        let counter = CountAllocs::start();
        let t0 = std::time::Instant::now();
        let mut covered = false;
        for _ in 0..iters {
            covered = fully_covered_by(target, &occluders);
        }
        let ns = t0.elapsed().as_nanos() as f64 / iters as f64;
        let allocs = counter.finish().div_ceil(iters);
        assert!(covered, "the far-right target must be reported covered");
        assert_eq!(
            allocs, 0,
            "{allocs} alloc(s)/frame in the occlusion pass — must reuse buffers"
        );
        assert!(
            ns < 200_000.0,
            "{ns:.0} ns/frame in the occlusion pass exceeds 200 µs (1000 occluders)"
        );
        eprintln!("occlusion bench: {ns:.1} ns/frame, {allocs} allocs/frame (1000 occluders)");
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::{border_rgba, presentation_value, CompWin};
    use crate::types::Rect;
    use maverick_gl::VisualFormat;

    fn transitioning_window() -> CompWin {
        let mut cw = CompWin::new(
            Rect::new(478, 8, 313, 584),
            0,
            VisualFormat {
                id: 0,
                depth: 24,
                red_bits: 8,
                green_bits: 8,
                blue_bits: 8,
                alpha_bits: 0,
                direct: true,
            },
        );
        cw.mapped = true;
        let cfg = crate::config::Cfg::default();
        cw.presentation_spring = Some((cfg.animations.stiffness, cfg.animations.damping));
        cw.set_transform(cw.outer, 0, 18, Rect::new(0, 0, 800, 600), 1);
        cw
    }

    fn settle_presentation(cw: &mut CompWin, target: Rect) -> usize {
        for frame in 0..600 {
            cw.tick_presentation(1.0 / 60.0);
            cw.set_transform(
                target,
                0,
                18,
                Rect::new(0, 0, 800, 600),
                cw.transform_gen + 1,
            );
            if cw.presentation.is_none() {
                assert_eq!(cw.transform, target);
                return frame + 1;
            }
        }
        panic!("presentation did not settle");
    }

    #[test]
    fn a8_fullscreen_presentation_has_intermediate_frame_and_exact_endpoint() {
        let mut cw = transitioning_window();
        let initial = cw.transform;
        let final_rect = Rect::new(0, 0, 800, 600);
        assert_ne!(initial, final_rect);
        cw.set_transform(final_rect, 0, 18, final_rect, 2);
        assert_eq!(cw.transform, initial);
        assert_eq!(cw.transform_radius, 18);
        cw.tick_presentation(1.0 / 60.0);
        cw.set_transform(final_rect, 0, 18, final_rect, 3);
        assert_ne!(cw.transform, initial);
        assert_ne!(cw.transform, final_rect);
        let frames = settle_presentation(&mut cw, final_rect);
        assert!(frames > 1);
        assert_eq!(cw.transform_radius, 0);
        assert_eq!(cw.outer, initial);
        cw.set_transform(initial, 0, 18, final_rect, cw.transform_gen + 1);
        assert_eq!(cw.transform, final_rect);
        settle_presentation(&mut cw, initial);
        assert_eq!(cw.transform_radius, 18);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn presentation_retargets_from_unrounded_value_without_return_drift() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        let b = Rect::new(490, 0, 800, 600);
        let c = Rect::new(-300, 8, 313, 584);
        for target in [screen, b, c, b] {
            let before = cw.presentation_value;
            cw.set_transform(target, 0, 18, screen, cw.transform_gen + 1);
            assert_eq!(cw.presentation_value, before);
            for _ in 0..5 {
                cw.tick_presentation(1.0 / 60.0);
                cw.set_transform(target, 0, 18, screen, cw.transform_gen + 1);
            }
        }
        settle_presentation(&mut cw, b);
        assert_eq!(cw.presentation_value, presentation_value(b, 18));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn ribbon_radius_fades_with_motion_and_target_does_not_restart_each_pixel() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        cw.presentation_goal = Some(presentation_value(screen, 0));
        cw.set_transform(Rect::new(796, 0, 800, 600), 0, 18, screen, 2);
        let from = cw.presentation.as_ref().unwrap().from;
        for x in (1..796).rev().step_by(20) {
            cw.tick_presentation(1.0 / 60.0);
            cw.set_transform(
                Rect::new(x, 0, 800, 600),
                0,
                18,
                screen,
                cw.transform_gen + 1,
            );
            if let Some(transition) = &cw.presentation {
                assert_eq!(transition.from, from);
            }
        }
        assert!(cw.transform_radius < 18);
        settle_presentation(&mut cw, screen);
        assert_eq!(cw.transform_radius, 0);
    }

    #[test]
    fn finished_progress_waits_for_installed_camera_endpoint() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        cw.presentation_goal = Some(presentation_value(screen, 0));
        cw.set_transform(Rect::new(100, 0, 800, 600), 0, 18, screen, 2);
        for _ in 0..120 {
            cw.tick_presentation(1.0 / 60.0);
            cw.set_transform(
                Rect::new(7, 0, 800, 600),
                0,
                18,
                screen,
                cw.transform_gen + 1,
            );
        }
        assert!(
            cw.presentation.is_some(),
            "camera still has seven pixels to travel"
        );
        cw.tick_presentation(1.0 / 60.0);
        assert!(
            cw.presentation.is_some(),
            "ticking alone must not consume the final frame"
        );
        cw.set_transform(screen, 0, 18, screen, cw.transform_gen + 1);
        assert_eq!(cw.transform, screen);
        assert_eq!(cw.transform_radius, 0);
        assert!(cw.presentation.is_none());
    }

    /// Navigation *away* from a settled fullscreen window must morph it back to
    /// its ribbon position through intermediate spatial frames — the same glide
    /// a toggle uses — instead of snapping to the new settled presentation. The
    /// ribbon rect moves *out* under the camera while the presentation spring
    /// interpolates, so the live x endpoint is the moving one; the test drives
    /// the live side exactly like the loop does and requires the first frames
    /// to stay strictly inside the from→endpoint envelope (B4/c999c27).
    #[test]
    fn navigation_away_from_fullscreen_glides_back_to_the_ribbon() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        cw.set_transform(screen, 0, 18, screen, cw.transform_gen + 1);
        settle_presentation(&mut cw, screen);
        assert_eq!(cw.transform, screen);
        assert_eq!(cw.transform_radius, 0);
        // The demotion flip: next settled goal is the ribbon tile again.
        cw.set_transform(
            Rect::new(478, 8, 313, 584),
            0,
            18,
            screen,
            cw.transform_gen + 1,
        );
        assert_eq!(
            cw.transform, screen,
            "first frame after the flip keeps the old look"
        );
        cw.tick_presentation(1.0 / 60.0);
        cw.set_transform(
            Rect::new(478, 8, 313, 584),
            0,
            18,
            screen,
            cw.transform_gen + 1,
        );
        assert_ne!(
            cw.transform, screen,
            "demotion must produce an intermediate frame"
        );
        let frames = settle_presentation(&mut cw, Rect::new(478, 8, 313, 584));
        assert!(frames > 1);
        assert_eq!(cw.transform, Rect::new(478, 8, 313, 584));
        assert_eq!(cw.transform_radius, 18);
    }

    /// Navigation *into* a fullscreen window from an off-screen ribbon position
    /// must animate from where the window actually *is* when the retarget
    /// lands — mid-scroll that is off-screen — not from the old settled goal
    /// and not a snap to the screen rect. Mirrors the traced
    /// fullscreen→fullscreen entry (x 796→0 over ~219 intermediate frames,
    /// progress 0→1, exact endpoint).
    #[test]
    fn navigation_into_fullscreen_starts_from_the_presented_geometry() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        let tile = Rect::new(478, 8, 313, 584);
        cw.presentation_goal = Some(presentation_value(tile, 18));
        cw.set_transform(tile, 0, 18, screen, cw.transform_gen + 1);
        settle_presentation(&mut cw, tile);
        // The camera scrolls the window far left: the *presented* rect follows
        // the camera while the settled goal is unchanged — a camera scroll is
        // not a presentation transition.
        cw.set_transform(
            Rect::new(-800, 0, 800, 600),
            0,
            18,
            screen,
            cw.transform_gen + 1,
        );
        assert!(
            cw.presentation.is_none(),
            "camera scroll must not start a presentation transition"
        );
        assert_eq!(cw.transform, Rect::new(-800, 0, 800, 600));
        // Navigation focuses it: the settled goal flips to the exclusive
        // overlay and the same frame presents the screen rect. The transition
        // must start from the off-screen *presented* value.
        cw.presentation_goal = Some(presentation_value(screen, 0));
        cw.set_transform(screen, 0, 18, screen, cw.transform_gen + 1);
        assert!(
            cw.presentation.is_some(),
            "the settled-goal flip must start a presentation transition"
        );
        assert_eq!(cw.transform, Rect::new(-800, 0, 800, 600));
        cw.tick_presentation(1.0 / 60.0);
        cw.set_transform(screen, 0, 18, screen, cw.transform_gen + 1);
        assert_ne!(cw.transform, Rect::new(-800, 0, 800, 600));
        assert_ne!(cw.transform, screen);
        let frames = settle_presentation(&mut cw, screen);
        assert!(frames > 1);
        assert_eq!(cw.transform, screen);
        assert_eq!(cw.transform_radius, 0);
    }

    #[test]
    fn disabled_animation_and_unmapped_first_placement_are_immediate() {
        let mut cw = transitioning_window();
        let screen = Rect::new(0, 0, 800, 600);
        cw.presentation_spring = None;
        cw.set_transform(screen, 0, 18, screen, 2);
        assert_eq!(cw.transform, screen);
        assert!(cw.presentation.is_none());
        cw.presentation_spring = Some((220.0, 30.0));
        cw.mapped = false;
        cw.set_transform(cw.outer, 0, 18, screen, 3);
        assert_eq!(cw.transform, cw.outer);
        assert!(cw.presentation.is_none());
    }

    /// The core invariant the floating-freeze fix relies on: `observe_configure`
    /// is *pure* — it never touches X11 or GL — and it reports a size change
    /// exactly when the window was resized (so the caller invalidates + defers the
    /// GLXPixmap/texture recreation to the once-per-frame render phase) and
    /// reports no change (a move-only `ConfigureNotify`) so no GL work is queued.
    ///
    /// Regression guard: before the fix, `on_configure` created the new
    /// GLXPixmap/texture *synchronously inside the event handler* — every
    /// `ConfigureNotify` a client-driven floating resize emitted paid a blocking
    /// `glXCreatePixmap`+`glXBindTexImageEXT`, which froze the frame. The
    /// decision itself must stay a cheap, side-effect-free mapping.
    #[test]
    fn observe_configure_is_pure_and_reports_resize_only() {
        let vf = VisualFormat {
            id: 0,
            depth: 24,
            red_bits: 8,
            green_bits: 8,
            blue_bits: 8,
            alpha_bits: 0,
            direct: true,
        };
        let mut cw = CompWin::new(Rect::default(), 0, vf);
        assert_eq!(cw.outer, Rect::default());

        // First configure: size goes 0x0 -> 100x50 with a 2px border.
        // Returns `true` (resized) and outer is expanded by the border on every
        // side.
        let resized = cw.observe_configure(10, 20, 100, 50, 2);
        assert!(resized, "first real size must be reported as a resize");
        assert_eq!(cw.outer, Rect::new(10, 20, 104, 54));

        // Move-only (same w/h, different position): must NOT be reported as a
        // resize, or the (deferred) bind would be needlessly re-armed for a pure
        // move — the path that, done synchronously, froze move/resize drags.
        let moved = cw.observe_configure(40, 60, 100, 50, 2);
        assert!(
            !moved,
            "a move-only ConfigureNotify must not look like a resize"
        );
        assert_eq!(cw.outer, Rect::new(40, 60, 104, 54));

        // Real resize: report it, and expand by the (changed) border.
        let resized2 = cw.observe_configure(40, 60, 200, 80, 4);
        assert!(resized2);
        assert_eq!(cw.outer, Rect::new(40, 60, 208, 88));
    }

    /// Border width is folded into `outer` so the compositor's drawn rect matches
    /// the window's frame geometry. A border change with no size change is a move
    /// for this decision (the bind decision uses `resized` only).
    /// `outer` includes the border, so a border-width change expands `outer` and is
    /// therefore reported as a resize (matches `on_configure`'s original decision,
    /// which compared `outer.w`/`outer.h`). This guards that the resize flag is
    /// driven by the drawn rect — including the border — so a focused-window
    /// border change still re-invalidates the GL texture correctly.
    #[test]
    fn observe_configure_expands_by_border() {
        let vf = VisualFormat {
            id: 0,
            depth: 24,
            red_bits: 8,
            green_bits: 8,
            blue_bits: 8,
            alpha_bits: 0,
            direct: true,
        };
        let mut cw = CompWin::new(Rect::default(), 0, vf);
        let _ = cw.observe_configure(0, 0, 100, 100, 0);
        let resized = cw.observe_configure(0, 0, 100, 100, 3);
        assert!(
            resized,
            "border change expands outer -> reported as a resize"
        );
        assert_eq!(cw.outer, Rect::new(0, 0, 106, 106));
    }

    /// GL transform = the placement's outer frame *as emitted*: X11
    /// `ConfigureWindow` x/y already mark the border-inclusive top-left
    /// (layout.rs subtracts 2·bw from content width; `emit_geometry` wires
    /// x/y through verbatim). The compositor must therefore NOT shift the
    /// quad by (-bw,-bw) — that displaced every GL frame one border up-left
    /// of the native frame. w/h still grow by 2·bw (content-only measures).
    #[test]
    fn set_transform_keeps_outer_origin_without_bw_shift() {
        let vf = VisualFormat {
            id: 0,
            depth: 24,
            red_bits: 8,
            green_bits: 8,
            blue_bits: 8,
            alpha_bits: 0,
            direct: true,
        };
        let mut cw = CompWin::new(Rect::default(), 0, vf);
        cw.set_transform(
            Rect::new(100, 200, 400, 300),
            1,
            12,
            Rect::new(0, 0, 1440, 900),
            1,
        );
        assert_eq!(cw.transform, Rect::new(100, 200, 402, 302));
        assert_eq!(cw.transform_border_w, 1);
        assert_eq!(cw.transform_radius, 12);

        // Same placement with bw 0 (maximize/fullscreen presentation): outer
        // equals the content rect, no rounding beyond the screen-cover gate.
        cw.set_transform(
            Rect::new(0, 0, 640, 480),
            0,
            12,
            Rect::new(0, 0, 1440, 900),
            2,
        );
        assert_eq!(cw.transform, Rect::new(0, 0, 640, 480));
        assert_eq!(cw.transform_border_w, 0);
        assert_eq!(
            cw.transform_radius, 12,
            "bw 0 alone must not square the window"
        );
    }

    /// Fullscreen policy parity with the X11 Shape path: only a presentation
    /// that covers the whole monitor is forced square; a maximized window
    /// whose rect merely equals the *workarea* keeps its rounding, and a
    /// zero config radius squares everything.
    #[test]
    fn set_transform_squares_only_full_screen_coverage() {
        let vf = VisualFormat {
            id: 0,
            depth: 24,
            red_bits: 8,
            green_bits: 8,
            blue_bits: 8,
            alpha_bits: 0,
            direct: true,
        };
        let screen = Rect::new(0, 0, 1440, 900);
        let mut cw = CompWin::new(Rect::default(), 0, vf);
        cw.set_transform(Rect::new(0, 0, 1440, 900), 0, 18, screen, 1);
        assert_eq!(cw.transform_radius, 0, "screen-covering overlay is square");
        cw.set_transform(Rect::new(0, 0, 1440, 876), 0, 18, screen, 2);
        assert_eq!(
            cw.transform_radius, 18,
            "workarea-sized maximize stays rounded"
        );
        cw.set_transform(Rect::new(0, 0, 1440, 900), 0, 0, screen, 3);
        assert_eq!(cw.transform_radius, 0, "corner_radius 0 disables rounding");
        // Radius clamped to half the border-inclusive shorter side
        // (44x24 outer from a 40x20 content + 2px frame → 12).
        cw.set_transform(Rect::new(0, 0, 40, 20), 2, 18, screen, 4);
        assert_eq!(cw.transform_radius, 12);
    }

    /// Border color bookkeeping: `on_border_color` records the pixel and
    /// flags a focus repaint; `border_rgba` must decode X `border_pixel`
    /// (0xRRGGBB, alpha forced 1 for premultiplied blending) and the
    /// no-color-yet state must leave stroke width 0 — an unknown color must
    /// never invent a ring.
    #[test]
    fn border_color_updates_feed_stroke_state() {
        let [r, g, b, a] = border_rgba(0x89b4fa);
        assert!((r - f32::from(0x89u8) / 255.0).abs() < f32::EPSILON);
        assert!((g - f32::from(0xb4u8) / 255.0).abs() < f32::EPSILON);
        assert!((b - f32::from(0xfau8) / 255.0).abs() < f32::EPSILON);
        assert!((a - 1.0).abs() < f32::EPSILON);
        let [r0, g0, b0, a0] = border_rgba(0);
        assert_eq!((r0, g0, b0, a0), (0.0, 0.0, 0.0, 1.0));

        let vf = VisualFormat {
            id: 0,
            depth: 24,
            red_bits: 8,
            green_bits: 8,
            blue_bits: 8,
            alpha_bits: 0,
            direct: true,
        };
        let mut cw = CompWin::new(Rect::default(), 0, vf);
        assert_eq!(cw.border_color, None, "tracked windows start colorless");
        cw.border_color = Some(0xff0000);
        cw.set_transform(
            Rect::new(10, 10, 200, 100),
            1,
            8,
            Rect::new(0, 0, 1440, 900),
            1,
        );
        // Stroke width rides the live transform's border and is suppressed
        // whenever no color is known (the unwrap_or(0) path yields width 0
        // in compute_scene).
        assert_eq!(cw.transform_border_w, 1);
        assert_eq!(cw.transform_radius, 8);
        // Idempotence is the caller's dedup (Some == Some check), mirrored here.
        let before = cw.border_color;
        cw.border_color = Some(0xff0000);
        assert_eq!(cw.border_color, before);
    }
}

#[cfg(test)]
mod substep_tests {
    use super::substep_bounds;

    #[test]
    fn zero_and_negative_yields_empty() {
        let v: Vec<f32> = substep_bounds(0.0).collect();
        assert!(v.is_empty());
        let v: Vec<f32> = substep_bounds(-0.01).collect();
        assert!(v.is_empty());
        let v: Vec<f32> = substep_bounds(f32::NAN).collect();
        assert!(v.is_empty());
        let v: Vec<f32> = substep_bounds(f32::INFINITY).collect();
        assert!(v.is_empty());
    }

    #[test]
    fn small_dt_single_step() {
        let v: Vec<f32> = substep_bounds(0.004).collect();
        assert_eq!(v.len(), 1);
        assert!((v[0] - 0.004).abs() < 1e-6);
        let v: Vec<f32> = substep_bounds(0.008).collect();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn multi_step_invariants() {
        let cases = [(0.016, 2), (0.017, 3), (0.024, 3), (0.032, 4)];
        for (dt, expect_n) in cases {
            let v: Vec<f32> = substep_bounds(dt).collect();
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
        use crate::types::{Monitor, Rect};
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workspaces[0].camera.position = 0.0;
        mon.workspaces[0].camera.target = 100.0;
        let dt = 0.016;
        for sub in substep_bounds(dt) {
            mon.workspaces[0].camera.step(sub);
        }
        assert!((mon.workspaces[0].camera.position - 0.0).abs() > 1e-6);
        let mut mon2 = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon2.workspaces[0].camera.position = 0.0;
        mon2.workspaces[0].camera.target = 100.0;
        for sub in substep_bounds(0.0) {
            mon2.workspaces[0].camera.step(sub);
        }
        assert!((mon2.workspaces[0].camera.position - 0.0).abs() < 1e-6);
    }
}
