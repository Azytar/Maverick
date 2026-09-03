// maverick/src/core/wallpaper.rs
//
// The wallpaper *domain model* — kept entirely free of any GL/X11 type so the
// upper layers (State, Engine, WindowManager) never name OpenGL. The actual GPU
// work goes through the `WallpaperGpu` trait (implemented inside the x11/GL
// backend as `GlWallpaper`), which is the seam the plan requires for a future
// Vulkan backend.

use crate::types::Rect;
use std::path::PathBuf;
use std::str::FromStr;

/// Where the wallpaper pixels come from. `Video` is reserved (Fase 10): the enum
/// variant exists so the type system and IPC round-trip it, but the backend
/// does not yet implement a video decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperSource {
    None,
    /// A still image: PNG (decoded natively) or any other format via the
    /// external-converter fallback. Path is the user-supplied (possibly
    /// space-containing) path.
    Image(PathBuf),
    /// A user GLSL fragment shader (the compositor supplies `u_time`,
    /// `u_resolution`, `u_delta_time`). Compiled once and re-drawn every frame.
    Shader(PathBuf),
    /// Reserved for a future external video backend (mpv/ffmpeg). Not decoded yet.
    Video(PathBuf),
}

/// How the image is mapped onto each output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WallpaperMode {
    /// Cover the whole output, cropping the image to the output's aspect ratio.
    #[default]
    Fill,
    /// Fit the whole image inside the output, letterboxing (no distortion).
    Fit,
    /// Stretch to the whole output (distorts aspect ratio).
    Stretch,
    /// Draw 1:1 pixels, centred; crops when larger, gaps when smaller.
    Center,
}

impl FromStr for WallpaperMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fill" => Ok(WallpaperMode::Fill),
            "fit" => Ok(WallpaperMode::Fit),
            "stretch" => Ok(WallpaperMode::Stretch),
            "center" => Ok(WallpaperMode::Center),
            other => Err(format!(
                "unknown wallpaper mode '{other}' (fill|fit|stretch|center)"
            )),
        }
    }
}

/// The full wallpaper configuration held in `State`. Pure data — no GPU handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperSpec {
    pub source: WallpaperSource,
    pub mode: WallpaperMode,
}

impl Default for WallpaperSpec {
    fn default() -> Self {
        WallpaperSpec {
            source: WallpaperSource::None,
            mode: WallpaperMode::Fill,
        }
    }
}

/// Whether a wallpaper fragment shader actually depends on time, and therefore
/// requires a fresh frame every turn.
///
/// The compositor's fixed uniform contract supplies `u_time` (float) and
/// `u_delta_time` (float); a shader that references neither produces an identical
/// image every frame, so it is "static" and must not keep the frame scheduler
/// awake (which would otherwise make the compositor present at vsync forever on
/// idle, burning a CPU core for nothing). GLSL comments are ignored so a
/// `u_time` mention inside a comment cannot misclassify a static shader.
pub fn shader_is_animated(src: &str) -> bool {
    let stripped = strip_glsl_comments(src);
    stripped
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|tok| tok == "u_time" || tok == "u_delta_time")
}

/// Drop GLSL `//` line comments and `/* … */` block comments from `src` so the
/// shader-animation probe cannot be fooled by an identifier that only appears in
/// a comment. Input is expected to be ASCII (GLSL source); non-ASCII bytes are
/// copied through unchanged.
fn strip_glsl_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut block = false;
    while i < bytes.len() {
        if block {
            if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            block = true;
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

impl WallpaperSource {
    /// Infer the source kind from a path's extension. Shader fragments use a
    /// known GLSL suffix (`.glsl`/`.frag`/`.vert`/`.shader`/`.fs`); everything
    /// else is treated as a still image. `Video` is never inferred here — it is
    /// reserved and only reachable through its explicit enum variant.
    pub fn from_path(path: PathBuf) -> WallpaperSource {
        let is_shader = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "glsl" | "frag" | "vert" | "shader" | "fs"
            )
        });
        if is_shader {
            WallpaperSource::Shader(path)
        } else {
            WallpaperSource::Image(path)
        }
    }
}

/// Neutral GPU handle for an uploaded wallpaper image (opaque `u32` texture id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuImage(pub u32);

/// Compute, for every output, the destination rect (screen pixels) and the
/// source UV rectangle (0..1, top-down) to draw the wallpaper image. Pure: no
/// GL, no allocation beyond the returned `Vec`. One tuple per output; the image
/// is a single shared texture, each quad uses its own src/dst.
///
/// * `Fill`    — cover (crop to output aspect, no distortion).
/// * `Fit`     — contain (letterbox, no distortion).
/// * `Stretch` — fill output exactly (distorts).
/// * `Center`  — 1:1 px, centered (crops when larger, gaps when smaller).
pub fn compute_wallpaper_rects(
    img_w: u32,
    img_h: u32,
    mode: WallpaperMode,
    outputs: &[Rect],
) -> Vec<(Rect, [f32; 4])> {
    let (iw, ih) = (img_w as f64, img_h as f64);
    let mut out = Vec::with_capacity(outputs.len());
    for o in outputs {
        let (ow, oh) = (o.w as f64, o.h as f64);
        if iw <= 0.0 || ih <= 0.0 || ow <= 0.0 || oh <= 0.0 {
            out.push((*o, [0.0, 0.0, 1.0, 1.0]));
            continue;
        }
        let (dst, src) = match mode {
            WallpaperMode::Fill => {
                // Cover: scale = max, then centre the overflowing axis.
                let scale = (ow / iw).max(oh / ih);
                let disp_w = iw * scale;
                let disp_h = ih * scale;
                let fu = (ow / disp_w) as f32;
                let fv = (oh / disp_h) as f32;
                let u0 = (1.0 - fu) / 2.0;
                let v0 = (1.0 - fv) / 2.0;
                (*o, [u0, v0, u0 + fu, v0 + fv])
            }
            WallpaperMode::Fit => {
                // Contain: scale = min, letterbox the shortfall.
                let scale = (ow / iw).min(oh / ih);
                let disp_w = iw * scale;
                let disp_h = ih * scale;
                let x = o.x as f64 + (ow - disp_w) / 2.0;
                let y = o.y as f64 + (oh - disp_h) / 2.0;
                let fw = (disp_w / ow) as f32;
                let fh = (disp_h / oh) as f32;
                let u0 = (1.0 - fw) / 2.0;
                let v0 = (1.0 - fh) / 2.0;
                (
                    Rect::new(x as i32, y as i32, disp_w as u32, disp_h as u32),
                    [u0, v0, u0 + fw, v0 + fh],
                )
            }
            WallpaperMode::Stretch => (*o, [0.0, 0.0, 1.0, 1.0]),
            WallpaperMode::Center => {
                if iw >= ow && ih >= oh {
                    // Image larger than output: crop, centred.
                    let u0 = ((iw - ow) / 2.0 / iw) as f32;
                    let v0 = ((ih - oh) / 2.0 / ih) as f32;
                    let fu = (ow / iw) as f32;
                    let fv = (oh / ih) as f32;
                    (*o, [u0, v0, u0 + fu, v0 + fv])
                } else {
                    // Image smaller: 1:1, centred with letterbox gaps.
                    let x = o.x as f64 + (ow - iw) / 2.0;
                    let y = o.y as f64 + (oh - ih) / 2.0;
                    (
                        Rect::new(x as i32, y as i32, iw as u32, ih as u32),
                        [0.0, 0.0, 1.0, 1.0],
                    )
                }
            }
        };
        out.push((dst, src));
    }
    out
}

/// Unit tests for the pure wallpaper geometry.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Rect;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn fill_covers_output() {
        // Image 4:3, output 16:9 → image is taller relative to width, so we
        // crop top/bottom and cover the full output width.
        let rects = compute_wallpaper_rects(800, 600, WallpaperMode::Fill, &[r(0, 0, 1920, 1080)]);
        let (dst, src) = rects[0];
        assert_eq!(dst, r(0, 0, 1920, 1080));
        // Vertical crop: disp 1920x1440, fv=0.75, v0=0.125
        assert!((src[0] - 0.0).abs() < 1e-6);
        assert!((src[2] - 1.0).abs() < 1e-6);
        assert!((src[1] - 0.125).abs() < 1e-3);
        assert!((src[3] - 0.875).abs() < 1e-3);
    }

    #[test]
    fn fit_letterboxes() {
        let rects = compute_wallpaper_rects(800, 600, WallpaperMode::Fit, &[r(0, 0, 1920, 1080)]);
        let (dst, _src) = rects[0];
        // 800x600 → fit means height matches output, width is smaller.
        assert_eq!(dst.w, 1440);
        assert_eq!(dst.h, 1080);
        let x = (1920 - 1440) / 2;
        assert_eq!(dst.x, x);
    }

    #[test]
    fn stretch_ignores_aspect() {
        let rects =
            compute_wallpaper_rects(800, 600, WallpaperMode::Stretch, &[r(0, 0, 1920, 1080)]);
        let (dst, src) = rects[0];
        assert_eq!(dst, r(0, 0, 1920, 1080));
        assert!(src
            .iter()
            .zip([0.0, 0.0, 1.0, 1.0])
            .all(|(a, b)| (a - b).abs() < 1e-6));
    }
}
