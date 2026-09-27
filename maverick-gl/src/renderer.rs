// The GPU side of the compositor: one GLX context on the Composite overlay
// window, one shader program, one unit quad.
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
//
// Every method here runs on the thread that called `glXMakeCurrent` on the
// overlay drawable. `Renderer` holds the `GLXContext` as a raw pointer, so it
// is neither `Send` nor `Sync` and the compiler enforces the affinity.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::fmt;
use std::os::raw::{c_int, c_uint, c_ulong};
use std::sync::OnceLock;

use maverick_img::Rgba8;

/// Opaque handle to a compiled wallpaper shader program (opaque `u32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShaderId(pub u32);

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
use maverick_x11::{XDisplay, XID};

/// Opt-in diagnostic: when `MAV_GLX_TRACE` is set, log every GLX texture
/// lifecycle op (create/bind/release/destroy) with the GLXPixmap and texture
/// ids. Off by default and a no-op when unset — never changes rendering.
fn glx_trace_enabled() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::env::var_os("MAV_GLX_TRACE").is_some())
}

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
uniform float u_border_width;
uniform vec4 u_border_color;
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
        float radius = min(u_radius, min(u_size.x, u_size.y) * 0.5);
        float d = sd_round_box(p, u_size * 0.5, radius);
        float outer = 1.0 - smoothstep(-0.5, 0.5, d);
        if (u_border_width > 0.0) {
            float width = min(u_border_width, min(u_size.x, u_size.y) * 0.5);
            float inner_d = sd_round_box(p, u_size * 0.5 - width, max(radius - width, 0.0));
            float inner = min(outer, 1.0 - smoothstep(-0.5, 0.5, inner_d));
            src = src * inner + u_border_color * (outer - inner);
        } else {
            src *= outer;
        }
    }
    // Scaling the whole vec4 keeps the result premultiplied.
    frag = src * a;
}
"#;

/// The context the compositor asks for, as the key/value list
/// `glXCreateContextAttribsARB` expects: attribute/value pairs closed by a
/// `0`, which is a terminator and not an attribute.
///
/// The version is deliberately kept in step with the `#version` line both
/// built-in shaders declare. GLSL only ever versions *downwards*: a context
/// older than its shaders is a compile failure on a real driver, and nothing
/// in this process reports it before the window goes black.
const GLX_CTX_ATTRIBS: [c_int; 7] = [
    GLX_CONTEXT_MAJOR_VERSION_ARB,
    3,
    GLX_CONTEXT_MINOR_VERSION_ARB,
    3,
    GLX_CONTEXT_PROFILE_MASK_ARB,
    GLX_CONTEXT_CORE_PROFILE_BIT_ARB,
    0,
];

/// A window (or pixmap) bound as an OpenGL texture through
/// `GLX_EXT_texture_from_pixmap`.
///
/// Not `Drop`: freeing it needs the `Display*` and a current GL context, so the
/// owner must hand it back to [`Renderer::destroy_texture`] rather than let it
/// go out of scope.
pub struct Texture {
    pub glx_pixmap: GLXPixmap,
    pub(crate) tex: GLuint,
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
    /// frames — but it depends on `DrawQuad::filter`, which changes when a window
    /// starts or stops being scaled by an animation. Caching the value here is
    /// what lets `draw` re-issue `glTexParameteri` only on that transition
    /// instead of twice per quad per frame.
    filter: Filter,
}

impl Texture {
    /// Opaque handle to the underlying GL texture name.
    pub fn handle(&self) -> TextureHandle {
        TextureHandle(self.tex)
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
            filter: Filter::Linear,
        }
    }
}

impl Texture {
    #[inline]
    pub fn is_bound(&self) -> bool {
        self.bound
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

/// Filter mode for a textured quad. Mirrors the OpenGL constants
/// `GL_NEAREST` / `GL_LINEAR` without exposing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// `GL_NEAREST` (1:1, so nearest is both sharper and cheaper). The default.
    #[default]
    Nearest,
    /// `GL_LINEAR` (the quad is scaled by an animation).
    Linear,
}

impl Filter {
    #[inline]
    pub fn to_gl(self) -> GLint {
        match self {
            Filter::Nearest => GL_NEAREST,
            Filter::Linear => GL_LINEAR,
        }
    }
}

/// Opaque handle to a GPU texture. The underlying `u32` is backend-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureHandle(pub u32);

/// Quad to draw this frame, free of any OpenGL constant.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawQuad {
    /// Destination rect in screen pixels: `x0, y0, x1, y1`, origin top-left.
    pub dst: [f32; 4],
    /// Source rect in normalised texture coords: `u0, v0, u1, v1`, `v` top-down.
    pub src: [f32; 4],
    /// Quad size in pixels — what the rounded-rect SDF measures against.
    pub size: [f32; 2],
    /// Corner radius in pixels; `0.0` takes the fast path (no SDF at all).
    pub radius: f32,
    pub border_width: f32,
    pub border_color: [f32; 4],
    /// 0..1 multiplier applied to the premultiplied source.
    pub opacity: f32,
    /// Texture filtering mode.
    pub filter: Filter,
}

/// Which backend is driving the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererBackend {
    OpenGlGlx,
}

impl fmt::Display for RendererBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RendererBackend::OpenGlGlx => f.write_str("OpenGL/GLX"),
        }
    }
}

/// VSync mode the compositor asks GLX for at renderer construction. Adaptive
/// needs `GLX_EXT_swap_control_tear`; the other two are honoured by every
/// swap-control extension `enable_vsync` knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsyncMode {
    On,
    Off,
    Adaptive,
}

/// Hardware vs software acceleration classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceleration {
    Gpu,
    Software,
    Unknown,
}

impl fmt::Display for Acceleration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Acceleration::Gpu => f.write_str("GPU"),
            Acceleration::Software => f.write_str("Software"),
            Acceleration::Unknown => f.write_str("Unknown"),
        }
    }
}

/// Structured renderer info returned to the compositor for startup logging.
#[derive(Debug, Clone)]
pub struct RendererInfo {
    pub backend: RendererBackend,
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    pub accelerated: Acceleration,
}

impl fmt::Display for RendererInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Compositor:")?;
        writeln!(f, "  Backend: {}", self.backend)?;
        writeln!(f, "  Vendor: {}", self.vendor)?;
        writeln!(f, "  Renderer: {}", self.renderer)?;
        writeln!(f, "  Version: {}", self.version)?;
        writeln!(f, "  Acceleration: {}", self.accelerated)
    }
}

/// Classify a (vendor, renderer) pair as GPU, Software, or Unknown using
/// purely string-based heuristics (no GPU-vendor-name guessing).
pub fn classify_acceleration(vendor: &str, renderer: &str) -> Acceleration {
    let s = format!("{vendor} {renderer}").to_ascii_lowercase();
    if s.contains("llvmpipe")
        || s.contains("softpipe")
        || s.contains("swrast")
        || s.contains("software renderer")
        || s.contains("software rasterizer")
        || s.contains("swiftshader")
    {
        Acceleration::Software
    } else if vendor.is_empty() && renderer.is_empty() {
        Acceleration::Unknown
    } else {
        Acceleration::Gpu
    }
}

/// An fbconfig usable as a texture source for one particular X visual.
#[derive(Clone, Copy)]
struct TfpConfig {
    cfg: GLXFBConfig,
    /// `GLX_TEXTURE_FORMAT_RGB_EXT` or `GLX_TEXTURE_FORMAT_RGBA_EXT`.
    format: c_int,
    flip: bool,
    // Reported in the startup report only; the draw path never reads them.
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

/// The `GLX_TEXTURE_FORMAT_EXT` a pixmap of visual `want` is bound with.
///
/// Decided by the *visual*, never by the fbconfig: an ARGB visual is
/// sampled as RGBA and everything else as RGB, where the TFP spec
/// guarantees the sampler returns `a = 1.0` whatever alpha the config
/// carries. So this must agree with what [`rate_fbconfig`] demands the
/// chosen config be bindable as, or a window ends up bound through a
/// config that cannot serve the request.
fn tfp_texture_format(want: VisualFormat) -> c_int {
    if want.has_alpha() {
        GLX_TEXTURE_FORMAT_RGBA_EXT
    } else {
        GLX_TEXTURE_FORMAT_RGB_EXT
    }
}

/// Whether a texture from an fbconfig reporting `y_inverted` has to be
/// sampled y-flipped.
///
/// `GLX_Y_INVERTED_EXT == TRUE` puts the *top* of the drawable at `t = 0`,
/// which is already how `VERTEX_SRC` measures `u_src.y`, so only FALSE
/// needs a flip. Tested against 0 rather than against "not TRUE": such
/// servers measurably answer the out-of-spec `GLX_DONT_CARE` (-1), and
/// flipping on that would turn every window upside down.
fn tfp_flip(y_inverted: Option<c_int>) -> bool {
    y_inverted == Some(0)
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
/// This is where the compositor is made *screen-aware*: nothing about colour
/// depth or channel layout is assumed, all of it is read back from the X
/// `Setup` and the fbconfig. The rules, each with the failure it prevents:
///
///   * **Depth comes from the X visual table, never from `GLX_BUFFER_SIZE`.**
///     A depth-24 visual is stored as `x8r8g8b8`, so its fbconfig reports a
///     32-bit buffer with 8 alpha bits. Requiring `buffer_size == depth` finds
///     nothing on such a driver, and every ordinary window then silently
///     vanishes from the frame.
///   * **Channel widths must be at least the visual's, and are ranked on how
///     exactly they match.** `buffer_size == 32 && alpha != 0` also matches
///     `R10G10B10A2`, and binding an 8-bit-per-channel ARGB pixmap through a
///     10-bit config reinterprets the bits across channel boundaries: orange
///     `(255,128,64)` comes back as `(255,247,16)`.
///   * **Alpha bits on the *config* are not alpha in the *visual*.** For a
///     depth-24 visual we ask for `GLX_TEXTURE_FORMAT_RGB_EXT`, and the TFP spec
///     guarantees the sampler returns `a = 1.0` whatever the config carries, so
///     rejecting configs that merely *have* an alpha channel would leave
///     24-bit windows unbindable on drivers that only expose 32-bit ones.
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

/// The rectangle `glScissor` must be given for a damage rect of
/// `(x, y, w, h)` in top-left screen coordinates on a `width x height`
/// screen, returned as GL's own bottom-left `(x, y, w, h)`.
///
/// Every number here is decided by clamping and saturating arithmetic over
/// values a client can push arbitrarily out of range — a window being
/// resized hands us a rect from the screen it is *leaving* — and both
/// failure directions are silent: a box escaping the viewport is a GL
/// error, and a box that is too small leaves stale pixels in the frame.
///
/// `width`/`height` are narrowed to `i32` to clamp against, so a screen of
/// 2^31 pixels or more is outside what this accepts rather than supported.
fn scissor_box(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    width: u32,
    height: u32,
) -> (GLint, GLint, GLsizei, GLsizei) {
    let x0 = x.clamp(0, width as i32) as u32;
    let y0 = y.clamp(0, height as i32) as u32;
    let right = (x as i64 + w as i64).clamp(0, width as i64) as u32;
    let bottom = (y as i64 + h as i64).clamp(0, height as i64) as u32;
    let clipped_w = right.saturating_sub(x0);
    let clipped_h = bottom.saturating_sub(y0);
    (
        x0 as GLint,
        (height - clipped_h - y0) as GLint,
        clipped_w as GLsizei,
        clipped_h as GLsizei,
    )
}

/// Straight RGBA8 to premultiplied RGBA8 — the source convention the window
/// path's `(ONE, ONE_MINUS_SRC_ALPHA)` blend needs, and what X Render and
/// `GLX_EXT_texture_from_pixmap` already hand us for redirected windows.
///
/// Alpha is carried through untouched and each colour channel is scaled by
/// it and rounded to nearest, because alpha is what the blend later reads to
/// decide how much of the destination survives. The result holds one texel
/// per input pixel, so its length is the input's: `glTexImage2D` is handed
/// this buffer and reads `w * h * 4` bytes out of it.
fn premultiply_rgba(data: &[u8]) -> Vec<u8> {
    let mut premult = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(4) {
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
    premult
}

pub struct Renderer {
    dpy: XDisplay,
    screen: c_int,
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
    u_border_width: GLint,
    u_border_color: GLint,
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
    last_tex: TextureHandle,
    /// Current viewport size (set by `begin_frame`), reused by `draw_shader`'s
    /// vertex transform (clip space needs the full screen resolution).
    screen_w: u32,
    screen_h: u32,
    /// Whether vsync (swap interval 1) is actually in effect.
    pub vsync: bool,
    /// Whether `GLX_SGI_video_sync` is available. Retained purely as an
    /// instrumentation signal: its counter can measure missed vblanks. It must
    /// not drive pacing — swap interval 1 (see `vsync`) is the only
    /// synchroniser, and asking for a retrace on top of it would skip every
    /// other vblank.
    pub video_sync: bool,
    /// Structured renderer info for the compositor's startup log.
    pub info: RendererInfo,
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
        Self::new_with_vsync(
            dpy,
            screen,
            overlay,
            root_visual,
            visuals,
            (width, height),
            VsyncMode::On,
        )
    }

    /// Like [`Renderer::new`] but with an explicit vsync mode.
    ///
    /// On success the new context is current on `overlay` for the calling
    /// thread and the swap interval has already been applied to that drawable;
    /// every other method here assumes both.
    pub fn new_with_vsync(
        dpy: XDisplay,
        screen: i32,
        overlay: u32,
        root_visual: u32,
        visuals: &[VisualFormat],
        size: (u32, u32),
        vsync_mode: VsyncMode,
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

        let win_cfg = choose_window_fbconfig(&glx, d, screen, root_format)?;

        let ctx = unsafe {
            create_ctx(
                d,
                win_cfg,
                std::ptr::null_mut(),
                1,
                GLX_CTX_ATTRIBS.as_ptr(),
            )
        };
        // The context request is asynchronous; sync so a GLXBadFBConfig has
        // landed (and been swallowed by our silent handler) before we test.
        dpy.sync();
        if ctx.is_null() {
            return Err("glXCreateContextAttribsARB(3.3 core) failed".into());
        }
        if unsafe { (glx.glXIsDirect)(d, ctx) } == 0 {
            unsafe { (glx.glXDestroyContext)(d, ctx) };
            return Err("GLX compositor context is indirect".into());
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

        // `glXSwapBuffers` with swap interval 1 blocks until the vertical blank,
        // so a frame lands exactly once per refresh — no tearing on the moving
        // edge, and the loop paces itself for free (no spinning, no 16 ms guess).
        // This is the *single* synchroniser: nothing else may set a conflicting
        // interval, or the loop would skip vblanks.
        let vsync = enable_vsync(&glx, d, screen, glx_win, &exts, vsync_mode);

        // `GLX_SGI_video_sync` stays purely an instrumentation signal: its
        // counter can measure missed vblanks. The interval must not be touched
        // here — zeroing it would undo `enable_vsync` above.
        let video_sync = has_extension(&exts, "GLX_SGI_video_sync");

        let has_buffer_age =
            has_extension(&exts, "GLX_EXT_buffer_age") && glx.glXQueryDrawable.is_some();

        let mut r = Renderer {
            dpy,
            screen,
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
            u_border_width: -1,
            u_border_color: -1,
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
            last_tex: TextureHandle(0),
            vsync,
            video_sync,
            info: RendererInfo {
                backend: RendererBackend::OpenGlGlx,
                vendor: String::new(),
                renderer: String::new(),
                version: String::new(),
                accelerated: Acceleration::Unknown,
            },
            has_buffer_age,
            screen_w: 0,
            screen_h: 0,
        };

        if let Err(e) = r.init_gl_objects() {
            r.destroy();
            return Err(e);
        }

        let vendor = r.gl.get_string(GL_VENDOR);
        let renderer_str = r.gl.get_string(GL_RENDERER);
        let version = r.gl.get_string(GL_VERSION);
        r.info = RendererInfo {
            backend: RendererBackend::OpenGlGlx,
            vendor: vendor.clone(),
            renderer: renderer_str.clone(),
            version: version.clone(),
            accelerated: classify_acceleration(&vendor, &renderer_str),
        };
        let _ = size;
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
        self.u_border_width = uniform("u_border_width");
        self.u_border_color = uniform("u_border_color");

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
        self.last_tex = TextureHandle(0);
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
    pub fn set_scissor(&mut self, x: i32, y: i32, w: u32, h: u32, width: u32, height: u32) {
        let (sx, sy, sw, sh) = scissor_box(x, y, w, h, width, height);
        let gl = &self.gl;
        unsafe {
            (gl.glEnable)(GL_SCISSOR_TEST);
            (gl.glScissor)(sx, sy, sw, sh);
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
    pub fn draw(&mut self, tex: &mut Texture, q: &DrawQuad) {
        let gl = &self.gl;
        let handle = tex.handle();
        unsafe {
            if self.last_tex != handle {
                (gl.glBindTexture)(GL_TEXTURE_2D, tex.tex);
                self.last_tex = handle;
            }
            let filter = q.filter.to_gl();
            if tex.filter != q.filter {
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
                tex.filter = q.filter;
            }
            (gl.glUniform4f)(self.u_dst, q.dst[0], q.dst[1], q.dst[2], q.dst[3]);
            (gl.glUniform4f)(self.u_src, q.src[0], q.src[1], q.src[2], q.src[3]);
            (gl.glUniform2f)(self.u_size, q.size[0], q.size[1]);
            (gl.glUniform1f)(self.u_radius, q.radius);
            (gl.glUniform1f)(self.u_border_width, q.border_width);
            (gl.glUniform4f)(
                self.u_border_color,
                q.border_color[0],
                q.border_color[1],
                q.border_color[2],
                q.border_color[3],
            );
            (gl.glUniform1f)(self.u_opacity, q.opacity);
            (gl.glUniform1f)(self.u_flip, if tex.flip { 1.0 } else { 0.0 });
            (gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
        }
    }

    /// Draw a quad given a texture handle, the previously-bound texture id (for
    /// bind-cache elision), the TFP orientation, and the quad parameters. Used
    /// by the compositor's explicit-scene path, where the `Texture` itself stays
    /// owned by `CompWin` and only the handle travels in the `DrawItem`.
    pub fn draw_raw(
        &mut self,
        tex: TextureHandle,
        prev_tex: TextureHandle,
        flip: bool,
        q: &DrawQuad,
    ) -> TextureHandle {
        let gl = &self.gl;
        let inner = tex.0;
        let bound = if prev_tex.0 != inner {
            unsafe { (gl.glBindTexture)(GL_TEXTURE_2D, inner) };
            tex
        } else {
            prev_tex
        };
        let filter = q.filter.to_gl();
        unsafe {
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
            (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
            (gl.glUniform4f)(self.u_dst, q.dst[0], q.dst[1], q.dst[2], q.dst[3]);
            (gl.glUniform4f)(self.u_src, q.src[0], q.src[1], q.src[2], q.src[3]);
            (gl.glUniform2f)(self.u_size, q.size[0], q.size[1]);
            (gl.glUniform1f)(self.u_radius, q.radius);
            (gl.glUniform1f)(self.u_border_width, q.border_width);
            (gl.glUniform4f)(
                self.u_border_color,
                q.border_color[0],
                q.border_color[1],
                q.border_color[2],
                q.border_color[3],
            );
            (gl.glUniform1f)(self.u_opacity, q.opacity);
            (gl.glUniform1f)(self.u_flip, if flip { 1.0 } else { 0.0 });
            (gl.glDrawArrays)(GL_TRIANGLES, 0, 6);
        }
        bound
    }

    /// Present the frame. With swap interval 1 this blocks until the vertical
    /// blank, which is what paces the whole animation loop.
    pub fn end_frame(&mut self) -> bool {
        unsafe { (self.glx.glXSwapBuffers)(self.dpy.as_ptr(), self.glx_win) };
        self.gl.take_error() == GL_NO_ERROR
    }

    /// Block until the next vertical retrace (`GLX_SGI_video_sync`).
    ///
    /// Instrumentation only: it reads the vblank counter so a caller can
    /// measure missed retraces. It must not be used to pace the frame loop —
    /// the swap-interval-1 path in [`Renderer::end_frame`] is the sole
    /// synchroniser, and waiting on a retrace *as well* would skip every other
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
            maverick_x11::clear_x_error();
        }
        let glx_pixmap = unsafe {
            (self.glx.glXCreatePixmap)(
                self.dpy.as_ptr(),
                tfp.cfg,
                c_ulong::from(pixmap),
                attribs.as_ptr(),
            )
        };
        if glx_trace_enabled() {
            log::info!(
                "[GLX] create glxpixmap={} for_x_pixmap={} visual={}",
                glx_pixmap,
                pixmap,
                visual
            );
        }
        if verify {
            self.dpy.sync();
            if let Some(code) = maverick_x11::take_x_error() {
                self.verified.remove(&visual.id);
                if glx_pixmap != 0 {
                    unsafe {
                        (self.glx.glXDestroyPixmap)(self.dpy.as_ptr(), glx_pixmap);
                    }
                }
                return Err(format!(
                    "glXCreatePixmap for {visual} failed with {} ({})",
                    maverick_x11::x_error_name(code),
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
        self.last_tex = TextureHandle(tex);
        let mut t = Texture {
            glx_pixmap,
            tex,
            flip: tfp.flip,
            width,
            height,
            bound: false,
            // Must match the filter actually set above, or the first `draw`
            // would skip the update it needs.
            filter: Filter::Linear,
        };
        if let Err(e) = self.bind(&mut t) {
            self.destroy_texture(t);
            return Err(e);
        }
        if verify {
            self.dpy.sync();
            if let Some(code) = maverick_x11::take_x_error() {
                self.destroy_texture(t);
                self.verified.remove(&visual.id);
                return Err(format!(
                    "glXBindTexImageEXT for {visual} failed with {} ({tfp})",
                    maverick_x11::x_error_name(code),
                ));
            }
        }
        Ok(t)
    }

    /// (Re)bind the pixmap to its texture. Cheap — it is a driver-side rebind,
    /// not a copy — and mandatory after every damage event: the TFP spec leaves
    /// the texture contents *undefined* once the client has drawn into the
    /// drawable while it was bound.
    pub fn bind(&mut self, t: &mut Texture) -> Result<(), String> {
        let (Some(bind), Some(release)) =
            (self.glx.glXBindTexImageEXT, self.glx.glXReleaseTexImageEXT)
        else {
            return Err("GLX_EXT_texture_from_pixmap entry points are unavailable".into());
        };
        let d = self.dpy.as_ptr();
        let handle = t.handle();
        if glx_trace_enabled() {
            log::info!(
                "[GLX] bind glxpixmap={} texture={} was_bound={}",
                t.glx_pixmap,
                t.tex,
                t.bound
            );
        }
        unsafe {
            if self.last_tex != handle {
                (self.gl.glBindTexture)(GL_TEXTURE_2D, t.tex);
                self.last_tex = handle;
            }
            if t.bound {
                release(d, t.glx_pixmap, GLX_FRONT_LEFT_EXT);
                t.bound = false;
            }
            bind(d, t.glx_pixmap, GLX_FRONT_LEFT_EXT, std::ptr::null());
        }
        t.bound = true;
        let err = self.gl.take_error();
        if err != GL_NO_ERROR {
            t.bound = false;
            return Err(format!("glXBindTexImageEXT generated GL error 0x{err:x}"));
        }
        Ok(())
    }

    /// Delete a raw GL texture (one not backed by a GLX pixmap) created by
    /// `upload_rgba`. Does not touch any X resource. Used to release wallpaper
    /// image textures.
    pub fn destroy_raw(&mut self, tex: TextureHandle) {
        if tex.0 == 0 {
            return;
        }
        let gl = &self.gl;
        unsafe {
            if self.last_tex == tex {
                self.last_tex = TextureHandle(0);
            }
            (gl.glDeleteTextures)(1, &tex.0);
        }
    }

    /// Upload a decoded RGBA8 image to a GPU texture (straight → premultiplied, so
    /// the window-path premultiplied blend is already correct). Returns the
    /// texture handle; the caller owns it and must `destroy_raw` it. Errors (driver
    /// rejection, oversized) return `Err` with a clear message and free the
    /// half-created texture. An image larger than `GL_MAX_TEXTURE_SIZE` is
    /// rejected, never silently downscaled: the caller has to decide what the
    /// wallpaper should look like at that size.
    pub fn upload_rgba(&mut self, img: &Rgba8) -> Result<TextureHandle, String> {
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
        let premult = premultiply_rgba(&img.data);
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
            self.destroy_raw(TextureHandle(tex));
            return Err("glTexImage2D failed for wallpaper texture".into());
        }
        self.last_tex = TextureHandle(tex);
        Ok(TextureHandle(tex))
    }

    /// Compile a user GLSL fragment shader into a wallpaper program (combined with
    /// our unit-quad vertex shader). The fragment shader must declare the fixed
    /// contract uniforms `u_time` (float), `u_resolution` (vec2) and `u_delta_time`
    /// (float) and write its colour to `out vec4 frag`. On failure returns `Err`
    /// with the GL log (no panic) so the wallpaper can be disabled without taking
    /// down the compositor.
    pub fn compile_fragment(&mut self, frag: &str) -> Result<ShaderId, String> {
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
        Ok(ShaderId(prog))
    }

    /// `GL_MAX_TEXTURE_SIZE` as the driver reports it; a driver that answers
    /// nonsense (0 or negative) gets a conservative 4096 so the caller's
    /// oversize check still rejects something absurd.
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
    ///
    /// `s` must be the program this renderer currently holds — the one
    /// [`Renderer::compile_fragment`] linked last and
    /// [`Renderer::destroy_shader`] has not since dropped. The uniform locations
    /// are cached per program, so an earlier, already-replaced `ShaderId` would
    /// write its uniforms into whichever locations that program's own layout
    /// happened to leave at the cached indices.
    pub fn draw_shader(&mut self, s: ShaderId, out: Rect, time: f32, dt: f32) {
        let gl = &self.gl;
        unsafe {
            (gl.glUseProgram)(s.0);
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
    pub fn destroy_shader(&mut self, shader: ShaderId) {
        if shader.0 == 0 {
            return;
        }
        unsafe {
            (self.gl.glDeleteProgram)(shader.0);
        }
        if self.wp_prog == shader.0 {
            self.wp_prog = 0;
            self.wp_u_dst = -1;
            self.wp_u_res = -1;
            self.wp_u_time = -1;
            self.wp_u_resolution = -1;
            self.wp_u_delta_time = -1;
        }
    }

    /// Release a bound pixmap texture: `glXReleaseTexImageEXT` if it is still
    /// bound, then the GLX pixmap, then the GL texture name. The X pixmap the
    /// GLXPixmap was created from is *not* touched — the caller frees that.
    /// Requires the renderer's context to still be current.
    pub fn destroy_texture(&mut self, mut t: Texture) {
        let d = self.dpy.as_ptr();
        if glx_trace_enabled() {
            log::info!(
                "[GLX] destroy glxpixmap={} texture={} was_bound={}",
                t.glx_pixmap,
                t.tex,
                t.bound
            );
        }
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
        self.last_tex = TextureHandle(0);
    }

    fn tfp_config(&mut self, visual: VisualFormat) -> Result<TfpConfig, String> {
        // A *failed* lookup is deliberately not cached. A visual whose fbconfig
        // negotiation fails once (a transient `BadMatch`, an X server still
        // initialising, a GLX race during a resize storm) must be retried on the
        // next pixmap, not frozen into a permanent negative cache that would
        // silently drop every window of that visual for the rest of the session.
        // Only a successful config is cached.
        if let Some(Ok(hit)) = self.tfp_cache.get(&visual.id) {
            return Ok(*hit);
        }
        let found = choose_tfp_fbconfig(
            &self.glx,
            self.dpy.as_ptr(),
            self.screen,
            &self.visuals,
            visual,
        );
        if found.is_ok() {
            self.tfp_cache.insert(visual.id, found.clone());
        }
        found
    }

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
        unsafe { maverick_x11::XFree(list.cast()) };
        out
    }

    /// Drop the GL context and its drawable. Called when the compositor is
    /// disabled at runtime (a GL failure) and on shutdown. Textures must have
    /// been destroyed first.
    ///
    /// The order is load-bearing: the programs, VBO and VAO are deleted while
    /// the context is still current, and only then is the context released and
    /// destroyed — `glXDestroyContext` rejects a context that is still current
    /// on any drawable.
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

/// Apply `mode` to `drawable` and report whether vsync ends up in effect.
///
/// Only extensions actually present in `exts` (the server's GLX extension
/// string) are used: a non-`None` [`Glx`] field merely means libGL exported the
/// symbol.
fn enable_vsync(
    glx: &Glx,
    d: *mut maverick_x11::Display,
    screen: c_int,
    drawable: GLXDrawable,
    exts: &str,
    mode: VsyncMode,
) -> bool {
    let interval: c_int = match mode {
        VsyncMode::Adaptive => {
            if has_extension(exts, "GLX_EXT_swap_control_tear") {
                -1
            } else {
                1
            }
        }
        VsyncMode::On => 1,
        VsyncMode::Off => 0,
    };
    // `glXSwapIntervalEXT` is provided by EXT_swap_control and, for the
    // negative interval, by EXT_swap_control_tear. Try it before the
    // interval-1-only MESA/SGI fallbacks, including for `Off` so the setting
    // actually disables an inherited interval.
    if has_extension(exts, "GLX_EXT_swap_control")
        || (interval == -1 && has_extension(exts, "GLX_EXT_swap_control_tear"))
    {
        if let Some(f) = glx.glXSwapIntervalEXT {
            unsafe { f(d, drawable, interval) };
            return interval != 0;
        }
    }
    // MESA/SGI only support interval 1 (or 0 for MESA), not -1.
    if interval == -1 {
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
        return false;
    }
    if has_extension(exts, "GLX_MESA_swap_control") {
        if let Some(f) = glx.glXSwapIntervalMESA {
            return unsafe { f(interval as c_uint) } == 0 && interval != 0;
        }
    }
    if has_extension(exts, "GLX_SGI_swap_control") {
        if let Some(f) = glx.glXSwapIntervalSGI {
            return unsafe { f(interval) } == 0 && interval != 0;
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
/// Deliberately **not** `glXChooseFBConfig`: its "at least" attribute form
/// (`GLX_RED_SIZE >= 8, GLX_GREEN_SIZE >= 8, ...`) excludes every fbconfig on a
/// 15- or 16-bit screen (`R5G6B5`) and would make the compositor refuse to
/// start there for no reason. The only hard requirement is the visual id, so we
/// enumerate and filter on that, and report precisely what was missing.
fn choose_window_fbconfig(
    glx: &Glx,
    d: *mut maverick_x11::Display,
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
    unsafe { maverick_x11::XFree(list.cast()) };
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
    d: *mut maverick_x11::Display,
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
                        format: tfp_texture_format(want),
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
                        // flip and FALSE is the one that does: test for `0`,
                        // not for "not TRUE". A server that answers the
                        // out-of-spec `-1` (`GLX_DONT_CARE`) is treated as the
                        // common TRUE case, which is what such servers
                        // measurably do.
                        flip: tfp_flip(fb.y_inverted),
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
    unsafe { maverick_x11::XFree(list.cast()) };
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

    /// A 10-bit config must never serve an 8-bit-per-channel ARGB visual.
    /// `GLX_BUFFER_SIZE == 32 && alpha != 0` matches `R10G10B10A2` — 10+10+10+2
    /// is also 32 — and binding through it reinterprets the bits across channel
    /// boundaries: orange (255,128,64) reads back as (255,247,16).
    #[test]
    fn argb32_never_binds_through_a_10bit_config() {
        let configs = [
            // Mesa lists the deep-colour, visual-less config first, so the wrong
            // one is offered before the right one and must still lose.
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

    /// A depth-24 visual's fbconfig legitimately reports a **32-bit** buffer
    /// with 8 alpha bits, because `x8r8g8b8` is how the server stores it.
    /// Requiring `buffer_size == depth`, or refusing configs that merely have
    /// an alpha channel, finds nothing at all — and every ordinary window then
    /// silently disappears from the frame.
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

    /// `GLX_DONT_CARE` (-1) for the bind targets means "unspecified", not
    /// "no GL_TEXTURE_2D": reading it as the latter disables compositing on
    /// those servers entirely.
    #[test]
    fn dont_care_bind_targets_are_usable() {
        let mut cfg = fb(RGB24.id, Some(24), [8, 8, 8, 8]);
        // What `choose_tfp_fbconfig` derives from -1 / no answer at all.
        cfg.target_2d = true;
        assert!(rate_fbconfig(RGB24, &cfg).is_ok());
        cfg.target_2d = false; // a server that really does say "no 2D"
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

    #[test]
    fn software_rasterizers_classify_as_software() {
        assert_eq!(
            classify_acceleration("Mesa", "llvmpipe (LLVM 15.0.7, 256 bits)"),
            Acceleration::Software
        );
        assert_eq!(
            classify_acceleration("Red Hat", "softpipe"),
            Acceleration::Software
        );
        assert_eq!(classify_acceleration("", "swrast"), Acceleration::Software);
        assert_eq!(
            classify_acceleration("VMware, Inc.", "Software Renderer"),
            Acceleration::Software
        );
        assert_eq!(
            classify_acceleration("Google", "Software Rasterizer"),
            Acceleration::Software
        );
        assert_eq!(
            classify_acceleration("X.Org", "swiftshader"),
            Acceleration::Software
        );
    }

    /// No vendor-name guessing: the classification keys off software-renderer
    /// markers only, so every real GPU reports `Gpu` whatever it is called.
    #[test]
    fn real_gpus_classify_as_gpu_regardless_of_vendor() {
        // Intel iGPUs in particular must not be taken for llvmpipe.
        assert_eq!(
            classify_acceleration("Intel", "Mesa Intel(R) HD Graphics 630 (KBL GT2)"),
            Acceleration::Gpu
        );
        assert_eq!(
            classify_acceleration("NVIDIA Corporation", "NVIDIA GeForce GTX 1060/PCIe/SSE2"),
            Acceleration::Gpu
        );
        assert_eq!(
            classify_acceleration("X.Org", "AMD Radeon RX 580 (POLARIS10, DRM 3.49)"),
            Acceleration::Gpu
        );
    }

    #[test]
    fn empty_renderer_info_classifies_unknown() {
        assert_eq!(classify_acceleration("", ""), Acceleration::Unknown);
    }

    #[test]
    fn renderer_info_display_matches_startup_block() {
        let info = RendererInfo {
            backend: RendererBackend::OpenGlGlx,
            vendor: "Intel".into(),
            renderer: "Mesa Intel(R) HD Graphics 630".into(),
            version: "4.6 (Core Profile)".into(),
            accelerated: Acceleration::Gpu,
        };
        assert_eq!(
            info.to_string(),
            "Compositor:\n  Backend: OpenGL/GLX\n  Vendor: Intel\n  \
             Renderer: Mesa Intel(R) HD Graphics 630\n  Version: 4.6 (Core Profile)\n  \
             Acceleration: GPU\n"
        );
    }

    #[test]
    fn filter_maps_to_gl_constants() {
        assert_eq!(Filter::Nearest.to_gl(), GL_NEAREST);
        assert_eq!(Filter::Linear.to_gl(), GL_LINEAR);
        assert_eq!(Filter::default(), Filter::Nearest);
    }
}

/// Properties of the decisions that have to hold for *any* visual, fbconfig
/// or pixel buffer, and not only for the handful a real screen happens to
/// offer: which fbconfig may serve which visual and how they rank, the
/// arithmetic of the damage clip, the premultiply the blend depends on, and
/// the shape of the lists handed across the FFI boundary.
///
/// Every input here is a plain value. Nothing in this module needs a GL
/// context, a GLX connection or an X display, which is the only reason these
/// invariants can be checked at all on a build machine.
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// A flag a working screen mostly sets, so generated configs do not spend
    /// every case on a rejection the interesting rules never reach.
    fn likely() -> impl Strategy<Value = bool> {
        prop_oneof![9 => Just(true), 1 => Just(false)]
    }

    /// A flag a real screen is genuinely split on.
    fn evenly() -> impl Strategy<Value = bool> {
        any::<bool>()
    }

    /// A screen dimension. `i32::MAX` is in because it is the largest size the
    /// scissor clamp's narrowing to `i32` still describes, and 0 because a
    /// compositor is handed an empty screen before the first monitor is known.
    fn screen_dim() -> impl Strategy<Value = u32> {
        prop_oneof![
            Just(0),
            Just(1),
            2u32..=16384,
            0x7FFF_FF00u32..=i32::MAX as u32
        ]
    }

    /// A client-supplied coordinate, including the extremes a resize produces:
    /// a window leaving a larger screen has negative coordinates, and one
    /// arriving from off-screen can be at any distance.
    fn coord() -> impl Strategy<Value = i32> {
        prop_oneof![
            Just(i32::MIN),
            Just(i32::MAX),
            Just(-1),
            Just(0),
            Just(1),
            -1_000_000i32..=1_000_000
        ]
    }

    /// A client-supplied extent, where `u32::MAX` stands in for the "as big as
    /// the damage rect will ever be" a compositor computes with an
    /// underflowing subtraction.
    fn extent() -> impl Strategy<Value = u32> {
        prop_oneof![Just(0), Just(1), Just(u32::MAX), 0u32..=1_000_000]
    }

    /// Whole RGBA texels, which is the only shape `upload_rgba` is given: an
    /// `Rgba8` whose buffer holds exactly `w * h * 4` bytes.
    fn arb_pixels() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(prop::array::uniform4(any::<u8>()), 0..=16)
            .prop_map(|pixels| pixels.into_iter().flatten().collect())
    }

    proptest! {
        /// Whatever the damage rect, the box handed to `glScissor` lies
        /// inside the viewport.
        ///
        /// This is the one thing that must never break: a scissor box that
        /// leaves the viewport is a GL error, and a box that is merely wrong
        /// leaves the undamaged part of the previous frame on screen, which
        /// reads as a compositor that "forgets" to repaint. The rects reach
        /// `set_scissor` straight from client geometry during a resize, so
        /// clipping rather than trusting the caller is what keeps the box in
        /// range — including for the rect of a screen that no longer exists.
        #[test]
        fn scissor_box_never_leaves_the_viewport(
            width in screen_dim(),
            height in screen_dim(),
            x in coord(),
            y in coord(),
            w in extent(),
            h in extent(),
        ) {
            let (sx, sy, sw, sh) = scissor_box(x, y, w, h, width, height);
            prop_assert!(sx >= 0, "x origin {sx} is left of the screen");
            prop_assert!(sy >= 0, "y origin {sy} is below the screen");
            prop_assert!(sw >= 0 && sh >= 0, "negative size ({sw}, {sh})");
            prop_assert!(
                i64::from(sx) + i64::from(sw) <= i64::from(width),
                "x extent {} overruns a {width}-wide screen",
                i64::from(sx) + i64::from(sw)
            );
            prop_assert!(
                i64::from(sy) + i64::from(sh) <= i64::from(height),
                "y extent {} overruns a {height}-tall screen",
                i64::from(sy) + i64::from(sh)
            );
        }

        /// A damage rect that is already inside the screen reaches GL
        /// unchanged, y origin included.
        ///
        /// Clipping may only shrink a rect, never move it, and a partial
        /// redraw has to clear exactly the rows the rect named. The y flip is
        /// the part that fails silently: the compositor counts damage from the
        /// top and GL scissors from the bottom, and an origin one row off
        /// leaves a band of the previous frame behind with no error anywhere.
        #[test]
        fn scissor_box_passes_an_interior_damage_rect_through_unchanged(
            x in 0i32..=4096,
            y in 0i32..=4096,
            w in 0u32..=4096,
            h in 0u32..=4096,
            spare_x in 0u32..=64,
            spare_y in 0u32..=64,
        ) {
            // The screen is grown past the rect, so it is interior by
            // construction and clipping has nothing left to do.
            let width = x as u32 + w + spare_x;
            let height = y as u32 + h + spare_y;
            prop_assert_eq!(
                scissor_box(x, y, w, h, width, height),
                (
                    x,
                    height as GLint - y as GLint - h as GLint,
                    w as GLsizei,
                    h as GLsizei,
                )
            );
        }

        /// Asking for a bigger damaged region never clips down to a smaller
        /// one, on any screen.
        ///
        /// The compositor derives the damage rect of a resize from the union
        /// of the old and the new geometry and relies on the clip being
        /// monotonic to clear that whole union in one pass. A box that shrank
        /// as the request grew would leave the new part of a window showing
        /// the frame before it moved.
        #[test]
        fn scissor_box_never_shrinks_as_the_damage_grows(
            width in screen_dim(),
            height in screen_dim(),
            x in coord(),
            y in coord(),
            w in extent(),
            h in extent(),
            grow in extent(),
            grow_h in extent(),
        ) {
            let (bx, by, bw, bh) = scissor_box(x, y, w, h, width, height);
            let (wx, _, ww, _) =
                scissor_box(x, y, w.saturating_add(grow), h, width, height);
            let (_, ty, _, th) = scissor_box(x, y, w, h.saturating_add(grow_h), width, height);
            prop_assert!(ww >= bw, "clip width fell for a wider request");
            prop_assert!(th >= bh, "clip height fell for a taller request");
            // The left edge does not move, and the top edge (GL's origin plus
            // its height, since GL counts from the bottom) never moves up: a
            // growing request may only ever claim more rows.
            prop_assert_eq!(wx, bx, "the left edge moved for a wider request");
            prop_assert!(ty as i64 + th as i64 >= by as i64 + bh as i64, "the top edge moved up");
            // A bigger screen can only leave more of the request intact. The
            // grown size stays inside the range the clamp narrows to, or the
            // screen itself would be out of what this accepts.
            let (_, _, sw, sh) = scissor_box(
                x,
                y,
                w,
                h,
                width.saturating_add(grow).min(i32::MAX as u32),
                height.saturating_add(grow_h).min(i32::MAX as u32),
            );
            prop_assert!(sw >= bw && sh >= bh, "a bigger screen clipped more");
        }

        /// A visual reports the sum of the channel widths it actually carries,
        /// wide enough to hold three of them.
        ///
        /// The figure is what the compositor compares a client's declared depth
        /// against, and a sum that wrapped in `u8` would claim an ARGB visual
        /// has 8 colour bits and turn down every configuration on the screen.
        #[test]
        fn visual_colour_bits_are_the_wide_sum_of_its_channels(
            r in any::<u8>(),
            g in any::<u8>(),
            b in any::<u8>(),
            a in any::<u8>(),
        ) {
            let v = VisualFormat {
                id: 0x21,
                depth: 24,
                red_bits: r,
                green_bits: g,
                blue_bits: b,
                alpha_bits: a,
                direct: true,
            };
            prop_assert_eq!(v.color_bits(), u32::from(r) + u32::from(g) + u32::from(b));
            prop_assert!(v.color_bits() <= 3 * 255, "more colour than three 8-bit channels");
            prop_assert_eq!(v.has_alpha(), a > 0);
        }
    }

    /// The channel width one component of a visual carries: the sizes X really
    /// reports, from a stub entry with none to a 16-bit-per-channel screen.
    fn channel_width() -> impl Strategy<Value = u8> {
        prop_oneof![0u8..=1, 4u8..=6, 8u8..=10, 15u8..=16, 32u8..=32]
    }

    /// A visual the X `Setup` could really report: `depth` consistent with the
    /// channel widths, as `alpha_bits = depth - (r + g + b)` documents. The
    /// widths are the sizes X actually uses, so the comparisons in
    /// [`rate_fbconfig`] run across the whole range instead of only at 8/8/8.
    fn arb_visual() -> impl Strategy<Value = VisualFormat> {
        (
            channel_width(),
            channel_width(),
            channel_width(),
            prop_oneof![0u8..=1, 8u8..=8, 10u8..=10, 16u8..=16],
            any::<u32>(),
        )
            .prop_map(|(r, g, b, a, id)| VisualFormat {
                id,
                depth: r + g + b + a,
                red_bits: r,
                green_bits: g,
                blue_bits: b,
                alpha_bits: a,
                direct: true,
            })
    }

    /// The channel width a real fbconfig reports where the visual needs
    /// `bits`: often exactly that, sometimes another. Widths above the
    /// visual's are generated too, because a wider config is one the driver
    /// widens rather than invents, and that case has to keep being accepted.
    fn arb_width_for(bits: u8) -> impl Strategy<Value = c_int> {
        prop_oneof![3 => Just(c_int::from(bits)), 2 => 0..=48]
    }

    /// A visual paired with an fbconfig, drawn so that "this config serves
    /// this visual" and "it does not" are both common: a real screen offers a
    /// mix of both, and the decision has to come out right in either
    /// direction — too strict and every window vanishes, too lax and its
    /// colours are reinterpreted.
    fn arb_pair() -> impl Strategy<Value = (VisualFormat, FbAttrs)> {
        arb_visual().prop_flat_map(|want| {
            let other_depth = (0u8..=112).prop_filter("a depth of its own", {
                let taken = want.depth;
                move |d| *d != taken
            });
            let depth = prop_oneof![
                6 => Just(Some(want.depth)),
                2 => Just(None),
                2 => other_depth.prop_map(Some)
            ];
            let widths = (
                arb_width_for(want.red_bits),
                arb_width_for(want.green_bits),
                arb_width_for(want.blue_bits),
                arb_width_for(want.alpha_bits),
            );
            // Pixmap-renderable, RGBA-renderable, bind RGB, bind RGBA, 2D
            // target, caveat-free.
            let caps = (likely(), likely(), evenly(), evenly(), likely(), evenly());
            (depth, widths, caps).prop_map(move |(visual_depth, (r, g, b, a), caps)| {
                let rgba = [r, g, b, a];
                let fb = FbAttrs {
                    visual: want.id,
                    visual_depth,
                    pixmap_renderable: caps.0,
                    rgba_render: caps.1,
                    rgba,
                    // Deliberately the sum of the channel widths, i.e. what
                    // GLX_BUFFER_SIZE reports for a real config — including the
                    // depth-24 visual served by a 32-bit buffer, which is the
                    // case a `buffer_size == depth` rule would turn down.
                    buffer_size: rgba.iter().sum(),
                    bind_rgb: caps.2,
                    bind_rgba: caps.3,
                    target_2d: caps.4,
                    caveat_free: caps.5,
                    y_inverted: Some(1),
                };
                (want, fb)
            })
        })
    }

    /// The five things a config can match the visual on, in the order the
    /// rules rank them: the visual itself, then its depth, then the exact
    /// colour widths, then alpha, then the absence of a rendering caveat.
    fn match_key(fb: &FbAttrs, want: VisualFormat) -> [bool; 5] {
        [
            fb.visual == want.id,
            fb.visual_depth == Some(want.depth),
            fb.rgba[0] == c_int::from(want.red_bits)
                && fb.rgba[1] == c_int::from(want.green_bits)
                && fb.rgba[2] == c_int::from(want.blue_bits),
            fb.rgba[3] == c_int::from(want.alpha_bits),
            fb.caveat_free,
        ]
    }

    /// A config accepted for `want` whatever the ranking says of it: the
    /// visual's own channel widths widened by at most 8 bits each, a depth
    /// that is either the visual's own or absent, and the bind capability the
    /// visual needs. These are the shapes one screen's GLX list really has.
    fn arb_acceptable(want: VisualFormat) -> impl Strategy<Value = FbAttrs> {
        let alpha_needed = want.has_alpha();
        let parts = (
            prop_oneof![4 => Just(want.id), 1 => 0u32..=u32::MAX],
            prop_oneof![4 => Just(Some(want.depth)), 1 => Just(None)],
            (0u8..=8, 0u8..=8, 0u8..=8, 0u8..=8),
            evenly(),
        );
        parts.prop_map(move |(visual, visual_depth, (dr, dg, db, da), caveat)| {
            FbAttrs {
                visual,
                visual_depth,
                pixmap_renderable: true,
                rgba_render: true,
                rgba: [
                    c_int::from(want.red_bits) + c_int::from(dr),
                    c_int::from(want.green_bits) + c_int::from(dg),
                    c_int::from(want.blue_bits) + c_int::from(db),
                    c_int::from(want.alpha_bits) + c_int::from(da),
                ],
                buffer_size: 32,
                // A real config advertises whichever formats it can bind; the
                // one this visual needs is always among them, the other is
                // what actually varies between drivers.
                bind_rgb: !alpha_needed,
                bind_rgba: alpha_needed,
                target_2d: true,
                caveat_free: caveat,
                y_inverted: Some(1),
            }
        })
    }

    /// What an fbconfig reports for `GLX_Y_INVERTED_EXT`: nothing, the two
    /// documented values, the out-of-spec `GLX_DONT_CARE` such servers really
    /// answer, and anything else a driver might invent.
    fn arb_y_inverted() -> impl Strategy<Value = Option<c_int>> {
        prop_oneof![
            1 => Just(None),
            3 => Just(Some(0)),
            3 => Just(Some(1)),
            2 => Just(Some(GLX_DONT_CARE)),
            2 => Just(Some(-2)),
            2 => any::<c_int>().prop_map(Some)
        ]
    }

    /// One visual and two configs that can both serve it, for the ranking.
    fn arb_ranking() -> impl Strategy<Value = (VisualFormat, FbAttrs, FbAttrs)> {
        arb_visual().prop_flat_map(|want| {
            (arb_acceptable(want), arb_acceptable(want)).prop_map(move |(a, b)| (want, a, b))
        })
    }

    proptest! {
        /// A config is accepted exactly when it can carry the visual: it must
        /// be able to render a pixmap as RGBA, offer at least the visual's
        /// colour bits (and its alpha, when it has any), not disagree with
        /// its own visual's depth, be bindable in the format the pixmap will be
        /// requested in, and offer a `GL_TEXTURE_2D` target.
        ///
        /// Both directions are contractual. Accepting too much quantises every
        /// window or reinterprets its channels; rejecting too much is the
        /// failure this function exists to avoid — a depth-24 visual on a
        /// driver whose only configs are 32-bit finds *nothing*, and then
        /// every ordinary window silently vanishes from the frame.
        #[test]
        fn fbconfig_acceptance_is_exactly_what_the_visual_requires(pair in arb_pair()) {
            let (want, fb) = pair;
            let can_carry = fb.pixmap_renderable
                && fb.rgba_render
                && fb.rgba[0] >= c_int::from(want.red_bits)
                && fb.rgba[1] >= c_int::from(want.green_bits)
                && fb.rgba[2] >= c_int::from(want.blue_bits)
                && (!want.has_alpha() || fb.rgba[3] >= c_int::from(want.alpha_bits))
                && fb.visual_depth.is_none_or(|d| d == want.depth)
                && fb.target_2d
                && (if tfp_texture_format(want) == GLX_TEXTURE_FORMAT_RGBA_EXT {
                    fb.bind_rgba
                } else {
                    fb.bind_rgb
                });
            prop_assert_eq!(
                rate_fbconfig(want, &fb).is_ok(),
                can_carry,
                "visual {} against {:?}", want, fb
            );
        }

        /// Of two configs that can both serve the visual, the better match
        /// always scores higher, and two matching equally well score the same.
        ///
        /// The bonus weights exist for this: each is worth more than all the
        /// ones below it together, so the documented order of preference
        /// survives no matter what the two configs differ in besides. Lose it
        /// and the compositor stops preferring the visual it was handed, and
        /// takes a neighbouring one whose channel layout then reinterprets
        /// every colour of every window on that visual.
        #[test]
        fn accepted_fbconfigs_rank_by_how_well_they_match(
            (want, a, b) in arb_ranking()
        ) {
            let sa = rate_fbconfig(want, &a).expect("generated an acceptable config");
            let sb = rate_fbconfig(want, &b).expect("generated an acceptable config");
            let ka = match_key(&a, want);
            let kb = match_key(&b, want);
            prop_assert!(sa >= 0, "a usable config never scores below zero");
            // `[bool; 5]` compares in the documented order of preference, so
            // this says the score ranks two configs exactly the way the rules
            // say they should rank, and that two matching equally well score
            // the same. A weight that lost its place would show up here as a
            // disagreement between the two orders.
            prop_assert_eq!(
                ka.cmp(&kb),
                sa.cmp(&sb),
                "{:?} against {:?} ranked the other way round: {} against {}", ka, kb, sa, sb
            );
        }

        /// Whatever the ranking picks is bindable in the format the pixmap
        /// attribute list will ask for.
        ///
        /// The two live apart: `texture_from_pixmap` builds
        /// `[GLX_TEXTURE_TARGET_EXT, …, GLX_TEXTURE_FORMAT_EXT, format, 0]`
        /// from the *visual*, while the config is chosen from what it
        /// advertises. If the two ever disagree about the format an ARGB
        /// visual needs, every compositing client is bound through a config
        /// that cannot serve the request — unbindable, not merely wrong.
        #[test]
        fn a_pixmap_is_only_ever_bound_in_a_format_its_config_can_bind(
            pair in arb_pair()
        ) {
            let (want, fb) = pair;
            let format = tfp_texture_format(want);
            prop_assert!(
                matches!(format, GLX_TEXTURE_FORMAT_RGB_EXT | GLX_TEXTURE_FORMAT_RGBA_EXT),
                "asked for format {format}, which is not a TFP texture format"
            );
            if rate_fbconfig(want, &fb).is_ok() {
                if format == GLX_TEXTURE_FORMAT_RGBA_EXT {
                    prop_assert!(fb.bind_rgba, "asked RGBA of a config that only binds RGB");
                } else {
                    prop_assert!(fb.bind_rgb, "asked RGB of a config that only binds RGBA");
                }
            }
        }

        /// Only an fbconfig that reports `GLX_Y_INVERTED_EXT` as FALSE needs
        /// its texture sampled y-flipped.
        ///
        /// TRUE puts the top of the drawable at `t = 0`, which is already how
        /// the vertex shader measures its source rect, so flipping there
        /// renders every window upside down. A server answering the
        /// out-of-spec `GLX_DONT_CARE` (-1) is treated as the common TRUE
        /// case, because testing for "not TRUE" instead flips every window on
        /// exactly those servers.
        #[test]
        fn only_y_inverted_false_needs_a_flip(y in arb_y_inverted()) {
            prop_assert_eq!(tfp_flip(y), y == Some(0));
        }

        /// The tally behind "no fbconfig binds this visual" reports every
        /// reason that was found, once, with its count, and nothing for a
        /// reason that was not.
        ///
        /// This string is the only thing a user sees when compositing is off,
        /// so a counter wired to the wrong reason sends them after a colour
        /// problem they do not have, and a reason missing from the list reads
        /// as "none" on a screen that was full of them.
        #[test]
        fn reject_tally_reports_each_found_reason_with_its_count(
            picks in prop::collection::vec(0usize..7, 1..24)
        ) {
            const ALL: [Reject; 7] = [
                Reject::NotPixmap,
                Reject::NotRgba,
                Reject::TooFewBits,
                Reject::NoAlpha,
                Reject::DepthMismatch,
                Reject::NotBindable,
                Reject::No2dTarget,
            ];
            let mut tally = Rejects::default();
            for i in &picks {
                tally.note(ALL[*i]);
            }
            // Each reason paired with the count the report must show for it.
            let labelled: [(&str, usize); 7] = [
                ("not pixmap-renderable", tally.not_pixmap),
                ("not RGBA", tally.not_rgba),
                ("fewer colour bits than the visual", tally.too_few_bits),
                ("no alpha channel", tally.no_alpha),
                ("wrong visual depth", tally.depth_mismatch),
                ("not bindable as a texture", tally.not_bindable),
                ("no GL_TEXTURE_2D target", tally.no_2d_target),
            ];
            let report = tally.to_string();
            let mut listed = 0;
            for (label, n) in labelled {
                prop_assert_eq!(
                    report.contains(&format!("{n} {label}")),
                    n > 0,
                    "{} was recorded {} times but the report reads {:?}", label, n, report
                );
                listed += usize::from(n > 0);
            }
            prop_assert_eq!(
                report.split(", ").count(),
                listed.max(1),
                "wrong number of reasons in {:?}", report
            );
            prop_assert_eq!(listed == 0, report == "none", "an empty tally reads {:?}", report);
        }

        /// The premultiply keeps one texel per pixel and carries alpha
        /// through untouched.
        ///
        /// `glTexImage2D` is handed this buffer and reads exactly `w * h * 4`
        /// bytes out of it, so a buffer of the wrong length is a read past its
        /// end or a frame of garbage. Alpha is the channel the blend later
        /// reads to decide how much of the destination survives, so it has to
        /// arrive unchanged: premultiplying it as well would darken the window
        /// twice over.
        #[test]
        fn premultiplied_upload_keeps_one_texel_per_pixel_and_the_source_alpha(
            data in arb_pixels()
        ) {
            let out = premultiply_rgba(&data);
            prop_assert_eq!(out.len(), data.len());
            for (i, px) in data.chunks_exact(4).enumerate() {
                prop_assert_eq!(out[i * 4 + 3], px[3], "alpha was scaled at pixel {}", i);
            }
        }

        /// No premultiplied channel is brighter than the alpha it was scaled
        /// by, and an opaque source survives untouched.
        ///
        /// The blend is `dst = src + dst * (1 - src.a)`, which assumes colour
        /// is already counted inside alpha: a channel above its own alpha is
        /// brighter than fully covered and haloes every edge. Opaque is the
        /// identity case, because X Render's unpremultiply is the inverse —
        /// a premultiply that altered it would round-trip a solid colour into
        /// a different one.
        #[test]
        fn premultiplied_channels_never_exceed_their_alpha(data in arb_pixels()) {
            let out = premultiply_rgba(&data);
            for (i, px) in data.chunks_exact(4).enumerate() {
                for c in 0..3 {
                    prop_assert!(
                        out[i * 4 + c] <= out[i * 4 + 3],
                        "pixel {} channel {}: {} is brighter than its alpha {}",
                        i,
                        c,
                        out[i * 4 + c],
                        out[i * 4 + 3]
                    );
                }
                if px[3] == u8::MAX {
                    for c in 0..3 {
                        prop_assert_eq!(
                            out[i * 4 + c],
                            px[c],
                            "an opaque pixel must survive premultiplying"
                        );
                    }
                }
            }
        }

        /// Each premultiplied channel is the nearest integer to
        /// `channel * alpha / 255`, and never goes down as either factor goes
        /// up.
        ///
        /// The quantisation a premultiply introduces is part of what a
        /// compositor's independent look is calibrated against: truncating
        /// instead of rounding biases every semi-transparent pixel a half-step
        /// dark, and a scale that is not monotone in the channel bands smooth
        /// gradients the caller passed in untouched.
        #[test]
        fn premultiplied_channels_are_rounded_to_nearest_and_monotone(
            a in any::<u8>(),
            c0 in any::<u8>(),
        ) {
            let channel = |c: u8, a: u8| premultiply_rgba(&[c, c, c, a])[0];
            for c in 0..=u8::MAX {
                let p = channel(c, a);
                let exact = f64::from(c) * f64::from(a) / 255.0;
                prop_assert!(
                    (f64::from(p) - exact).abs() <= 0.5,
                    "channel {c} at alpha {a}: {p} is not the nearest to {exact}"
                );
                if c > 0 {
                    prop_assert!(p >= channel(c - 1, a), "channel {c} is darker than {}", c - 1);
                }
            }
            for al in 0..=u8::MAX {
                let p = channel(c0, al);
                if al > 0 {
                    prop_assert!(
                        p >= channel(c0, al - 1),
                        "alpha {al} darkened channel {c0}"
                    );
                }
            }
        }

        /// A CPU-uploaded texture is nothing but the handle it was given: no
        /// GLX pixmap behind it, no y-flip, not bound, and a filter cache that
        /// agrees with the `GL_LINEAR` `upload_rgba` set on the object.
        ///
        /// `destroy_texture` releases a GLX pixmap unconditionally and every
        /// damage event releases before binding, so a CPU texture claiming to
        /// have one has the renderer free an X resource it never created — and
        /// a flip it does not need turns the wallpaper upside down. The filter
        /// cache matters just as quietly: it is compared against the filter a
        /// draw asks for, so a cache that disagrees makes the first draw skip
        /// the `glTexParameteri` the driver still needs.
        #[test]
        fn a_cpu_texture_is_only_the_handle_it_was_given(
            tex in any::<u32>(),
            w in any::<u16>(),
            h in any::<u16>(),
        ) {
            let t = Texture::new_cpu(tex, w, h);
            prop_assert_eq!(t.handle(), TextureHandle(tex));
            prop_assert_eq!(t.tex, tex);
            prop_assert_eq!(t.glx_pixmap, 0, "a CPU texture owns no X pixmap");
            prop_assert!(!t.flip, "CPU image data is already top-down");
            prop_assert!(!t.is_bound());
            prop_assert_eq!(t.width, w);
            prop_assert_eq!(t.height, h);
            prop_assert_eq!(t.filter, Filter::Linear, "must match the uploaded GL_LINEAR");
        }

    }

    /// The context request is a 0-terminated list of attribute/value pairs,
    /// every one of them asking for an attribute the driver knows by name.
    ///
    /// GLX reads the array up to the 0, so a missing terminator has the
    /// driver walk off the end of it and an interior 0 silently truncates the
    /// request — and neither is reported as an error: the compositor would
    /// just come up on a context nobody asked for. The list is a constant, so
    /// this is a scan of all of it rather than a generated case.
    #[test]
    fn glx_context_attribute_list_is_paired_and_zero_terminated() {
        let list = GLX_CTX_ATTRIBS;
        assert_eq!(
            *list.last().expect("the list is not empty"),
            0,
            "the request must end on its terminator"
        );
        let body = &list[..list.len() - 1];
        assert_eq!(body.len() % 2, 0, "every attribute needs a value");
        for (i, slot) in body.iter().enumerate() {
            assert_ne!(*slot, 0, "slot {i} ends the request early");
        }
        // Every even slot names a context attribute, so no value can end up
        // paired with the wrong key.
        for (i, key) in body.iter().step_by(2).enumerate() {
            assert!(
                [
                    GLX_CONTEXT_MAJOR_VERSION_ARB,
                    GLX_CONTEXT_MINOR_VERSION_ARB,
                    GLX_CONTEXT_PROFILE_MASK_ARB,
                ]
                .contains(key),
                "attribute {i} is 0x{key:x}, which no context attribute uses"
            );
        }
    }

    /// The context is never older than the GLSL the built-in shaders declare.
    ///
    /// GLSL only versions downwards: a `#version 330 core` shader on a 3.1
    /// context fails to compile on the driver, the program fails to link, and
    /// the window manager comes up with a compositor that draws nothing and
    /// reports no error. Asking for *more* than the shaders need is harmless,
    /// so only the unsafe direction is required.
    #[test]
    fn glx_context_is_new_enough_for_the_builtin_shaders() {
        let (major, minor) = (GLX_CTX_ATTRIBS[1] as u32, GLX_CTX_ATTRIBS[3] as u32);
        assert_eq!(GLX_CTX_ATTRIBS[0], GLX_CONTEXT_MAJOR_VERSION_ARB);
        assert_eq!(GLX_CTX_ATTRIBS[2], GLX_CONTEXT_MINOR_VERSION_ARB);
        assert_eq!(GLX_CTX_ATTRIBS[4], GLX_CONTEXT_PROFILE_MASK_ARB);
        for src in [VERTEX_SRC, FRAGMENT_SRC] {
            let decl = src
                .lines()
                .find(|l| !l.trim().is_empty())
                .expect("a shader starts with its #version");
            let number: u32 = decl
                .trim()
                .trim_start_matches("#version")
                .split_whitespace()
                .next()
                .expect("a version follows #version")
                .parse()
                .expect("a numeric GLSL version");
            // GLSL numbers its version `330` where GL numbers the same one
            // `3.3`, so both are compared as hundredths of a major version.
            let glsl = number / 10;
            let requested = major * 10 + minor;
            assert!(
                requested >= glsl,
                "GL {major}.{minor} cannot compile a #version {number} shader"
            );
            // The shaders use core-profile constructs (explicit attribute
            // locations, `texture()`), so the request must ask for core.
            if decl.contains("core") {
                assert_ne!(
                    GLX_CTX_ATTRIBS[5] & GLX_CONTEXT_CORE_PROFILE_BIT_ARB,
                    0,
                    "a core-profile shader needs a core-profile context"
                );
            }
        }
    }
}
