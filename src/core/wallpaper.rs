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

pub use maverick_img::Rgba8;

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

impl WallpaperSource {
    /// Infer the source kind from a path's extension. Shader fragments use a
    /// known GLSL suffix (`.glsl`/`.frag`/`.vert`/`.shader`/`.fs`); everything
    /// else is treated as a still image. `Video` is never inferred here — it is
    /// reserved and only reachable through its explicit enum variant.
    pub fn from_path(path: PathBuf) -> WallpaperSource {
        let is_shader = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "glsl" | "frag" | "vert" | "shader" | "fs"
                )
            })
            .unwrap_or(false);
        if is_shader {
            WallpaperSource::Shader(path)
        } else {
            WallpaperSource::Image(path)
        }
    }
}

/// Neutral GPU handle for an uploaded wallpaper image (opaque `u32` texture id).
/// Lives in `core`, never names GL — the backend fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuImage(pub u32);

/// Neutral GPU handle for a compiled wallpaper shader program (opaque `u32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderId(pub u32);

/// The GPU abstraction the wallpaper needs. Implemented by the x11/GL backend
/// (`GlWallpaper`); a future Vulkan backend implements the same trait. The core
/// only ever calls these methods — it never speaks OpenGL.
pub trait WallpaperGpu {
    /// Upload decoded CPU pixels to a GPU texture.
    fn upload_image(&mut self, img: &Rgba8) -> Result<GpuImage, String>;
    /// Compile a user fragment shader, returning an opaque program id.
    fn compile_shader(&mut self, frag: &str) -> Result<ShaderId, String>;
    /// Draw `img` into `dst` (screen px) sampling `src_uv` (0..1, top-down).
    fn draw_image(&mut self, img: &GpuImage, dst: Rect, src_uv: [f32; 4]);
    /// Draw the shader `s` filling `out` (screen px) for time `time`/`dt`.
    fn draw_shader(&mut self, s: ShaderId, out: Rect, time: f32, dt: f32);
    /// Release a previously uploaded image.
    fn release(&mut self, img: GpuImage);
}

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
                (
                    Rect {
                        x: x.round() as i32,
                        y: y.round() as i32,
                        w: disp_w.round() as u32,
                        h: disp_h.round() as u32,
                    },
                    [0.0, 0.0, 1.0, 1.0],
                )
            }
            WallpaperMode::Stretch => (*o, [0.0, 0.0, 1.0, 1.0]),
            WallpaperMode::Center => {
                // 1:1, centred: native-size quad (may overflow or gap).
                let x = o.x as f64 + (ow - iw) / 2.0;
                let y = o.y as f64 + (oh - ih) / 2.0;
                (
                    Rect {
                        x: x.round() as i32,
                        y: y.round() as i32,
                        w: img_w,
                        h: img_h,
                    },
                    [0.0, 0.0, 1.0, 1.0],
                )
            }
        };
        out.push((dst, src));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn mode_from_str_round_trips() {
        assert_eq!(
            "fill".parse::<WallpaperMode>().unwrap(),
            WallpaperMode::Fill
        );
        assert_eq!("FIT".parse::<WallpaperMode>().unwrap(), WallpaperMode::Fit);
        assert_eq!(
            "Stretch".parse::<WallpaperMode>().unwrap(),
            WallpaperMode::Stretch
        );
        assert_eq!(
            "center".parse::<WallpaperMode>().unwrap(),
            WallpaperMode::Center
        );
        assert!("bogus".parse::<WallpaperMode>().is_err());
        assert_eq!(WallpaperMode::default(), WallpaperMode::Fill);
    }

    #[test]
    fn fill_covers_output_cropping() {
        // image 200x100 (wide), output 1920x1080 (narrower) -> width fills, crop X.
        let r = compute_wallpaper_rects(200, 100, WallpaperMode::Fill, &[rect(0, 0, 1920, 1080)]);
        let (dst, src) = &r[0];
        assert_eq!(*dst, rect(0, 0, 1920, 1080));
        // vertical is fully used, horizontal is cropped symmetrically.
        assert!((src[1] - 0.0).abs() < 1e-6 && (src[3] - 1.0).abs() < 1e-6);
        let used_u = src[2] - src[0];
        // 1920/2160 ≈ 0.8889 of the width.
        assert!((used_u - 1920.0 / 2160.0).abs() < 1e-4);
        assert!((src[0] - (1.0 - used_u) / 2.0).abs() < 1e-4);
    }

    #[test]
    fn fit_letterboxes_inside_output() {
        let r = compute_wallpaper_rects(200, 100, WallpaperMode::Fit, &[rect(0, 0, 1920, 1080)]);
        let (dst, src) = &r[0];
        assert_eq!(*src, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(dst.w, 1920);
        assert_eq!(dst.h, 960);
        assert_eq!(dst.x, 0);
        assert_eq!(dst.y, 60); // (1080-960)/2
    }

    #[test]
    fn stretch_fills_without_crop() {
        let r =
            compute_wallpaper_rects(200, 100, WallpaperMode::Stretch, &[rect(0, 0, 1920, 1080)]);
        let (dst, src) = &r[0];
        assert_eq!(*dst, rect(0, 0, 1920, 1080));
        assert_eq!(*src, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn center_is_native_size_centered() {
        // image smaller than output -> centred 1:1.
        let r =
            compute_wallpaper_rects(1000, 1000, WallpaperMode::Center, &[rect(0, 0, 1920, 1080)]);
        let (dst, src) = &r[0];
        assert_eq!(*src, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(*dst, rect(460, 40, 1000, 1000)); // (1920-1000)/2=460, (1080-1000)/2=40
    }

    #[test]
    fn multi_monitor_different_aspect_fill() {
        let outs = [rect(0, 0, 1920, 1080), rect(1920, 0, 1280, 1024)];
        let r = compute_wallpaper_rects(1000, 1000, WallpaperMode::Fill, &outs);
        assert_eq!(r.len(), 2);
        // Each output is fully covered.
        assert_eq!(r[0].0, rect(0, 0, 1920, 1080));
        assert_eq!(r[1].0, rect(1920, 0, 1280, 1024));
        // A square image on either landscape output covers full width (crops the
        // height); the two crops differ only because the outputs differ in height.
        let fu0 = r[0].1[2] - r[0].1[0];
        let fu1 = r[1].1[2] - r[1].1[0];
        assert!((fu0 - 1.0).abs() < 1e-6); // 1920x1080 -> width fully used
        assert!((fu1 - 1.0).abs() < 1e-6); // 1280x1024 -> width fully used
                                           // Both crops centre the (taller) image vertically.
        assert!((r[1].1[1] - (1.0 - 0.8) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn multi_monitor_fit_letterboxes_asymmetric() {
        let outs = [rect(0, 0, 1920, 1080), rect(1920, 0, 1280, 1024)];
        let r = compute_wallpaper_rects(1000, 1000, WallpaperMode::Fit, &outs);
        // Both quads are 1:1 (square image, square quad), centred per monitor.
        assert_eq!(r[0].0.w, r[0].0.h);
        assert_eq!(r[1].0.w, r[1].0.h);
        // Second monitor (narrower/taller) yields a smaller quad.
        assert!(r[1].0.w < r[0].0.w);
    }

    #[test]
    fn reordering_outputs_changes_quads() {
        let a = [rect(0, 0, 1920, 1080), rect(1920, 0, 800, 600)];
        let mut b = a.to_vec();
        b.reverse();
        let ra = compute_wallpaper_rects(1000, 1000, WallpaperMode::Fit, &a);
        let rb = compute_wallpaper_rects(1000, 1000, WallpaperMode::Fit, &b);
        assert_ne!(ra[0].0, rb[0].0); // different destination for monitor 0
        assert_eq!(ra[0].0, rb[1].0); // but consistent per-output geometry
    }

    #[test]
    fn video_source_round_trips_as_reserved() {
        let s = WallpaperSource::Video(std::path::PathBuf::from("/tmp/x.mp4"));
        assert_eq!(
            s,
            WallpaperSource::Video(std::path::PathBuf::from("/tmp/x.mp4"))
        );
    }
}
