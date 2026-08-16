// maverick/src/backend/x11/compositor.rs
//
// The OpenGL/GLX compositor.
//
// It sits *on top* of the existing window manager, sharing the same X
// connection (so the same sequence-number space and event queue). What it
// changes is not *what* the WM does but *how often*:
//
//   * Without it, every animation frame re-`ConfigureWindow`s each window and
//     re-issues the XShape mask — a storm of X round-trips and client
//     repaints, with no vsync.
//   * With it, the WM writes a window's *final* geometry exactly once (the
//     `Settled` arrange), then the compositor draws that window's texture at a
//     *live* (current-instant) transform every frame. The spring is a GPU
//     matrix, not a configure storm; `glXSwapIntervalEXT(1)` makes `swap`
//     block until the vertical blank, which is the real vsync the old loop
//     could only approximate with a 16 ms guess.
//
// Design notes (see the plan for the full rationale):
//
//   * One overlay window (`CompositeGetOverlayWindow`) covers the whole root;
//     we draw into it directly. It's never redirected, and its *input* shape is
//     made empty so clicks fall through to the real windows underneath.
//   * Every managed window is redirected `Manual` (only when actually damaged
//     does Composite copy up), `NameWindowPixmap` turns the off-screen storage
//     into a GL texture via `GLX_EXT_texture_from_pixmap`, and
//     `XDamageSubtract` re-arms the per-window `Damage` so we only rebind a
//     texture when the client actually repainted.
//   * Alpha is **premultiplied** (X Render convention). The border, content and
//     descendants are all in one pixmap (Composite guarantees that), so a
//     single quad per window is enough — the rounded-corner SDF and opacity are
//     shader uniforms, not CPU tessellation, and not an X Shape mask (which
//     would destroy the client's own shape).
//   * If GL is missing, the 3.3 context can't be created, or another compositor
//     already owns `_NET_WM_CM_S0`, `init` returns `None` and the WM keeps the
//     classic path. This is the entire fallback story.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use maverick_gl::{
    DrawQuad, Filter, Rect as GlRect, Renderer, Texture, TextureHandle, VisualFormat, XConn,
    XDisplay,
};
use maverick_img::Rgba8;

use crate::core::wallpaper::{
    compute_wallpaper_rects, shader_is_animated, GpuImage, ShaderId, WallpaperGpu, WallpaperMode,
    WallpaperSource, WallpaperSpec,
};
use crate::log;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::damage::{ConnectionExt as _, Damage, ReportLevel};
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::*;

use crate::config::Cfg;
use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements, RibbonScratch};
use crate::core::present::present_into;
use crate::types::{Rect, State, WindowId};

/// Soft upper bound on substep length (seconds). The camera spring (`damping`
/// 30) is unstable above ~8 ms, so every animation frame is split into pieces
/// no longer than this — see `WindowManager::run_once`.
const SUBSTEP_MS: f32 = 8.0;

/// One redirected window the compositor tracks.
struct CompWin {
    /// Outer (border-inclusive) geometry as last seen from X.
    outer: Rect,
    border_w: u32,
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
    /// Fase 12 — true when this window is fully hidden behind a single opaque,
    /// square-cornered window above it this frame, so it need not be drawn.
    /// Recomputed every frame by `compute_scene`'s top→bottom occlusion pass.
    occluded: bool,
}

impl CompWin {
    fn new(outer: Rect, border_w: u32, format: VisualFormat) -> Self {
        Self {
            outer,
            border_w,
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
            // 0 is never a live generation: `set_transforms` pre-increments, so
            // the first frame is generation 1.
            transform_gen: 0,
            prev_visual: None,
            occluded: false,
        }
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
    dpy: XDisplay,
    renderer: Renderer,
    root: Window,
    #[allow(dead_code)]
    overlay: Window,
    screen_w: u32,
    screen_h: u32,
    /// Every visual this screen advertises, keyed by id. The compositor never
    /// infers a pixel format from a depth; it looks it up here.
    formats: HashMap<u32, VisualFormat>,
    /// The root/overlay visual — the format the frame is finally presented in.
    root_format: VisualFormat,
    /// The WM's own windows that must never be composited.
    ignored: HashSet<Window>,
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
}

impl Compositor {
    /// Try to bring up the compositor. Returns `None` (logging why) when GL is
    /// unavailable or another compositor already owns the screen.
    pub fn init(
        conn: Rc<XConn>,
        dpy: XDisplay,
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

        let mut renderer = match Renderer::new(
            dpy,
            screen_num as i32,
            overlay,
            root_visual,
            &visuals,
            screen_w,
            screen_h,
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
            renderer,
            root,
            overlay,
            screen_w,
            screen_h,
            formats,
            root_format,
            ignored,
            wins: HashMap::new(),
            damages: HashMap::new(),
            warned_visuals: HashSet::new(),
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
            force_full_redraw: std::env::var_os("MAVERICK_FORCE_FULL_REDRAW").is_some(),
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
            g.x as i32 - bw as i32,
            g.y as i32 - bw as i32,
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
        self.track(win);
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
        if let Some(cw) = self.wins.remove(&win) {
            self.release_texture(cw);
        }
        if let Some(dmg) = self.damages.remove(&win) {
            let _ = self.conn.damage_destroy(dmg);
        }
        stack_remove(&mut self.stack, win);
        self.mark_full(DirtyReason::SURFACE);
    }

    /// Window mapped (`MapNotify`). Name its off-screen pixmap and bind a texture.
    ///
    /// Mapping does **not** restack in X — an unmapped window keeps its place
    /// in the sibling order — so this deliberately does not touch `stack`. It
    /// only asks for a resync when the window is missing entirely, which means
    /// we never saw its `CreateNotify`.
    pub fn on_map(&mut self, win: Window) {
        if !self.wins.contains_key(&win) {
            self.track(win);
        }
        {
            let Some(cw) = self.wins.get_mut(&win) else {
                return;
            };
            cw.mapped = true;
        }
        self.rename_and_bind(win, false);
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
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.mapped = false;
            cw.hidden = false;
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
            self.mark_full(DirtyReason::SURFACE);
        }
    }

    /// Geometry change (`ConfigureNotify` for a tracked, non-root window).
    pub fn on_configure(&mut self, win: Window, x: i32, y: i32, w: u32, h: u32, bw: u32) {
        let (resized, mapped, stale) = {
            let Some(cw) = self.wins.get_mut(&win) else {
                return;
            };
            let new_outer = Rect::new(
                x.saturating_sub(bw as i32),
                y.saturating_sub(bw as i32),
                w + 2 * bw,
                h + 2 * bw,
            );
            let resized = new_outer.w != cw.outer.w || new_outer.h != cw.outer.h;
            cw.outer = new_outer;
            cw.border_w = bw;
            let mapped = cw.mapped;
            // NameWindowPixmap returns a *new* pixmap on every resize (per the
            // Composite spec), so the old GLXPixmap/texture is stale.
            let stale = if resized && mapped {
                (cw.tex.take(), cw.pixmap.take())
            } else {
                (None, None)
            };
            (resized, mapped, stale)
        };
        if let Some(t) = stale.0 {
            self.renderer.destroy_texture(t);
        }
        if let Some(pm) = stale.1 {
            let _ = self.conn.free_pixmap(pm);
        }
        if resized && mapped {
            self.rename_and_bind(win, false);
        }
        self.mark_full(DirtyReason::GEOMETRY);
    }

    /// Damage reported (`DamageNotify`). Re-arm and mark dirty; the texture is
    /// rebound right before drawing.
    pub fn on_damage(&mut self, win: Window) {
        if let Some(dmg) = self.damages.get(&win) {
            let _ = self.conn.damage_subtract(*dmg, x11rb::NONE, x11rb::NONE);
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

    /// Client changed its own X shape (`ShapeNotify`). We never clobber the
    /// client's shape with our own X Shape mask, so this is just a redraw
    /// hint; the SDF/vs shader path already handles corner rounding, and an
    /// arbitrary client shape is respected because we don't overwrite it.
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
        self.frame_gen = self.frame_gen.wrapping_add(1);
        let gen = self.frame_gen;
        let corner_radius = self.corner_radius;
        for &(win, geom, bw) in placements {
            // `ignored` windows are never tracked (see `track`), so the lookup
            // below already rejects them — no separate set probe needed.
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            // Outer rect = content + borders on every side.
            let outer = Rect::new(
                geom.x - bw as i32,
                geom.y - bw as i32,
                geom.w + 2 * bw,
                geom.h + 2 * bw,
            );
            cw.transform = outer;
            cw.transform_radius = if corner_radius == 0 {
                0
            } else {
                corner_radius.min((outer.w / 2).min(outer.h / 2))
            };
            cw.transform_gen = gen;
        }
        // No `mark_full` here: a pure animation/scroll only moves windows, which
        // `compute_scene` records as `old ∪ new` animation damage (Fase 7) so the
        // buffer-age Partial path can scissor just the swept region. Forcing a
        // full repaint every animation frame would defeat partial redraw during
        // scroll. Structural changes (resize/restack/opacity/map/unmap) already
        // call `mark_full` through their own events, and the frame loop renders
        // while `animating` is true, so dropping this flag does not skip frames.
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
        }
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
        // Snapshot the stack order so we can mutate `self` (for a pending rebind)
        // while iterating without holding an immutable borrow of `self.stack`.
        let stack = self.stack.clone();
        for &win in &stack {
            // No `ignored` probe here: `track` refuses to record an ignored
            // window, so `wins` can never contain one and this lookup is the
            // filter. That is one hash per stack entry saved every frame.
            let needs_fixup = match self.wins.get(&win) {
                Some(cw) if !cw.mapped || cw.hidden || cw.occluded => false,
                Some(cw) => cw.tex.is_none() || cw.needs_rebind,
                None => false,
            };
            if needs_fixup {
                let has_pix = self.wins.get(&win).and_then(|cw| cw.pixmap).is_some();
                self.rename_and_bind(win, has_pix);
            }
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            if !cw.mapped || cw.hidden {
                continue;
            }
            if cw.occluded {
                continue;
            }
            let Some(tex) = cw.tex.as_mut() else {
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
            let moved = cw.prev_visual != Some(outer);
            if was_damaged || moved {
                let mut aout = [Rect::default(); 2];
                let n = anim_damage_rects(cw.prev_visual, outer, &mut aout);
                for &r in &aout[..n] {
                    self.frame_dirty.add(r);
                }
            }
            cw.prev_visual = Some(outer);
            // Cull windows that are entirely outside the screen. This is the
            // single biggest draw-time win: a 50-window ribbon only has ~5 on
            // screen at once; the rest are scrolled off the edges and would
            // otherwise each issue a `glDrawArrays` + texture bind for nothing.
            if CompWin::offscreen(outer, sw, sh) {
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
                opacity: cw.opacity,
                filter,
            };
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
        if self.stack_dirty {
            self.refresh_stack();
        }
        let t_frame_start = if self.trace {
            Some(Instant::now())
        } else {
            None
        };
        self.compute_scene();
        let t_build = t_frame_start.map(|t| t.elapsed().as_nanos() as u64);

        let (sw, sh) = (self.screen_w, self.screen_h);

        // Decide how much to repaint from the two damage facts plus the
        // buffer-age capability. `decide_redraw` is the single source of truth
        // for the full/partial/idle choice (unit-tested in `frameplan_tests`).
        // `force_full_redraw` (MAVERICK_FORCE_FULL_REDRAW) pretends buffer-age
        // is unavailable so the harness can exercise the full-redraw fallback.
        let has_age = self.renderer.has_buffer_age && !self.force_full_redraw;
        let t0 = Instant::now();
        let mut mode = decide_redraw(has_age, self.needs_full, !self.frame_dirty.is_empty());
        let decided_partial = mode == FrameMode::Partial;
        let mut observed_age: u32 = 0;

        // Honest partial: only trust the accumulated damage when the back buffer
        // is exactly one present stale (buffer-age == 1). With a double-buffered
        // fbconfig the age is usually 2, so the Partial path would otherwise
        // repaint content two presents old and leave ghosting (B3). Without a
        // trustworthy age we fall back to a full clear. Per `GLX_EXT_buffer_age`:
        // age 0 = undefined, age 1 = back buffer holds the *previous* frame (last
        // swap was a copy), age 2+ = back buffer is that many frames old (the
        // norm for double-buffered exchange). The single-frame `damage_acc` we
        // keep only matches age 1, so anything else is rejected as Full.
        if mode == FrameMode::Partial {
            observed_age = if has_age {
                self.renderer.back_buffer_age()
            } else {
                0
            };
            if observed_age == 1 {
                for r in &self.frame_dirty.rects[..self.frame_dirty.count] {
                    self.damage_acc.add(*r);
                }
                if self.damage_acc.needs_full {
                    mode = FrameMode::Full;
                }
            } else {
                mode = FrameMode::Full;
            }
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
        if let Some(shader) = self.wallpaper_shader {
            // Animated shader: one fill per output; `u_resolution` tells each shader
            // its own pixel size. Keeps requesting frames via `wallpaper_animating`.
            for out in &self.wallpaper_outputs {
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
            // Legacy root pixmap fallback (no native wallpaper configured).
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

        for item in &self.scene {
            // The texture is owned by `wins`; `draw_raw` takes the handle and the
            // quad's filter, and elides the `glBindTexture` when it matches
            // `last_tex` — exactly the bind-cache the `&Texture` path
            // kept on the texture, reconstructed from the scene.
            last_tex = self.renderer.draw_raw(item.tex, last_tex, &item.quad);
        }

        if matches!(mode, FrameMode::Partial) {
            self.renderer.clear_scissor();
        }

        self.renderer.end_frame();
        let swap_ns = t_frame_start.map(|t| t.elapsed().as_nanos() as u64);
        // The just-presented frame is now the committed back buffer, so the
        // accumulated damage describes only what changed since this present.
        // Clearing it each frame bounds the partial-redraw work and stops the
        // region from growing until it covers the whole screen (B4).
        self.damage_acc.clear();
        self.dirty = false;
        self.needs_full = false;
        self.dirty_reasons.clear();

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
                    self.trace_ns_interval_total += iv;
                    self.trace_ns_interval_max = self.trace_ns_interval_max.max(iv);
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
        let (w, h) = if cw.outer.w == 0 || cw.outer.h == 0 {
            (1u16, 1u16)
        } else {
            (cw.outer.w as u16, cw.outer.h as u16)
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
    let max = SUBSTEP_MS / 1000.0;
    let n = (dt / max).ceil().max(1.0) as usize;
    let step = dt / n as f32;
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
