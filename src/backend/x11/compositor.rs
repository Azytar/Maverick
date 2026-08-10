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

use maverick_gl::{Quad as GlQuad, Renderer, Texture, VisualFormat, XConn, XDisplay};

use crate::log;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::damage::{ConnectionExt as _, Damage, ReportLevel};
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::*;

use crate::config::Cfg;
use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements};
use crate::core::present::present;
use crate::types::{Rect, State};

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
        }
    }
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
    /// Per-frame transforms for *managed* windows: window → outer rect (live) +
    /// corner radius. Anything not present falls back to its X geometry.
    transforms: Vec<(Window, Rect, u32)>,
    /// Corner radius the WM wants applied (shader SDF), px.
    corner_radius: u32,
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
            transforms: Vec::new(),
            corner_radius: cfg.corner_radius,
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
        self.dirty = true;
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
        self.dirty = true;
    }

    /// Window mapped (MapNotify). Name its off-screen pixmap and bind a texture.
    ///
    /// Mapping does **not** restack in X — an unmapped window keeps its place
    /// in the sibling order — so this deliberately does not touch `stack`. It
    /// only asks for a resync when the window is missing entirely, which means
    /// we never saw its `CreateNotify`.
    pub fn on_map(&mut self, win: Window) {
        if self.wins.get(&win).is_none() {
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
        self.dirty = true;
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
        self.dirty = true;
    }

    /// Mark a window hidden/shown by the WM's workspace switcher
    /// (`hide_offscreen`). A hidden window is never painted, so a window that
    /// belongs to a non-active workspace cannot briefly cover the active one
    /// while its off-screen `ConfigureNotify` is still in flight.
    pub fn set_hidden(&mut self, win: Window, hidden: bool) {
        if let Some(cw) = self.wins.get_mut(&win) {
            cw.hidden = hidden;
            self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
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
        self.dirty = true;
    }

    /// The WM computed live placements; hand them to the compositor as the
    /// per-window transform (outer rect + corner radius) for this frame.
    pub fn set_transforms(&mut self, placements: &[(Window, Rect, u32)]) {
        self.transforms.clear();
        for &(win, geom, bw) in placements {
            if self.ignored.contains(&win) {
                continue;
            }
            // Outer rect = content + borders on every side.
            let outer = Rect::new(
                geom.x - bw as i32,
                geom.y - bw as i32,
                geom.w + 2 * bw,
                geom.h + 2 * bw,
            );
            let radius = if self.corner_radius == 0 {
                0
            } else {
                self.corner_radius.min((outer.w / 2).min(outer.h / 2))
            };
            self.transforms.push((win, outer, radius));
        }
        self.dirty = true;
    }

    /// Mark the whole frame dirty (used when stacking or the wallpaper changes).
    pub fn invalidate(&mut self) {
        self.dirty = true;
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

    // ── frame ───────────────────────────────────────────────────────────────

    /// Render one frame: wallpaper, then every window bottom→top. Blocks to
    /// vsync via the swap (when `vsync` is on). Returns `false` on a GL error
    /// so the caller can disable the compositor.
    pub fn render(&mut self) -> bool {
        if self.stack_dirty {
            self.refresh_stack();
        }
        self.renderer.begin_frame(self.screen_w, self.screen_h);

        // Wallpaper first (so un-textured/transparent areas show it).
        if let Some(wp) = self.wallpaper.as_mut() {
            self.renderer.bind(wp);
            self.renderer.draw(
                wp,
                &GlQuad {
                    dst: [0.0, 0.0, self.screen_w as f32, self.screen_h as f32],
                    ..Default::default()
                },
            );
        }

        for &win in &self.stack {
            if self.ignored.contains(&win) {
                continue;
            }
            let Some(cw) = self.wins.get_mut(&win) else {
                continue;
            };
            if !cw.mapped || cw.hidden {
                continue;
            }
            let Some(tex) = cw.tex.as_mut() else {
                continue;
            };
            // Rebind the texture if the client repainted.
            if cw.damaged {
                self.renderer.bind(tex);
                cw.damaged = false;
            }
            // Live outer rect, or fall back to the X geometry (OR windows).
            let (outer, radius) = self
                .transforms
                .iter()
                .find(|(w, _, _)| *w == win)
                .map(|(_, r, rad)| (*r, *rad))
                .unwrap_or((cw.outer, 0));
            if outer.w == 0 || outer.h == 0 {
                continue;
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
            self.renderer.draw(tex, &q);
        }

        self.renderer.end_frame();
        self.dirty = false;
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
                self.dirty = true;
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
) {
    let mut buf = Vec::with_capacity(out.capacity());
    arrange(state, mon_idx, cfg, registry, Phase::Live, &mut buf);
    let mon = &state.monitors[mon_idx];
    present(state, mon, &mut buf);
    out.clear();
    out.extend(buf.drain(..));
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
    use super::{stack_add_top, stack_remove, stack_restack};

    const A: u32 = 0xA;
    const B: u32 = 0xB;
    const C: u32 = 0xC;

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
