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

use maverick_gl::{gl::*, Quad as GlQuad, Renderer, Texture, VisualFormat, XConn, XDisplay};

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
            mapped: false,
            hidden: false,
            format,
            transform: Rect::default(),
            transform_radius: 0,
            // 0 is never a live generation: `set_transforms` pre-increments, so
            // the first frame is generation 1.
            transform_gen: 0,
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
    // Carried so later phases (damage, partial redraw, debug overlays) can
    // attribute each submitted quad back to its window. Not read by `render`
    // yet, hence `allow`.
    #[allow(dead_code)]
    win: Window,
    quad: GlQuad,
    tex: GLuint,
    /// The texture's cached filter (`GL_LINEAR`/`GL_NEAREST`), carried out of
    /// `CompWin` so the draw path need not borrow the `Texture` back.
    filter: GLint,
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
    /// True while at least one frame is queued/needed.
    dirty: bool,
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
        let overlay = match conn
            .composite_get_overlay_window(root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(reply) => reply.overlay_win,
            None => {
                log::warn!("compositor: CompositeGetOverlayWindow failed");
                let _ = conn.composite_unredirect_subwindows(root, Redirect::MANUAL);
                let _ = conn.set_selection_owner(x11rb::NONE, cm_atom, x11rb::CURRENT_TIME);
                return None;
            }
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
        log::info!(
            "compositor: GL ready ({}; vsync {})",
            renderer.info,
            renderer.vsync
        );

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
            dirty: true,
            stack_dirty: true,
            stack: Vec::new(),
            scene: Vec::new(),
            frame_gen: 0,
            corner_radius: cfg.corner_radius,
            frame_dirty: DamageRegion::new(),
            needs_full: false,
        };

        comp.scan_existing();
        comp.refresh_wallpaper();
        comp.refresh_stack();
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

    /// Window appeared (CreateNotify). A freshly created window is placed on
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
        self.mark_full();
    }

    /// Window destroyed (DestroyNotify).
    pub fn on_destroy(&mut self, win: Window) {
        if let Some(cw) = self.wins.remove(&win) {
            self.release_texture(cw);
        }
        if let Some(dmg) = self.damages.remove(&win) {
            let _ = self.conn.damage_destroy(dmg);
        }
        stack_remove(&mut self.stack, win);
        self.mark_full();
    }

    /// Window mapped (MapNotify). Name its off-screen pixmap and bind a texture.
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
        self.rename_and_bind(win);
        if !self.ignored.contains(&win) && !self.stack.contains(&win) {
            self.stack_dirty = true;
        }
        self.mark_full();
    }

    /// Window unmapped (UnmapNotify). Drop the texture (the pixmap is gone).
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
        self.mark_full();
    }

    /// Mark a window hidden/shown by the WM's workspace switcher
    /// (`hide_offscreen`). A hidden window is never painted, so a window that
    /// belongs to a non-active workspace cannot briefly cover the active one
    /// while its off-screen `ConfigureNotify` is still in flight.
    pub fn set_hidden(&mut self, win: Window, hidden: bool) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.hidden = hidden;
            self.mark_full();
        }
    }

    /// Geometry change (ConfigureNotify for a tracked, non-root window).
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
            self.rename_and_bind(win);
        }
        self.mark_full();
    }

    /// Damage reported (DamageNotify). Re-arm and mark dirty; the texture is
    /// rebound right before drawing.
    pub fn on_damage(&mut self, win: Window) {
        if let Some(dmg) = self.damages.get(&win) {
            let _ = self.conn.damage_subtract(*dmg, x11rb::NONE, x11rb::NONE);
        }
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.damaged = true;
        }
        self.dirty = true;
    }

    /// `_NET_WM_WINDOW_OPACITY` changed (PropertyNotify).
    pub fn on_opacity(&mut self, win: Window, opacity: f32) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.opacity = opacity.clamp(0.0, 1.0);
        }
        self.mark_full();
    }

    /// Client changed its own X shape (ShapeNotify). We never clobber the
    /// client's shape with our own X Shape mask, so this is just a redraw
    /// hint; the SDF/vs shader path already handles corner rounding, and an
    /// arbitrary client shape is respected because we don't overwrite it.
    #[allow(dead_code)]
    pub fn on_shape(&mut self, win: Window) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.damaged = true;
        }
        self.mark_full();
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
        self.mark_full();
    }

    /// Mark the whole frame dirty (used when stacking or the wallpaper changes).
    pub fn invalidate(&mut self) {
        self.mark_full();
    }

    /// Pace the next frame to the vertical retrace (see `Renderer::wait_vblank`).
    /// Returns `false` when `GLX_SGI_video_sync` is unavailable so the loop can
    /// fall back to a short poll.
    pub fn wait_vblank(&mut self) -> bool {
        self.renderer.wait_vblank()
    }

    /// Whether a frame is still needed (an animation is running or a damage/
    /// configure/opacity event marked us dirty). Drives the event-loop's wake
    /// timeout so the compositor redraws as soon as something changed.
    pub fn needs_frame(&self) -> bool {
        self.dirty
    }

    /// The screen-space damage accumulated for the most recent `compute_scene`.
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
    fn mark_full(&mut self) {
        self.dirty = true;
        self.needs_full = true;
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
        for &win in &self.stack {
            // No `ignored` probe here: `track` refuses to record an ignored
            // window, so `wins` can never contain one and this lookup is the
            // filter. That is one hash per stack entry saved every frame.
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            if !cw.mapped || cw.hidden {
                continue;
            }
            let Some(tex) = cw.tex.as_mut() else {
                continue;
            };
            // Rebind the texture if the client repainted. The filter is part of
            // the texture's cached GL state, so read it out here (while we hold
            // the only borrow) and carry it in the DrawItem.
            let filter = tex.filter();
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
            let q = GlQuad {
                dst: [
                    outer.x as f32,
                    outer.y as f32,
                    (outer.x + outer.w as i32) as f32,
                    (outer.y + outer.h as i32) as f32,
                ],
                size: [outer.w as f32, outer.h as f32],
                radius: radius as f32,
                opacity: cw.opacity,
                smooth,
                ..Default::default()
            };
            items.push(DrawItem {
                win,
                quad: q,
                tex: tex.tex,
                filter,
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
        self.compute_scene();
        self.renderer.begin_frame(self.screen_w, self.screen_h);

        // Wallpaper first (so un-textured/transparent areas show it).
        let mut last_tex = 0;
        if let Some(wp) = self.wallpaper.as_mut() {
            self.renderer.bind(wp);
            last_tex = wp.tex;
            self.renderer.draw(
                wp,
                &GlQuad {
                    dst: [0.0, 0.0, self.screen_w as f32, self.screen_h as f32],
                    ..Default::default()
                },
            );
        }

        for item in &self.scene {
            // The texture is owned by `wins`; `draw_raw` takes the raw id and the
            // texture's cached filter, and elides the `glBindTexture` when it
            // matches `last_tex` — exactly the bind-cache the `&Texture` path
            // kept on the texture, reconstructed from the scene.
            last_tex = self
                .renderer
                .draw_raw(item.tex, item.filter, last_tex, &item.quad);
        }

        self.renderer.end_frame();
        self.dirty = false;
        self.needs_full = false;
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
                self.mark_full();
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
                self.track(win);
            }
        }
    }

    /// Name the window's off-screen pixmap and wrap it as a GL texture.
    fn rename_and_bind(&mut self, win: Window) {
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
        let Ok(pixmap) = self.conn.generate_id() else {
            return;
        };
        if self.conn.composite_name_window_pixmap(win, pixmap).is_err() {
            return;
        }
        match self.renderer.texture_from_pixmap(pixmap, format, w, h) {
            Ok(t) => {
                if let Some(cw) = self.wins.get_mut(&win) {
                    cw.tex = Some(t);
                    cw.pixmap = Some(pixmap);
                    cw.damaged = false;
                } else {
                    // The window vanished while we were binding.
                    self.renderer.destroy_texture(t);
                    let _ = self.conn.free_pixmap(pixmap);
                }
            }
            Err(e) => {
                let _ = self.conn.free_pixmap(pixmap);
                // Once per visual: this is the difference between "that app
                // draws nothing" and "the compositor cannot represent that
                // app's pixel format on this screen".
                if self.warned_visuals.insert(format.id) {
                    log::warn!("compositor: cannot texture windows of {format}: {e}");
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
        Some(sib) => match stack.iter().position(|&w| w == sib) {
            Some(i) => i + 1,
            None => {
                // Drop any stale entry so the resync starts from a consistent
                // state rather than a duplicate.
                stack.retain(|&w| w != win);
                return false;
            }
        },
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

/// Forget `win` entirely (DestroyNotify).
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
        assert!(!CompWin::offscreen(Rect::new((W as i32) - 60, 100, 400, 300), W, H));
        // Fully to the left, beyond the margin.
        assert!(CompWin::offscreen(Rect::new(-200, 100, 100, 300), W, H));
        // Fully below.
        assert!(CompWin::offscreen(Rect::new(100, (H as i32) + 200, 100, 300), W, H));
        // Entirely past the right edge.
        assert!(CompWin::offscreen(Rect::new((W as i32) + 100, 100, 100, 300), W, H));
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
            assert_eq!(stack, vec![A, B, C], "re-applying the same order is a no-op");
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
        assert_eq!(sorted.len(), stack.len(), "no window may appear twice: {stack:?}");
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
    use super::DamageRegion;
    use crate::types::Rect;

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
    use super::live_placements;
    use crate::config::Cfg;
    use crate::core::framebench::CountAllocs;
    use crate::core::layout::{LayoutRegistry, Placements, RibbonScratch};
    use crate::types::{Client, Column, Focus, Monitor, Rect, State, WindowId};

    /// Build a one-monitor state with `n` single-window columns on a 1920x1080
    /// monitor, camera mid-animation (the only state in which this path runs).
    fn ribbon(n: u32) -> State {
        let mut state = State::new();
        state.monitors.push(Monitor::new(Rect::new(0, 0, 1920, 1080), 1));
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
            live_placements(state, 0, &cfg, &registry, &mut out, &mut raise, &mut scratch);
        }
        // Two rounds: one timed, one counted (the counter only runs while armed).
        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            live_placements(state, 0, &cfg, &registry, &mut out, &mut raise, &mut scratch);
        }
        let elapsed = t0.elapsed().as_nanos() as f64 / iters as f64;

        let counter = CountAllocs::start();
        for _ in 0..iters {
            live_placements(state, 0, &cfg, &registry, &mut out, &mut raise, &mut scratch);
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
            eprintln!("  N={n:>5}  {:.1} ns/frame", ns);
        }
    }
}

