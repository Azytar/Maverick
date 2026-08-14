// maverick-gl/src/renderer.rs
// The GPU side of Maverick's compositor: one GLX context on the Composite
// overlay window, one shader program, one unit quad.
//
// Everything here is deliberately tiny. A compositor for a tiling WM never has
// to blend hundreds of layers — it draws the wallpaper plus at most a few dozen
// window textures, each one a single `glDrawArrays` of 6 vertices. The cost
// that matters is the *X traffic we no longer generate*: with the window
// textures living on the GPU, an animation frame is a transform on a uniform
// instead of a `ConfigureWindow` per window.
//
// Alpha convention: **premultiplied**, because that is what X Render and
// Composite produce and what `GLX_EXT_texture_from_pixmap` hands us. The blend
// func is therefore `(ONE, ONE_MINUS_SRC_ALPHA)` and the fragment shader scales
// the whole `vec4` (rgb *and* a) by coverage, never just the alpha.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::fmt;
use std::os::raw::{c_int, c_uint, c_ulong};

use maverick_img::Rgba8;

/// Screen-space rectangle in pixels, owned by `maverick-gl`. The compositor
/// converts its `crate::types::Rect` into this when handing the renderer a
/// wallpaper output quad — `maverick-gl` must not depend on the main crate's
/// geometry type.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

use crate::dl::Lib;
use crate::gl::*;
use crate::glx::*;
use crate::xlib::{XDisplay, XID};

const VERTEX_SRC: &str = r#"#version 330 core
layout(location = 0) in vec2 a_pos;   // unit quad, 0..1
uniform vec4 u_dst;   // destination rect in pixels: x0,y0,x1,y1 (origin top-left)
uniform vec4 u_src;   // source rect in texture coords 0..1, y measured top-down
uniform vec2 u_res;   // viewport size in pixels
uniform float u_flip; // 1.0 when the fbconfig reports GLX_Y_INVERTED_EXT
out vec2 v_tex;
out vec2 v_uv;
void main() {
    vec2 p = mix(u_dst.xy, u_dst.zw, a_pos);
    vec2 clip = vec2(p.x / u_res.x * 2.0 - 1.0, 1.0 - p.y / u_res.y * 2.0);
    gl_Position = vec4(clip, 0.0, 1.0);
    vec2 t = mix(u_src.xy, u_src.zw, a_pos);
    v_tex = vec2(t.x, mix(t.y, 1.0 - t.y, u_flip));
    v_uv = a_pos;
}
"#;

const FRAGMENT_SRC: &str = r#"#version 330 core
in vec2 v_tex;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform float u_opacity;  // _NET_WM_WINDOW_OPACITY, 0..1
uniform float u_radius;   // WM corner_radius in px, 0 disables the whole branch
uniform vec2  u_size;     // quad size in px, for the SDF
out vec4 frag;

// Signed distance to a rounded box centred on the origin.
float sd_round_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec4 src = texture(u_tex, v_tex);  // premultiplied (X Render convention)
    float a = u_opacity;
    if (u_radius > 0.0) {
        vec2 p = v_uv * u_size - u_size * 0.5;
        float d = sd_round_box(p, u_size * 0.5, u_radius);
        // 1px analytic antialiasing across the corner edge.
        a *= 1.0 - smoothstep(-1.0, 1.0, d);
    }
    // Scaling the whole vec4 keeps the result premultiplied.
    frag = src * a;
}
"#;

/// A window (or pixmap) bound as an OpenGL texture through
/// `GLX_EXT_texture_from_pixmap`.
///
/// Not `Drop`: freeing it needs the `Display*` and a current GL context, so the
/// owner must hand it back to [`Renderer::destroy_texture`]. The compositor
/// does that from exactly three places (unmap, destroy, resize).
pub struct Texture {
    pub glx_pixmap: GLXPixmap,
    pub tex: GLuint,
    /// `true` when the fbconfig reports `GLX_Y_INVERTED_EXT`, i.e. the texture's
    /// row 0 is the *bottom* of the window. Not optional: GLX pixmaps coming
    /// from redirected windows are y-flipped relative to plain GL textures on
    /// most drivers, and guessing gets you upside-down windows.
    pub flip: bool,
    pub width: u16,
    pub height: u16,
    /// Whether `glXBindTexImageEXT` is currently in effect. The TFP spec says
    /// the texture contents are *undefined* while the client renders into the
    /// drawable, so every damaged frame does release → bind.
    bound: bool,
    /// The `GL_TEXTURE_MIN_FILTER`/`MAG_FILTER` currently set on this texture
    /// object.
    ///
    /// Filtering is *texture* state, not draw state, so it survives between
    /// frames — but it depends on `Quad::smooth`, which changes when a window
    /// starts or stops being scaled by an animation. Caching the value here is
    /// what lets `draw` re-issue `glTexParameteri` only on that transition
    /// instead of twice per quad per frame.
    filter: GLint,
}

impl Texture {
    /// The texture's cached min/mag filter (set by `draw`/`draw_raw`). Exposed so
    /// callers that submit a quad via a raw id (the compositor's explicit scene
    /// path) can carry the value out without borrowing the `Texture`.
    pub fn filter(&self) -> GLint {
        self.filter
    }
    /// Construct a `Texture` that owns a raw GL texture id uploaded from CPU pixels
    /// (not a GLX pixmap). `glx_pixmap` is left 0 so `destroy_texture` never tries
    /// to release an X pixmap that does not exist. `flip` is `false` because CPU
    /// image data is already top-down.
    pub fn new_cpu(tex: GLuint, w: u16, h: u16) -> Self {
        Texture {
            glx_pixmap: 0,
            tex,
            flip: false,
            width: w,
            height: h,
            bound: false,
            filter: GL_LINEAR,
        }
    }
}

impl Texture {
    #[inline]
    pub fn is_bound(&self) -> bool {
        self.bound
    }
}

/// One textured quad to draw this frame.
#[derive(Debug, Clone, Copy)]
pub struct Quad {
    /// Destination rect in screen pixels: `x0, y0, x1, y1`, origin top-left.
    pub dst: [f32; 4],
    /// Source rect in normalised texture coords: `u0, v0, u1, v1`, `v` top-down.
    /// `[0.0, 0.0, 1.0, 1.0]` for a whole window.
    pub src: [f32; 4],
    /// Quad size in pixels — what the rounded-rect SDF measures against.
    pub size: [f32; 2],
    /// Corner radius in pixels; `0.0` takes the fast path (no SDF at all).
    pub radius: f32,
    /// 0..1 multiplier applied to the premultiplied source.
    pub opacity: f32,
    /// `true` → `GL_LINEAR` (the quad is scaled by an animation),
    /// `false` → `GL_NEAREST` (1:1, so nearest is both sharper and cheaper).
    pub smooth: bool,
}

impl Default for Quad {
    fn default() -> Self {
        Self {
            dst: [0.0; 4],
            src: [0.0, 0.0, 1.0, 1.0],
            size: [1.0, 1.0],
            radius: 0.0,
            opacity: 1.0,
            smooth: false,
        }
    }
}

/// One X visual exactly as the server describes it — the only ground truth
/// about how much colour this screen actually has.
///
/// The compositor builds this table from the X `Setup` (`allowed_depths`) and
/// hands it to the renderer, because **GLX on its own cannot answer "does this
/// fbconfig fit that pixmap?"**. The tempting attribute, `GLX_BUFFER_SIZE`, is
/// the *fbconfig's* colour-buffer width (R+G+B+A of the GPU format), not the X
/// drawable depth: virtually every driver reports 32 for the fbconfig attached
/// to a depth-**24** visual, because the 24-bit visual is stored as `x8r8g8b8`.
/// Comparing a pixmap's depth against `GLX_BUFFER_SIZE` therefore rejects the
/// one config that would have worked, on the one configuration everybody runs.
///
/// The reliable link is `GLX_VISUAL_ID` → this table → `depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualFormat {
    /// X visual id.
    pub id: u32,
    /// Bits per pixel the *server* stores for this visual (24, 32, 16, 30, ...).
    pub depth: u8,
    pub red_bits: u8,
    pub green_bits: u8,
    pub blue_bits: u8,
    /// `depth - (r+g+b)`: 8 for an ARGB32 visual, 0 for the usual RGB24 one.
    pub alpha_bits: u8,
    /// TrueColor or DirectColor. Anything else (PseudoColor, GrayScale, ...)
    /// is a palette visual, which `GLX_EXT_texture_from_pixmap` cannot sample —
    /// such windows are reported and skipped rather than drawn in fantasy
    /// colours.
    pub direct: bool,
}

impl VisualFormat {
    #[inline]
    pub fn has_alpha(self) -> bool {
        self.alpha_bits > 0
    }

    /// Bits of colour this visual can actually show. A screen cannot display
    /// more than this no matter what the client renders.
    #[inline]
    pub fn color_bits(self) -> u32 {
        self.red_bits as u32 + self.green_bits as u32 + self.blue_bits as u32
    }
}

impl fmt::Display for VisualFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "visual 0x{:x} depth {} R{}G{}B{}A{}{}",
            self.id,
            self.depth,
            self.red_bits,
            self.green_bits,
            self.blue_bits,
            self.alpha_bits,
            if self.direct { "" } else { " (palette)" }
        )
    }
}

/// An fbconfig usable as a texture source for one particular X visual.
#[derive(Clone, Copy)]
struct TfpConfig {
    cfg: GLXFBConfig,
    /// `GLX_TEXTURE_FORMAT_RGB_EXT` or `GLX_TEXTURE_FORMAT_RGBA_EXT`.
    format: c_int,
    flip: bool,
    // ── kept for the startup report; never read by the draw path ──
    /// The fbconfig's own `GLX_VISUAL_ID` (0 when it has no X visual).
    visual: u32,
    buffer_size: c_int,
    rgba: [c_int; 4],
    /// Raw `GLX_Y_INVERTED_EXT`, which is *not* always 0/1 in the wild.
    y_inverted: Option<c_int>,
}

impl fmt::Display for TfpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fbconfig visual 0x{:x} buffer {} R{}G{}B{}A{} {} y_inverted {} -> flip {}",
            self.visual,
            self.buffer_size,
            self.rgba[0],
            self.rgba[1],
            self.rgba[2],
            self.rgba[3],
            if self.format == GLX_TEXTURE_FORMAT_RGBA_EXT {
                "RGBA"
            } else {
                "RGB"
            },
            match self.y_inverted {
                Some(v) => v.to_string(),
                None => "unsupported".into(),
            },
            self.flip
        )
    }
}

/// Whether one of the screen's visuals can be composited, and through what.
/// Produced by [`Renderer::format_report`].
pub struct VisualReport {
    pub format: VisualFormat,
    /// `Ok` describes the fbconfig chosen; `Err` says why nothing fits.
    pub binding: Result<String, String>,
}

/// The only attributes of a GLX fbconfig the texture-from-pixmap choice
/// depends on, lifted out of GLX so the decision itself is a pure function
/// (see [`rate_fbconfig`]) that can be unit-tested without an X server.
#[derive(Clone, Copy, Debug)]
struct FbAttrs {
    /// `GLX_VISUAL_ID`, or 0 for a pixmap-only config with no X visual.
    visual: u32,
    /// Depth of that visual according to the X `Setup`; `None` when the config
    /// has no X visual to clash with.
    visual_depth: Option<u8>,
    pixmap_renderable: bool,
    rgba_render: bool,
    /// `GLX_RED_SIZE` / `GREEN` / `BLUE` / `ALPHA`.
    rgba: [c_int; 4],
    buffer_size: c_int,
    bind_rgb: bool,
    bind_rgba: bool,
    target_2d: bool,
    caveat_free: bool,
    /// Raw `GLX_Y_INVERTED_EXT`; `None` when the server does not answer.
    y_inverted: Option<c_int>,
}

/// Why an fbconfig was turned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reject {
    NotPixmap,
    NotRgba,
    TooFewBits,
    NoAlpha,
    DepthMismatch,
    NotBindable,
    No2dTarget,
}

/// Score `fb` as a texture source for a pixmap of visual `want`, or say why it
/// cannot be one. Higher is better; only the relative order matters.
///
/// This is where the compositor is made *screen-aware*, and where it used to be
/// wrong. The rules, and the bug each one prevents:
///
///   * **Depth comes from the X visual table, never from `GLX_BUFFER_SIZE`.**
///     A depth-24 visual is stored as `x8r8g8b8`, so its fbconfig reports a
///     32-bit buffer with 8 alpha bits. Requiring `buffer_size == depth` finds
///     nothing on such a driver and *every ordinary window silently vanishes
///     from the frame*.
///   * **Channel widths must be at least the visual's, and are ranked on how
///     exactly they match.** `buffer_size == 32 && alpha != 0` also matches
///     `R10G10B10A2`, and binding an 8-bit-per-channel ARGB pixmap through a
///     10-bit config reinterprets the bits across channel boundaries: orange
///     `(255,128,64)` comes back as `(255,247,16)`. That is the colour bug.
///   * **Alpha bits on the *config* are not alpha in the *visual*.** For a
///     depth-24 visual we ask for `GLX_TEXTURE_FORMAT_RGB_EXT` and the TFP spec
///     guarantees the sampler returns `a = 1.0` whatever the config carries, so
///     rejecting configs that merely *have* an alpha channel is what left
///     24-bit windows unbindable on the drivers that only expose 32-bit ones.
///   * **Never fewer colour bits than the visual.** A narrower config would
///     quantise every window: banding and posterised gradients. Wider is fine —
///     the driver widens the value, it does not invent one.
fn rate_fbconfig(want: VisualFormat, fb: &FbAttrs) -> Result<i32, Reject> {
    if !fb.pixmap_renderable {
        return Err(Reject::NotPixmap);
    }
    // A colour-index config would hand the shader palette indices.
    if !fb.rgba_render {
        return Err(Reject::NotRgba);
    }
    let [r, g, b, a] = fb.rgba;
    if r < c_int::from(want.red_bits)
        || g < c_int::from(want.green_bits)
        || b < c_int::from(want.blue_bits)
    {
        return Err(Reject::TooFewBits);
    }
    let want_alpha = want.has_alpha();
    if want_alpha && a < c_int::from(want.alpha_bits) {
        return Err(Reject::NoAlpha);
    }
    // The fbconfig's own visual is what X compares the pixmap against; a
    // mismatch is `BadMatch` from `glXCreatePixmap`.
    if let Some(depth) = fb.visual_depth {
        if depth != want.depth {
            return Err(Reject::DepthMismatch);
        }
    }
    if want_alpha && !fb.bind_rgba {
        return Err(Reject::NotBindable);
    }
    if !want_alpha && !fb.bind_rgb {
        return Err(Reject::NotBindable);
    }
    if !fb.target_2d {
        return Err(Reject::No2dTarget);
    }

    // Exact visual first, then same depth, then the tightest channel fit, then
    // no rendering caveat (slow/non-conformant paths exist on some drivers).
    // A worse config is still better than no compositing at all.
    let mut score = 0;
    if fb.visual == want.id {
        score += 100;
    }
    if fb.visual_depth == Some(want.depth) {
        score += 50;
    }
    if r == c_int::from(want.red_bits)
        && g == c_int::from(want.green_bits)
        && b == c_int::from(want.blue_bits)
    {
        score += 20;
    }
    if a == c_int::from(want.alpha_bits) {
        score += 10;
    }
    if fb.caveat_free {
        score += 5;
    }
    Ok(score)
}

/// Tally of *why* fbconfigs were turned down, so a failure says something more
/// useful than "no fbconfig".
#[derive(Default)]
struct Rejects {
    not_pixmap: usize,
    not_rgba: usize,
    too_few_bits: usize,
    no_alpha: usize,
    depth_mismatch: usize,
    not_bindable: usize,
    no_2d_target: usize,
}

impl Rejects {
    fn note(&mut self, r: Reject) {
        let slot = match r {
            Reject::NotPixmap => &mut self.not_pixmap,
            Reject::NotRgba => &mut self.not_rgba,
            Reject::TooFewBits => &mut self.too_few_bits,
            Reject::NoAlpha => &mut self.no_alpha,
            Reject::DepthMismatch => &mut self.depth_mismatch,
            Reject::NotBindable => &mut self.not_bindable,
            Reject::No2dTarget => &mut self.no_2d_target,
        };
        *slot += 1;
    }
}

impl fmt::Display for Rejects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (n, label) in [
            (self.not_pixmap, "not pixmap-renderable"),
            (self.not_rgba, "not RGBA"),
            (self.too_few_bits, "fewer colour bits than the visual"),
            (self.no_alpha, "no alpha channel"),
            (self.depth_mismatch, "wrong visual depth"),
            (self.not_bindable, "not bindable as a texture"),
            (self.no_2d_target, "no GL_TEXTURE_2D target"),
        ] {
            if n == 0 {
                continue;
            }
            if !first {
                f.write_str(", ")?;
            }
            write!(f, "{n} {label}")?;
            first = false;
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}

pub struct Renderer {
    dpy: XDisplay,
    screen: c_int,
    #[allow(dead_code)]
    lib: Lib,
    gl: Gl,
    glx: Glx,
    ctx: GLXContext,
    glx_win: GLXWindow,
    prog: GLuint,
    vao: GLuint,
    vbo: GLuint,
    u_dst: GLint,
    u_src: GLint,
    u_res: GLint,
    u_flip: GLint,
    u_tex: GLint,
    u_opacity: GLint,
    u_radius: GLint,
    u_size: GLint,
    /// Wallpaper shader program (user fragment shader + our unit-quad vertex
    /// shader). `0` when no shader wallpaper is active. Separate from `prog` so
    /// the window path is untouched.
    wp_prog: GLuint,
    wp_u_dst: GLint,
    wp_u_res: GLint,
    wp_u_time: GLint,
    wp_u_resolution: GLint,
    wp_u_delta_time: GLint,
    /// Every visual the screen advertises, straight from the X `Setup`. This is
    /// what makes the renderer *screen-aware*: no depth or channel width is
    /// ever assumed, they are all read back from the server.
    visuals: Vec<VisualFormat>,
    /// The overlay/root visual — what the final framebuffer can actually show.
    root_format: VisualFormat,
    /// Lazily resolved fbconfig per **visual id** (not per depth: two visuals
    /// can share a depth, and a 30-bit deep-colour visual must not silently get
    /// the 32-bit config — `glXCreatePixmap` would answer `BadMatch` and the
    /// window would go black or take on the neighbouring visual's channel
    /// layout).
    tfp_cache: HashMap<u32, Result<TfpConfig, String>>,
    /// Visuals whose first `glXCreatePixmap` has already been round-tripped and
    /// checked for `BadMatch`. Only the first pixmap of each visual pays for
    /// the sync.
    verified: HashSet<u32>,
    /// Last texture bound via `draw`, to skip redundant `glBindTexture`.
    last_tex: GLuint,
    /// Current viewport size (set by `begin_frame`), reused by `draw_shader`'s
    /// vertex transform (clip space needs the full screen resolution).
    screen_w: u32,
    screen_h: u32,
    /// Whether vsync (swap interval 1) is actually in effect.
    pub vsync: bool,
    /// Whether `GLX_SGI_video_sync` is available. Retained purely as an
    /// instrumentation signal (C1): its counter can measure missed vblanks. It
    /// no longer drives pacing — swap interval 1 (see `vsync`) is the only
    /// synchroniser.
    pub video_sync: bool,
    /// `GL_VENDOR / GL_RENDERER / GL_VERSION`, for the startup log line.
    pub info: String,
    /// Whether `GLX_EXT_buffer_age` is present and `glXQueryDrawable` is
    /// resolvable. When true, the compositor can do safe partial redraws
    /// (scissor to the damage region) instead of clearing the whole screen.
    pub has_buffer_age: bool,
}

impl Renderer {
    /// Bring up GL on the Composite overlay window.
    ///
    /// `overlay` must already exist and use `root_visual` (which is what
    /// `CompositeGetOverlayWindow` guarantees). `visuals` is the screen's whole
    /// visual table as reported by the X `Setup`; the renderer refuses to guess
    /// anything about colour depth that is not in there. Every failure path
    /// returns `Err` with a human-readable reason — the caller logs it and
    /// stays on the non-composited path instead of dying.
    pub fn new(
        dpy: XDisplay,
        screen: i32,
        overlay: u32,
        root_visual: u32,
        visuals: &[VisualFormat],
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let lib = Lib::open_gl()?;
        let glx = Glx::load(&lib)?;
        let d = dpy.as_ptr();
        let screen = screen as c_int;

        let root_format = visuals
            .iter()
            .copied()
            .find(|v| v.id == root_visual)
            .ok_or_else(|| {
                format!("root visual 0x{root_visual:x} is not in the screen's visual table")
            })?;
        if !root_format.direct {
            return Err(format!(
                "root {root_format} is a palette visual; GLX cannot composite it"
            ));
        }

        // ── GLX availability ────────────────────────────────────────────────
        let (mut eb, mut ev) = (0, 0);
        if unsafe { (glx.glXQueryExtension)(d, &mut eb, &mut ev) } == 0 {
            return Err("server has no GLX extension".into());
        }
        let (mut maj, mut min) = (0, 0);
        unsafe { (glx.glXQueryVersion)(d, &mut maj, &mut min) };
        if maj < 1 || (maj == 1 && min < 3) {
            return Err(format!(
                "GLX {maj}.{min} is too old (need 1.3 for fbconfigs)"
            ));
        }

        let exts = glx.extensions(d, screen);
        for required in [
            "GLX_EXT_texture_from_pixmap",
            "GLX_ARB_create_context",
            "GLX_ARB_create_context_profile",
        ] {
            if !has_extension(&exts, required) {
                return Err(format!("missing GLX extension {required}"));
            }
        }
        if glx.glXBindTexImageEXT.is_none() || glx.glXReleaseTexImageEXT.is_none() {
            return Err("libGL exports no glXBind/ReleaseTexImageEXT".into());
        }
        let create_ctx = glx
            .glXCreateContextAttribsARB
            .ok_or("libGL exports no glXCreateContextAttribsARB")?;

        // ── fbconfig for the overlay window: must use the ROOT visual ───────
        let win_cfg = choose_window_fbconfig(&glx, d, screen, root_format)?;

        // ── OpenGL 3.3 core context ─────────────────────────────────────────
        let ctx_attribs: [c_int; 7] = [
            GLX_CONTEXT_MAJOR_VERSION_ARB,
            3,
            GLX_CONTEXT_MINOR_VERSION_ARB,
            3,
            GLX_CONTEXT_PROFILE_MASK_ARB,
            GLX_CONTEXT_CORE_PROFILE_BIT_ARB,
            0,
        ];
        let ctx = unsafe { create_ctx(d, win_cfg, std::ptr::null_mut(), 1, ctx_attribs.as_ptr()) };
        // The context request is asynchronous; sync so a GLXBadFBConfig has
        // landed (and been swallowed by our silent handler) before we test.
        dpy.sync();
        if ctx.is_null() {
            return Err("glXCreateContextAttribsARB(3.3 core) failed".into());
        }

        let glx_win =
            unsafe { (glx.glXCreateWindow)(d, win_cfg, c_ulong::from(overlay), std::ptr::null()) };
        dpy.sync();
        if glx_win == 0 {
            unsafe { (glx.glXDestroyContext)(d, ctx) };
            return Err("glXCreateWindow(overlay) failed".into());
        }

        if unsafe { (glx.glXMakeCurrent)(d, glx_win, ctx) } == 0 {
            unsafe {
                (glx.glXDestroyWindow)(d, glx_win);
                (glx.glXDestroyContext)(d, ctx);
            }
            return Err("glXMakeCurrent(overlay) failed".into());
        }

        let gl = match Gl::load(&lib) {
            Ok(g) => g,
            Err(e) => {
                unsafe {
                    (glx.glXMakeCurrent)(d, 0, std::ptr::null_mut());
                    (glx.glXDestroyWindow)(d, glx_win);
                    (glx.glXDestroyContext)(d, ctx);
                }
                return Err(e);
            }
        };

        // ── vsync ───────────────────────────────────────────────────────────
        // `glXSwapBuffers` with swap interval 1 blocks until the vertical blank,
        // so a frame lands exactly once per refresh — no tearing on the moving
        // edge, and the loop paces itself for free (no spinning, no 16 ms guess).
        // This is the *single* synchroniser: nothing else must set a conflicting
        // interval, or the loop would skip vblanks (B1).
        let vsync = enable_vsync(&glx, d, screen, glx_win, &exts);

        // `GLX_SGI_video_sync` is kept purely as an instrumentation signal (C1):
        // its counter can measure missed vblanks. It no longer drives pacing —
        // the swap-interval-1 path above is the only synchroniser, so we must
        // NOT zero the interval here (that used to undo `enable_vsync`).
        let video_sync = has_extension(&exts, "GLX_SGI_video_sync");

        let has_buffer_age =
            has_extension(&exts, "GLX_EXT_buffer_age") && glx.glXQueryDrawable.is_some();

        let mut r = Renderer {
            dpy,
            screen,
            lib,
            gl,
            glx,
            ctx,
            glx_win,
            prog: 0,
            vao: 0,
            vbo: 0,
            u_dst: -1,
            u_src: -1,
            u_res: -1,
            u_flip: -1,
            u_tex: -1,
            u_opacity: -1,
            u_radius: -1,
            u_size: -1,
            wp_prog: 0,
            wp_u_dst: -1,
            wp_u_res: -1,
            wp_u_time: -1,
            wp_u_resolution: -1,
            wp_u_delta_time: -1,
            visuals: visuals.to_vec(),
            root_format,
            tfp_cache: HashMap::new(),
            verified: HashSet::new(),
            last_tex: 0,
            vsync,
            video_sync,
            info: String::new(),
            has_buffer_age,
            screen_w: 0,
            screen_h: 0,
        };

        if let Err(e) = r.init_gl_objects() {
            r.destroy();
            return Err(e);
        }

        r.info = format!(
            "{} / {} / GL {}",
            r.gl.get_string(GL_VENDOR),
            r.gl.get_string(GL_RENDERER),
            r.gl.get_string(GL_VERSION)
        );
        let _ = (width, height);
        Ok(r)
    }

    fn init_gl_objects(&mut self) -> Result<(), String> {
        let gl = &self.gl;
        let vs = compile_shader(gl, GL_VERTEX_SHADER, VERTEX_SRC)?;
        let fs = match compile_shader(gl, GL_FRAGMENT_SHADER, FRAGMENT_SRC) {
            Ok(f) => f,
            Err(e) => {
                unsafe { (gl.glDeleteShader)(vs) };
                return Err(e);
            }
        };
        let prog = unsafe { (gl.glCreateProgram)() };
        unsafe {
            (gl.glAttachShader)(prog, vs);
            (gl.glAttachShader)(prog, fs);
            (gl.glLinkProgram)(prog);
            (gl.glDeleteShader)(vs);
            (gl.glDeleteShader)(fs);
        }
        let mut ok: GLint = 0;
        unsafe { (gl.glGetProgramiv)(prog, GL_LINK_STATUS, &mut ok) };
        if ok == 0 {
            let log = program_log(gl, prog);
            unsafe { (gl.glDeleteProgram)(prog) };
            return Err(format!("shader link failed: {log}"));
        }
        self.prog = prog;

        let uniform = |name: &str| -> GLint {
            let c = CString::new(name).expect("static uniform name has no NUL");
            unsafe { (gl.glGetUniformLocation)(prog, c.as_ptr()) }
        };
        self.u_dst = uniform("u_dst");
        self.u_src = uniform("u_src");
        self.u_res = uniform("u_res");
        self.u_flip = uniform("u_flip");
        self.u_tex = uniform("u_tex");
        self.u_opacity = uniform("u_opacity");
        self.u_radius = uniform("u_radius");
        self.u_size = uniform("u_size");

        // The wallpaper shader program reuses the same unit-quad vertex shader as
        // the window program, so a user fragment shader only has to declare the
        // fixed contract uniforms (`u_time`, `u_resolution`, `u_delta_time`) plus
        // `out vec4 frag`. It is compiled lazily per shader file in
        // `compile_fragment`; here we just initialise its uniform slots to "absent".
        self.wp_prog = 0;
        self.wp_u_dst = -1;
        self.wp_u_res = -1;
        self.wp_u_time = -1;
        self.wp_u_resolution = -1;
        self.wp_u_delta_time = -1;

        // Unit quad, two triangles. Every window is this same quad transformed
        // by `u_dst` — there is no per-window geometry upload, ever.
        #[rustfmt::skip]
        const QUAD: [GLfloat; 12] = [
            0.0, 0.0,  1.0, 0.0,  1.0, 1.0,
            0.0, 0.0,  1.0, 1.0,  0.0, 1.0,
        ];
        unsafe {
            (gl.glGenVertexArrays)(1, &mut self.vao);
            (gl.glBindVertexArray)(self.vao);
            (gl.glGenBuffers)(1, &mut self.vbo);
            (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo);
            (gl.glBufferData)(
                GL_ARRAY_BUFFER,
                std::mem::size_of_val(&QUAD) as GLsizeiptr,
                QUAD.as_ptr().cast(),
                GL_STATIC_DRAW,
            );
            (gl.glEnableVertexAttribArray)(0);
            (gl.glVertexAttribPointer)(
                0,
                2,
                GL_FLOAT,
                GL_FALSE,
                (2 * std::mem::size_of::<GLfloat>()) as GLsizei,
                std::ptr::null(),
            );

            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDisable)(GL_SCISSOR_TEST);
            (gl.glEnable)(GL_BLEND);
            // Premultiplied-alpha "over": dst = src + dst*(1-src.a).
            (gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (gl.glUseProgram)(prog);
            (gl.glActiveTexture)(GL_TEXTURE0);
            (gl.glUniform1i)(self.u_tex, 0);
        }

        let err = gl.take_error();
        if err != GL_NO_ERROR {
            return Err(format!("GL error 0x{err:x} during setup"));
        }
        Ok(())
    }

    // ── frame ───────────────────────────────────────────────────────────────

    /// Start a frame: set the viewport to the whole overlay. When `full_clear`
    /// is true the screen is cleared to transparent black and scissor is
    /// disabled (the normal path). When false the screen is left intact so the
    /// caller can scissor + clear only the damaged region (partial redraw) —
    /// leaving the rest of the back buffer preserved, which is what makes
    /// partial redraw correct.
    pub fn begin_frame(&mut self, width: u32, height: u32, full_clear: bool) {
        let gl = &self.gl;
        self.screen_w = width;
        self.screen_h = height;
        unsafe {
            (gl.glViewport)(0, 0, width as GLsizei, height as GLsizei);
            (gl.glUseProgram)(self.prog);
            (gl.glBindVertexArray)(self.vao);
            (gl.glUniform2f)(self.u_res, width as GLfloat, height as GLfloat);
            if full_clear {
                (gl.glDisable)(GL_SCISSOR_TEST);
                (gl.glClearColor)(0.0, 0.0, 0.0, 0.0);
                (gl.glClear)(GL_COLOR_BUFFER_BIT);
            }
        }
        self.last_tex = 0;
    }

    /// How many frames stale the back buffer is (`GLX_EXT_buffer_age`). Returns
    /// `0` when the extension is unavailable or the buffer is undefined — the
    /// caller treats `0` as "repaint everything".
    pub fn back_buffer_age(&self) -> u32 {
        let Some(f) = self.glx.glXQueryDrawable else {
            return 0;
        };
        let mut age: c_uint = 0;
        unsafe {
            f(
                self.dpy.as_ptr(),
                self.glx_win,
                GLX_BACK_BUFFER_AGE_EXT,
                &mut age,
            );
        }
        age
    }

    /// Enable a scissor rectangle. `x`/`y` are top-left screen coordinates
    /// (y grows downward); GL's scissor origin is bottom-left, so the y is
    /// flipped against `height`.
    pub fn set_scissor(&mut self, x: i32, y: i32, w: u32, h: u32, height: u32) {
        let gl = &self.gl;
        unsafe {
            (gl.glEnable)(GL_SCISSOR_TEST);
            (gl.glScissor)(
                x as GLint,
                (height - (y as u32 + h)) as GLint,
                w as GLsizei,
                h as GLsizei,
            );
        }
    }

    /// Clear the colour buffer, respecting the current scissor rectangle.
    pub fn scissor_clear(&mut self) {
        let gl = &self.gl;
        unsafe {
            (gl.glClearColor)(0.0, 0.0, 0.0, 0.0);
            (gl.glClear)(GL_COLOR_BUFFER_BIT);
        }
    }

    /// Disable the scissor rectangle (back to full-screen drawing).
    pub fn clear_scissor(&mut self) {
        let gl = &self.gl;
        unsafe {
            (gl.glDisable)(GL_SCISSOR_TEST);
        }
    }

    /// Draw one textured quad.
    ///
    /// Takes `&mut Texture` so the filter cache can be updated: the only
    /// per-draw GL *state* change left is the one that genuinely varies, and
    /// only when it varies.
    pub fn draw(&mut self, tex: &mut Texture, q: &Quad) {
        let gl = &self.gl;
        unsafe {
            if self.last_tex != tex.tex {
                (gl.glBindTexture)(GL_TEXTURE_2D, tex.tex);
                self.last_tex = tex.tex;
            }
            // `smooth` only flips when a window starts/stops being scaled, so
            // in the steady state (and during a scroll, where every window is
            // either 1:1 or not) this branch is not taken at all. It used to
            // run twice per quad per frame, which is a texture-object state
            // change the driver may have to revalidate against.
            let filter = if q.smooth { GL_LINEAR } else { GL_NEAREST };
            if tex.filter != filter {
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
                tex.filter = filter;
            }
            (gl.glUniform4f)(self.u_dst, q.dst[0], q.dst[1], q.dst[2], q.dst[3]);
            (gl.glUniform4f)(self.u_src, q.src[0], q.src[1], q.src[2], q.src[3]);
            (gl.glUniform2f)(self.u_size, q.size[0], q.size[1]);
            (gl.glUniform1f)(self.u_radius, q.radius);
            (gl.glUniform1f)(self.u_opacity, q.opacity);
            (gl.glUniform1f)(self.u_flip, if tex.flip { 1.0 } else { 0.0 });
            (gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
        }
    }

    /// Draw a quad given a raw texture id, the texture's cached filter, and the
    /// previously-bound texture id (for bind-cache elision). Used by the
    /// compositor's explicit-scene path, where the `Texture` itself stays owned
    /// by `CompWin` (so the filter cache and flip flag are read there and passed
    /// in), and only the `GLuint` travels in the `DrawItem`.
    pub fn draw_raw(&mut self, tex: GLuint, filter: GLint, prev_tex: GLuint, q: &Quad) -> GLuint {
        let gl = &self.gl;
        let bound = if prev_tex != tex {
            unsafe { (gl.glBindTexture)(GL_TEXTURE_2D, tex) };
            tex
        } else {
            prev_tex
        };
        unsafe {
            // `filter` is the texture's cached value, so this is just the
            // one-time (or changing) state upload — no per-draw re-validation
            // beyond what the caller already decided.
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
            (gl.glUniform4f)(self.u_dst, q.dst[0], q.dst[1], q.dst[2], q.dst[3]);
            (gl.glUniform4f)(self.u_src, q.src[0], q.src[1], q.src[2], q.src[3]);
            (gl.glUniform2f)(self.u_size, q.size[0], q.size[1]);
            (gl.glUniform1f)(self.u_radius, q.radius);
            (gl.glUniform1f)(self.u_opacity, q.opacity);
            (gl.glUniform1f)(self.u_flip, 0.0);
            (gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
        }
        bound
    }

    /// Present the frame. With swap interval 1 this blocks until the vertical
    /// blank, which is what paces the whole animation loop.
    pub fn end_frame(&mut self) {
        unsafe { (self.glx.glXSwapBuffers)(self.dpy.as_ptr(), self.glx_win) };
    }

    /// Block until the next vertical retrace (`GLX_SGI_video_sync`). Retained for
    /// instrumentation only (C1): it reads the vblank counter and can be used to
    /// measure missed retraces. It is no longer called from the frame loop — the
    /// swap-interval-1 path in `end_frame` is the sole synchroniser, so calling
    /// this *and* relying on `glXSwapBuffers` to pace would skip every other
    /// vblank. Returns `false` when the extension is unavailable.
    pub fn wait_vblank(&self) -> bool {
        let (Some(get), Some(wait)) = (self.glx.glXGetVideoSyncSGI, self.glx.glXWaitVideoSyncSGI)
        else {
            return false;
        };
        let mut count: c_uint = 0;
        unsafe {
            (get)(&mut count);
            (wait)(1, 0, &mut count) == 0
        }
    }

    // ── textures ────────────────────────────────────────────────────────────

    /// Wrap an X pixmap (a redirected window's off-screen storage, or the root
    /// wallpaper pixmap) as a GL texture.
    ///
    /// `visual` must be the pixmap's *own* visual, as read from the window with
    /// `GetWindowAttributes` — not a guess from its depth. Returns `Err` with
    /// the reason when this screen cannot bind that visual, which the caller
    /// logs once and then treats as "don't composite this one".
    pub fn texture_from_pixmap(
        &mut self,
        pixmap: u32,
        visual: VisualFormat,
        width: u16,
        height: u16,
    ) -> Result<Texture, String> {
        let tfp = self.tfp_config(visual)?;
        let attribs: [c_int; 5] = [
            GLX_TEXTURE_TARGET_EXT,
            GLX_TEXTURE_2D_EXT,
            GLX_TEXTURE_FORMAT_EXT,
            tfp.format,
            0,
        ];
        // The first pixmap of a given visual is round-tripped: `glXCreatePixmap`
        // reports a depth/fbconfig mismatch asynchronously as `BadMatch`, and
        // our error handler swallows it, so without this check a mismatched
        // config silently yields a texture full of the wrong channels. Later
        // pixmaps of the same visual skip the sync — it would stall every
        // interactive resize.
        let verify = self.verified.insert(visual.id);
        if verify {
            crate::xlib::clear_x_error();
        }
        let glx_pixmap = unsafe {
            (self.glx.glXCreatePixmap)(
                self.dpy.as_ptr(),
                tfp.cfg,
                c_ulong::from(pixmap),
                attribs.as_ptr(),
            )
        };
        if verify {
            self.dpy.sync();
            if let Some(code) = crate::xlib::take_x_error() {
                // Don't destroy: the resource was never created, and the extra
                // request would only raise a second error.
                self.verified.remove(&visual.id);
                return Err(format!(
                    "glXCreatePixmap for {visual} failed with {} ({})",
                    crate::xlib::x_error_name(code),
                    tfp
                ));
            }
        }
        if glx_pixmap == 0 {
            return Err(format!("glXCreatePixmap for {visual} returned None"));
        }
        let mut tex: GLuint = 0;
        unsafe {
            (self.gl.glGenTextures)(1, &mut tex);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, tex);
            // Wrap mode is genuinely constant for every texture we ever create,
            // so it is set exactly once, here.
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        }
        self.last_tex = tex;
        let mut t = Texture {
            glx_pixmap,
            tex,
            flip: tfp.flip,
            width,
            height,
            bound: false,
            // Must match the filter actually set above, or the first `draw`
            // would skip the update it needs.
            filter: GL_LINEAR,
        };
        self.bind(&mut t);
        Ok(t)
    }

    /// (Re)bind the pixmap to its texture. Cheap — it is a driver-side rebind,
    /// not a copy — and mandatory after every damage event: the TFP spec leaves
    /// the texture contents *undefined* once the client has drawn into the
    /// drawable while it was bound.
    pub fn bind(&mut self, t: &mut Texture) {
        let (Some(bind), Some(release)) =
            (self.glx.glXBindTexImageEXT, self.glx.glXReleaseTexImageEXT)
        else {
            return;
        };
        let d = self.dpy.as_ptr();
        unsafe {
            if self.last_tex != t.tex {
                (self.gl.glBindTexture)(GL_TEXTURE_2D, t.tex);
                self.last_tex = t.tex;
            }
            if t.bound {
                release(d, t.glx_pixmap, GLX_FRONT_LEFT_EXT);
                t.bound = false;
            }
            bind(d, t.glx_pixmap, GLX_FRONT_LEFT_EXT, std::ptr::null());
        }
        t.bound = true;
    }

    /// Delete a raw GL texture (one not backed by a GLX pixmap) created by
    /// `upload_rgba`. Does not touch any X resource. Used to release wallpaper
    /// image textures.
    pub fn destroy_raw(&mut self, tex: GLuint) {
        if tex == 0 {
            return;
        }
        let gl = &self.gl;
        unsafe {
            if self.last_tex == tex {
                self.last_tex = 0;
            }
            (gl.glDeleteTextures)(1, &tex);
        }
    }

    /// Upload a decoded RGBA8 image to a GPU texture (straight → premultiplied, so
    /// the window-path premultiplied blend is already correct). Returns the raw
    /// texture name; the caller owns it and must `destroy_raw` it. Errors (driver
    /// rejection, oversized) return `Err` with a clear message and free the
    /// half-created texture. Respects `GL_MAX_TEXTURE_SIZE` (the plan's risk note:
    /// reject, never silently downscale).
    pub fn upload_rgba(&mut self, img: &Rgba8) -> Result<GLuint, String> {
        let gl = &self.gl;
        let max_size = self.max_texture_size();
        if img.w > max_size || img.h > max_size {
            return Err(format!(
                "image {}x{} exceeds GL_MAX_TEXTURE_SIZE {}",
                img.w, img.h, max_size
            ));
        }
        let mut tex: GLuint = 0;
        unsafe {
            (gl.glGenTextures)(1, &mut tex);
        }
        if tex == 0 {
            return Err("glGenTextures failed".into());
        }
        // Premultiply straight RGBA → premultiplied (the compositor's blend is
        // (ONE, ONE_MINUS_SRC_ALPHA) and expects premultiplied source).
        let mut premult = Vec::with_capacity(img.data.len());
        for chunk in img.data.chunks_exact(4) {
            let r = chunk[0] as u32;
            let g = chunk[1] as u32;
            let b = chunk[2] as u32;
            let a = chunk[3] as u32;
            premult.extend_from_slice(&[
                ((r * a + 127) / 255) as u8,
                ((g * a + 127) / 255) as u8,
                ((b * a + 127) / 255) as u8,
                a as u8,
            ]);
        }
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, tex);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                img.w as GLsizei,
                img.h as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                premult.as_ptr().cast::<c_void>(),
            );
        }
        if self.gl.take_error() != GL_NO_ERROR {
            self.destroy_raw(tex);
            return Err("glTexImage2D failed for wallpaper texture".into());
        }
        Ok(tex)
    }

    /// Compile a user GLSL fragment shader into a wallpaper program (combined with
    /// our unit-quad vertex shader). The fragment shader must declare the fixed
    /// contract uniforms `u_time` (float), `u_resolution` (vec2) and `u_delta_time`
    /// (float) and write its colour to `out vec4 frag`. On failure returns `Err`
    /// with the GL log (no panic) so the wallpaper can be disabled without taking
    /// down the compositor.
    pub fn compile_fragment(&mut self, frag: &str) -> Result<GLuint, String> {
        let gl = &self.gl;
        let vs = match compile_shader(gl, GL_VERTEX_SHADER, VERTEX_SRC) {
            Ok(v) => v,
            Err(e) => return Err(format!("wallpaper vertex shader: {e}")),
        };
        let fs = match compile_shader(gl, GL_FRAGMENT_SHADER, frag) {
            Ok(f) => f,
            Err(e) => {
                unsafe { (gl.glDeleteShader)(vs) };
                return Err(format!("wallpaper fragment shader: {e}"));
            }
        };
        let prog = unsafe { (gl.glCreateProgram)() };
        unsafe {
            (gl.glAttachShader)(prog, vs);
            (gl.glAttachShader)(prog, fs);
            (gl.glLinkProgram)(prog);
            (gl.glDeleteShader)(vs);
            (gl.glDeleteShader)(fs);
        }
        let mut ok: GLint = 0;
        unsafe { (gl.glGetProgramiv)(prog, GL_LINK_STATUS, &mut ok) };
        if ok == 0 {
            let log = program_log(gl, prog);
            unsafe { (gl.glDeleteProgram)(prog) };
            return Err(format!("wallpaper shader link failed: {log}"));
        }
        let loc = |name: &str| -> GLint {
            let c = CString::new(name).expect("uniform name has no NUL");
            unsafe { (gl.glGetUniformLocation)(prog, c.as_ptr()) }
        };
        let u_dst = loc("u_dst");
        let u_res = loc("u_res");
        let u_time = loc("u_time");
        let u_resolution = loc("u_resolution");
        let u_delta_time = loc("u_delta_time");
        self.wp_prog = prog;
        self.wp_u_dst = u_dst;
        self.wp_u_res = u_res;
        self.wp_u_time = u_time;
        self.wp_u_resolution = u_resolution;
        self.wp_u_delta_time = u_delta_time;
        Ok(prog)
    }

    /// Query `GL_MAX_TEXTURE_SIZE` once (cached lazily). Returns a sane default if
    /// the query is unavailable.
    fn max_texture_size(&self) -> u32 {
        let mut v: GLint = 0;
        unsafe { (self.gl.glGetIntegerv)(GL_MAX_TEXTURE_SIZE, &mut v) };
        if v <= 0 {
            4096
        } else {
            v as u32
        }
    }

    /// Draw the wallpaper shader filling `out` (screen px) for `time`/`dt`. The
    /// shader fills the quad; per-output `u_resolution` lets it know its own pixel
    /// dimensions. No texture is sampled.
    pub fn draw_shader(&mut self, prog: GLuint, out: Rect, time: f32, dt: f32) {
        let gl = &self.gl;
        unsafe {
            (gl.glUseProgram)(prog);
            (gl.glBindVertexArray)(self.vao);
        }
        let (sw, sh) = (self.screen_w as f32, self.screen_h as f32);
        unsafe {
            (gl.glUniform2f)(self.wp_u_res, sw, sh);
            let dst = [
                out.x as f32,
                out.y as f32,
                (out.x + out.w as i32) as f32,
                (out.y + out.h as i32) as f32,
            ];
            (gl.glUniform4f)(self.wp_u_dst, dst[0], dst[1], dst[2], dst[3]);
            (gl.glUniform1f)(self.wp_u_time, time);
            (gl.glUniform2f)(self.wp_u_resolution, out.w as f32, out.h as f32);
            (gl.glUniform1f)(self.wp_u_delta_time, dt);
            (gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
        }
    }
    pub fn destroy_texture(&mut self, mut t: Texture) {
        let d = self.dpy.as_ptr();
        unsafe {
            if t.bound {
                if let Some(release) = self.glx.glXReleaseTexImageEXT {
                    (self.gl.glBindTexture)(GL_TEXTURE_2D, t.tex);
                    release(d, t.glx_pixmap, GLX_FRONT_LEFT_EXT);
                }
                t.bound = false;
            }
            (self.glx.glXDestroyPixmap)(d, t.glx_pixmap);
            (self.gl.glDeleteTextures)(1, &t.tex);
        }
        // Invalidate the bind cache unconditionally — *not* just when the
        // destroyed texture was the cached one.
        //
        // Two things happened above that both desynchronise `last_tex` from the
        // real GL binding: the release path binds `t.tex` (so the binding is no
        // longer whatever `last_tex` claims), and `glDeleteTextures` on the
        // currently bound texture reverts the binding to 0 per spec. Leaving a
        // stale non-zero `last_tex` makes the next `draw` of *that other*
        // texture skip its `glBindTexture` while nothing is actually bound, and
        // the window samples texture 0 — it renders as an empty hole. This is
        // reachable on any destroy/resize (both free the texture) that is not
        // the most recently drawn window.
        self.last_tex = 0;
    }

    fn tfp_config(&mut self, visual: VisualFormat) -> Result<TfpConfig, String> {
        if let Some(hit) = self.tfp_cache.get(&visual.id) {
            return hit.clone();
        }
        let found = choose_tfp_fbconfig(
            &self.glx,
            self.dpy.as_ptr(),
            self.screen,
            &self.visuals,
            visual,
        );
        self.tfp_cache.insert(visual.id, found.clone());
        found
    }

    // ── self-check / diagnostics ────────────────────────────────────────────

    /// The visual the final framebuffer uses — i.e. what the screen can
    /// actually display, however deep the client's own windows are.
    #[inline]
    pub fn root_format(&self) -> VisualFormat {
        self.root_format
    }

    /// What the compositor found out about one of the screen's visuals.
    ///
    /// The caller decides how loud to be about it; the renderer only reports.
    /// Without this, an unbindable visual is indistinguishable from a window
    /// that simply has nothing to draw — both end as `tex: None` and the window
    /// silently disappears from the frame, which looks like a colour or
    /// rendering bug rather than the format mismatch it is.
    pub fn format_report(&mut self) -> Vec<VisualReport> {
        let visuals = self.visuals.clone();
        visuals
            .into_iter()
            .map(|format| VisualReport {
                format,
                binding: self
                    .tfp_config(format)
                    .map(|cfg| cfg.to_string())
                    .map_err(|e| e.to_string()),
            })
            .collect()
    }

    /// Raw dump of every fbconfig, for `MAVERICK_LOG=debug`. Cheap to build and
    /// the first thing worth looking at when colours come out wrong.
    pub fn fbconfig_report(&self) -> Vec<String> {
        let d = self.dpy.as_ptr();
        let mut n: c_int = 0;
        let list = unsafe { (self.glx.glXGetFBConfigs)(d, self.screen, &mut n) };
        if list.is_null() || n <= 0 {
            return vec!["glXGetFBConfigs: none".into()];
        }
        let configs = unsafe { std::slice::from_raw_parts(list, n as usize) };
        let attr = |cfg, a| self.glx.config_attrib(d, cfg, a);
        let out = configs
            .iter()
            .enumerate()
            .map(|(i, &cfg)| {
                let vid = attr(cfg, GLX_VISUAL_ID).unwrap_or(0) as u32;
                let depth = self
                    .visuals
                    .iter()
                    .find(|v| v.id == vid)
                    .map(|v| v.depth as i32)
                    .unwrap_or(-1);
                format!(
                    "fbconfig[{i}] visual 0x{vid:x} (x depth {depth}) buffer {:?} \
                     R{:?}G{:?}B{:?}A{:?} draw {:?} render {:?} bindRGB {:?} bindRGBA {:?} \
                     targets {:?} y_inverted {:?} caveat {:?}",
                    attr(cfg, GLX_BUFFER_SIZE),
                    attr(cfg, GLX_RED_SIZE),
                    attr(cfg, GLX_GREEN_SIZE),
                    attr(cfg, GLX_BLUE_SIZE),
                    attr(cfg, GLX_ALPHA_SIZE),
                    attr(cfg, GLX_DRAWABLE_TYPE),
                    attr(cfg, GLX_RENDER_TYPE),
                    attr(cfg, GLX_BIND_TO_TEXTURE_RGB_EXT),
                    attr(cfg, GLX_BIND_TO_TEXTURE_RGBA_EXT),
                    attr(cfg, GLX_BIND_TO_TEXTURE_TARGETS_EXT),
                    attr(cfg, GLX_Y_INVERTED_EXT),
                    attr(cfg, GLX_CONFIG_CAVEAT),
                )
            })
            .collect();
        unsafe { crate::xlib::XFree(list.cast()) };
        out
    }

    // ── teardown ────────────────────────────────────────────────────────────

    /// Drop the GL context and its drawable. Called when the compositor is
    /// disabled at runtime (a GL failure) and on shutdown. Textures must have
    /// been destroyed first.
    pub fn destroy(&mut self) {
        let d = self.dpy.as_ptr();
        unsafe {
            if self.prog != 0 {
                (self.gl.glDeleteProgram)(self.prog);
                self.prog = 0;
            }
            if self.vbo != 0 {
                (self.gl.glDeleteBuffers)(1, &self.vbo);
                self.vbo = 0;
            }
            if self.vao != 0 {
                (self.gl.glDeleteVertexArrays)(1, &self.vao);
                self.vao = 0;
            }
            (self.glx.glXMakeCurrent)(d, 0, std::ptr::null_mut());
            if self.glx_win != 0 {
                (self.glx.glXDestroyWindow)(d, self.glx_win);
                self.glx_win = 0;
            }
            if !self.ctx.is_null() {
                (self.glx.glXDestroyContext)(d, self.ctx);
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn enable_vsync(
    glx: &Glx,
    d: *mut crate::xlib::Display,
    screen: c_int,
    drawable: GLXDrawable,
    exts: &str,
) -> bool {
    if has_extension(exts, "GLX_EXT_swap_control") {
        if let Some(f) = glx.glXSwapIntervalEXT {
            unsafe { f(d, drawable, 1) };
            return true;
        }
    }
    if has_extension(exts, "GLX_MESA_swap_control") {
        if let Some(f) = glx.glXSwapIntervalMESA {
            return unsafe { f(1) } == 0;
        }
    }
    if has_extension(exts, "GLX_SGI_swap_control") {
        if let Some(f) = glx.glXSwapIntervalSGI {
            return unsafe { f(1) } == 0;
        }
    }
    let _ = screen;
    false
}

/// Pick a double-buffered, window-renderable fbconfig whose visual is exactly
/// the root visual — a mismatch makes `glXCreateWindow` answer `BadMatch`,
/// because the Composite overlay is created with the root visual and X requires
/// drawable and fbconfig to agree.
///
/// Deliberately **not** `glXChooseFBConfig`: the attribute list used to ask for
/// `GLX_RED_SIZE >= 8, GLX_GREEN_SIZE >= 8`, which excludes every fbconfig on a
/// 15- or 16-bit screen (`R5G6B5`) and made the compositor refuse to start
/// there for no reason. The only hard requirement is the visual id, so we
/// enumerate and filter on that, and report precisely what was missing.
fn choose_window_fbconfig(
    glx: &Glx,
    d: *mut crate::xlib::Display,
    screen: c_int,
    root: VisualFormat,
) -> Result<GLXFBConfig, String> {
    let mut n: c_int = 0;
    let list = unsafe { (glx.glXGetFBConfigs)(d, screen, &mut n) };
    if list.is_null() || n <= 0 {
        return Err("glXGetFBConfigs returned no fbconfig at all".into());
    }
    let configs = unsafe { std::slice::from_raw_parts(list, n as usize) };
    let mut matched_visual = 0usize;
    let mut single_buffered = 0usize;
    let mut picked = None;
    for &cfg in configs {
        if glx.config_attrib(d, cfg, GLX_VISUAL_ID) != Some(root.id as c_int) {
            continue;
        }
        matched_visual += 1;
        if glx.config_attrib(d, cfg, GLX_DRAWABLE_TYPE).unwrap_or(0) & GLX_WINDOW_BIT == 0 {
            continue;
        }
        if glx
            .config_attrib(d, cfg, GLX_RENDER_TYPE)
            .unwrap_or(GLX_RGBA_BIT)
            & GLX_RGBA_BIT
            == 0
        {
            continue;
        }
        if glx.config_attrib(d, cfg, GLX_DOUBLEBUFFER) != Some(1) {
            single_buffered += 1;
            continue;
        }
        picked = Some(cfg);
        break;
    }
    unsafe { crate::xlib::XFree(list.cast()) };
    picked.ok_or_else(|| {
        format!(
            "no double-buffered fbconfig for the overlay's {root} \
             ({n} fbconfigs, {matched_visual} on that visual, {single_buffered} single-buffered)"
        )
    })
}

/// Find an fbconfig that can bind a pixmap of exactly `want`'s visual as a
/// `GL_TEXTURE_2D`.
///
/// This function only *reads* GLX; the actual decision lives in the pure
/// [`rate_fbconfig`], which documents every rule and is unit-tested.
fn choose_tfp_fbconfig(
    glx: &Glx,
    d: *mut crate::xlib::Display,
    screen: c_int,
    visuals: &[VisualFormat],
    want: VisualFormat,
) -> Result<TfpConfig, String> {
    if !want.direct {
        return Err(format!(
            "{want} is a palette visual — texture-from-pixmap only samples TrueColor/DirectColor"
        ));
    }
    let mut n: c_int = 0;
    let list = unsafe { (glx.glXGetFBConfigs)(d, screen, &mut n) };
    if list.is_null() || n <= 0 {
        return Err("glXGetFBConfigs returned no fbconfig at all".into());
    }
    let configs = unsafe { std::slice::from_raw_parts(list, n as usize) };
    let want_alpha = want.has_alpha();
    let mut why = Rejects::default();
    let mut best: Option<(i32, TfpConfig)> = None;

    for &cfg in configs {
        let attr = |a| glx.config_attrib(d, cfg, a);
        let visual = attr(GLX_VISUAL_ID).unwrap_or(0) as u32;
        let targets = attr(GLX_BIND_TO_TEXTURE_TARGETS_EXT);
        let fb = FbAttrs {
            visual,
            visual_depth: visuals.iter().find(|v| v.id == visual).map(|v| v.depth),
            pixmap_renderable: attr(GLX_DRAWABLE_TYPE).unwrap_or(0) & GLX_PIXMAP_BIT != 0,
            // A server that does not answer `GLX_RENDER_TYPE` predates
            // colour-index configs being interesting; assume RGBA.
            rgba_render: attr(GLX_RENDER_TYPE).unwrap_or(GLX_RGBA_BIT) & GLX_RGBA_BIT != 0,
            rgba: [
                attr(GLX_RED_SIZE).unwrap_or(0),
                attr(GLX_GREEN_SIZE).unwrap_or(0),
                attr(GLX_BLUE_SIZE).unwrap_or(0),
                attr(GLX_ALPHA_SIZE).unwrap_or(0),
            ],
            buffer_size: attr(GLX_BUFFER_SIZE).unwrap_or(0),
            bind_rgb: attr(GLX_BIND_TO_TEXTURE_RGB_EXT) == Some(1),
            bind_rgba: attr(GLX_BIND_TO_TEXTURE_RGBA_EXT) == Some(1),
            // `GLX_DONT_CARE` (-1) is what a server that does not track
            // per-target support answers; treating it as "no 2D target" would
            // disable compositing entirely on those servers.
            target_2d: match targets {
                None | Some(GLX_DONT_CARE) => true,
                Some(t) => t & GLX_TEXTURE_2D_BIT_EXT != 0,
            },
            caveat_free: attr(GLX_CONFIG_CAVEAT) == Some(GLX_NONE),
            y_inverted: attr(GLX_Y_INVERTED_EXT),
        };

        match rate_fbconfig(want, &fb) {
            Err(r) => why.note(r),
            Ok(score) if best.as_ref().is_none_or(|(s, _)| score > *s) => {
                best = Some((
                    score,
                    TfpConfig {
                        cfg,
                        format: if want_alpha {
                            GLX_TEXTURE_FORMAT_RGBA_EXT
                        } else {
                            GLX_TEXTURE_FORMAT_RGB_EXT
                        },
                        // `GLX_Y_INVERTED_EXT == TRUE` means the *top* of the
                        // drawable is at texture coordinate `t = 0` — the
                        // extension spec's own usage example spells it out:
                        //
                        //     if (y_inverted == TRUE) { top = 0.0; bottom = 1.0; }
                        //     else                    { top = 1.0; bottom = 0.0; }
                        //
                        // The vertex shader already measures `u_src.y`
                        // top-down, i.e. it samples `t = 0` at the top of the
                        // quad, so TRUE is precisely the case that needs **no**
                        // flip and FALSE is the one that does. Testing for
                        // `== 1` (as this used to) renders every window upside
                        // down on any driver that answers TRUE — which is most
                        // of them. Servers that answer the out-of-spec `-1`
                        // (`GLX_DONT_CARE`) are treated as the common TRUE
                        // case, which is what they measurably do.
                        flip: fb.y_inverted == Some(0),
                        visual: fb.visual,
                        buffer_size: fb.buffer_size,
                        rgba: fb.rgba,
                        y_inverted: fb.y_inverted,
                    },
                ));
            }
            Ok(_) => {}
        }
    }
    unsafe { crate::xlib::XFree(list.cast()) };
    best.map(|(_, c)| c)
        .ok_or_else(|| format!("no fbconfig binds {want} as a texture (of {n}: {why})"))
}

fn compile_shader(gl: &Gl, kind: GLenum, src: &str) -> Result<GLuint, String> {
    let sh = unsafe { (gl.glCreateShader)(kind) };
    if sh == 0 {
        return Err("glCreateShader failed".into());
    }
    let ptr = src.as_ptr().cast::<GLchar>();
    let len = src.len() as GLint;
    unsafe {
        (gl.glShaderSource)(sh, 1, &ptr, &len);
        (gl.glCompileShader)(sh);
    }
    let mut ok: GLint = 0;
    unsafe { (gl.glGetShaderiv)(sh, GL_COMPILE_STATUS, &mut ok) };
    if ok == 0 {
        let log = shader_log(gl, sh);
        unsafe { (gl.glDeleteShader)(sh) };
        let stage = if kind == GL_VERTEX_SHADER {
            "vertex"
        } else {
            "fragment"
        };
        return Err(format!("{stage} shader failed to compile: {log}"));
    }
    Ok(sh)
}

fn shader_log(gl: &Gl, sh: GLuint) -> String {
    let mut len: GLint = 0;
    unsafe { (gl.glGetShaderiv)(sh, GL_INFO_LOG_LENGTH, &mut len) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u8; len as usize];
    let mut written: GLsizei = 0;
    unsafe {
        (gl.glGetShaderInfoLog)(sh, len, &mut written, buf.as_mut_ptr().cast::<GLchar>());
    }
    buf.truncate(written.max(0) as usize);
    String::from_utf8_lossy(&buf).into_owned()
}

fn program_log(gl: &Gl, prog: GLuint) -> String {
    let mut len: GLint = 0;
    unsafe { (gl.glGetProgramiv)(prog, GL_INFO_LOG_LENGTH, &mut len) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u8; len as usize];
    let mut written: GLsizei = 0;
    unsafe {
        (gl.glGetProgramInfoLog)(prog, len, &mut written, buf.as_mut_ptr().cast::<GLchar>());
    }
    buf.truncate(written.max(0) as usize);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Keep the `XID` alias reachable for downstream crates that talk about GLX
/// drawables without importing `xlib` directly.
pub type GlxXid = XID;

#[cfg(test)]
mod tests {
    use super::*;

    // ── the screen's side: what X says it can show ──────────────────────────

    /// The ordinary opaque visual: depth 24 stored as `x8r8g8b8`.
    const RGB24: VisualFormat = VisualFormat {
        id: 0x102,
        depth: 24,
        red_bits: 8,
        green_bits: 8,
        blue_bits: 8,
        alpha_bits: 0,
        direct: true,
    };
    /// The ARGB visual every compositing client (terminals, GTK popups) uses.
    const ARGB32: VisualFormat = VisualFormat {
        id: 0x103,
        depth: 32,
        red_bits: 8,
        green_bits: 8,
        blue_bits: 8,
        alpha_bits: 8,
        direct: true,
    };
    /// A 16-bit screen: `R5G6B5`, no alpha.
    const RGB16: VisualFormat = VisualFormat {
        id: 0x21,
        depth: 16,
        red_bits: 5,
        green_bits: 6,
        blue_bits: 5,
        alpha_bits: 0,
        direct: true,
    };

    // ── the driver's side: fbconfigs, as measured from real servers ─────────

    fn fb(visual: u32, visual_depth: Option<u8>, rgba: [c_int; 4]) -> FbAttrs {
        FbAttrs {
            visual,
            visual_depth,
            pixmap_renderable: true,
            rgba_render: true,
            rgba,
            buffer_size: rgba.iter().sum(),
            bind_rgb: true,
            bind_rgba: true,
            target_2d: true,
            caveat_free: true,
            y_inverted: Some(1),
        }
    }

    fn best<'a>(want: VisualFormat, configs: &'a [(&'a str, FbAttrs)]) -> Option<&'a str> {
        configs
            .iter()
            .filter_map(|(name, a)| rate_fbconfig(want, a).ok().map(|s| (s, *name)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, name)| name)
    }

    /// The bug the user saw. On a driver that also exposes a 10-bit config,
    /// `GLX_BUFFER_SIZE == 32 && alpha != 0` matches `R10G10B10A2` — 10+10+10+2
    /// is also 32 — and an 8-bit ARGB window bound through it comes back with
    /// its channels reinterpreted: orange (255,128,64) reads as (255,247,16).
    #[test]
    fn argb32_never_binds_through_a_10bit_config() {
        let configs = [
            // Mesa lists the deep-colour, visual-less config first.
            ("rgb10a2", fb(0, None, [10, 10, 10, 2])),
            ("rgba8", fb(ARGB32.id, Some(32), [8, 8, 8, 8])),
        ];
        assert_eq!(best(ARGB32, &configs), Some("rgba8"));
        assert_eq!(
            rate_fbconfig(ARGB32, &configs[0].1),
            Err(Reject::NoAlpha),
            "a 2-bit alpha channel cannot carry an 8-bit ARGB visual"
        );
    }

    /// The other half of the bug: a depth-24 visual's fbconfig reports a
    /// **32-bit** buffer with 8 alpha bits, because `x8r8g8b8` is how the
    /// server stores it. Requiring `buffer_size == depth`, or refusing configs
    /// that merely have an alpha channel, finds nothing at all — and every
    /// ordinary window then silently disappears from the frame.
    #[test]
    fn rgb24_binds_through_a_32bit_buffer_with_alpha_bits() {
        let cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        assert_eq!(cfg.buffer_size, 32, "this is the case that used to fail");
        assert!(rate_fbconfig(RGB24, &cfg).is_ok());
    }

    /// A config must never be chosen for a pixmap of a different depth:
    /// `glXCreatePixmap` answers `BadMatch`, and a lenient server hands back a
    /// texture with the wrong channel layout instead.
    #[test]
    fn depth_must_match_the_configs_own_visual() {
        let rgb24_cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        assert_eq!(
            rate_fbconfig(ARGB32, &rgb24_cfg),
            Err(Reject::DepthMismatch)
        );
        let argb32_cfg = fb(ARGB32.id, Some(32), [8, 8, 8, 8]);
        assert_eq!(
            rate_fbconfig(RGB24, &argb32_cfg),
            Err(Reject::DepthMismatch)
        );
    }

    /// The exact visual beats a merely same-depth one.
    #[test]
    fn exact_visual_wins_over_same_depth() {
        let configs = [
            ("other-24bit-visual", fb(0x999, Some(24), [8, 8, 8, 8])),
            ("root-visual", fb(RGB24.id, Some(24), [8, 8, 8, 8])),
        ];
        assert_eq!(best(RGB24, &configs), Some("root-visual"));
    }

    /// A screen cannot show more colour than it has, but the compositor must
    /// not show *less* either: a config narrower than the visual would
    /// posterise every window, so it is rejected rather than silently used.
    #[test]
    fn a_narrower_config_is_rejected_a_wider_one_is_allowed() {
        assert_eq!(
            rate_fbconfig(RGB24, &fb(0, None, [5, 6, 5, 0])),
            Err(Reject::TooFewBits)
        );
        assert!(rate_fbconfig(RGB16, &fb(0, None, [8, 8, 8, 0])).is_ok());
        // ...but on a 16-bit screen the native 5/6/5 config still wins.
        let configs = [
            ("widened-8888", fb(0, None, [8, 8, 8, 0])),
            ("native-565", fb(RGB16.id, Some(16), [5, 6, 5, 0])),
        ];
        assert_eq!(best(RGB16, &configs), Some("native-565"));
    }

    /// Some servers answer `GLX_DONT_CARE` (-1) for the bind targets. Reading
    /// that as "no GL_TEXTURE_2D" disables compositing on them entirely.
    #[test]
    fn dont_care_bind_targets_are_usable() {
        let mut cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        cfg.target_2d = true; // what the reader derives from -1 / unsupported
        assert!(rate_fbconfig(RGB24, &cfg).is_ok());
        cfg.target_2d = false; // a server that really says "no 2D"
        assert_eq!(rate_fbconfig(RGB24, &cfg), Err(Reject::No2dTarget));
    }

    /// A depth-24 pixmap is bound with `GLX_TEXTURE_FORMAT_RGB_EXT`, so it only
    /// needs `GLX_BIND_TO_TEXTURE_RGB_EXT`; an ARGB one needs the RGBA form.
    #[test]
    fn bind_capability_follows_the_texture_format() {
        let mut cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        cfg.bind_rgba = false;
        assert!(
            rate_fbconfig(RGB24, &cfg).is_ok(),
            "an opaque visual does not need RGBA binding"
        );
        let mut cfg = fb(ARGB32.id, Some(32), [8, 8, 8, 8]);
        cfg.bind_rgba = false;
        assert_eq!(rate_fbconfig(ARGB32, &cfg), Err(Reject::NotBindable));
    }

    /// Colour-index and non-pixmap configs are never texture sources.
    #[test]
    fn colour_index_and_window_only_configs_are_skipped() {
        let mut cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        cfg.rgba_render = false;
        assert_eq!(rate_fbconfig(RGB24, &cfg), Err(Reject::NotRgba));
        let mut cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        cfg.pixmap_renderable = false;
        assert_eq!(rate_fbconfig(RGB24, &cfg), Err(Reject::NotPixmap));
    }
}
