//! Root-pixmap wallpaper — the no-compositor, feh-style path.
//!
//! The GL compositor draws the wallpaper itself; when it is not running
//! (the WM built without a compositor backend, the user disabled it, or
//! GL failed at runtime) this module keeps `[wallpaper]` working with
//! plain X11:
//!
//!   decode (`maverick-img`) → map onto every monitor
//!     (`compute_wallpaper_rects`, same pure mapping the GL path uses)
//!     → paint one root-sized pixmap → install with the ESETROOT
//!     protocol (`_XROOTPMAP_ID` / `_XSETROOT_ID`), so `feh`/`xsetroot`
//!     can still replace it later.
//!
//! # Timing
//!
//! The WM calls this exactly when it knows the time is right — it owns
//! the screen and its monitor list: after startup finished loading, on
//! config reload, on monitor (re)configuration, and when the compositor
//! falls back.
//!
//! # Leak fix
//!
//! `last_root_pixmap` tracks the previous pixmap so it can be
//! `free_pixmap`ed before replacing it (old code leaked).
//!
//! # Safety
//!
//! `put_image` is fire-and-forget; X11 errors are caught by the
//! silent error handler. The pixmap is a valid X11 resource until
//! `free_pixmap`.

use x11rb::connection::Connection;
//   pixmap → install it with the ESETROOT protocol (`_XROOTPMAP_ID` /
//   `_XSETROOT_ID`), so `feh`/`xsetroot` can still replace it later.
//
// The WM calls this exactly when it *knows* the time is right — it owns the
// screen and its monitor list: after startup finished loading, on config
// reload, on monitor (re)configuration, and when the compositor falls back.

use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, ImageFormat, PropMode,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::core::wallpaper::{compute_wallpaper_rects, WallpaperSource};
use crate::log;
use crate::types::Rect;

impl super::WindowManager {
    /// Draw `[wallpaper]` onto the root window (no-op on the compositor path,
    /// when no path is configured, or when the source is a GLSL shader).
    pub(super) fn apply_root_wallpaper(&mut self) {
        if self.compositor.is_some() {
            return; // the compositor paints its own background
        }
        let Some(path) = self.engine.cfg.wallpaper.path.clone() else {
            return;
        };
        // Shaders are a compositor feature; the root path draws images only.
        if !matches!(
            self.engine.state.wallpaper.source,
            WallpaperSource::Image(_)
        ) {
            return;
        }

        let screen = &self.conn.setup().roots[self.screen_num];
        let (root_w, root_h) = (
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );
        let depth = screen.root_depth;
        let outputs: Vec<Rect> = self
            .engine
            .state
            .monitors
            .iter()
            .map(|m| m.screen)
            .collect();
        let mode = self.engine.cfg.wallpaper.mode;

        let img = match maverick_img::decode(std::path::Path::new(&path)) {
            Ok(img) if img.is_valid() => img,
            Ok(_) => {
                log::warn!("wallpaper: corrupt image {path} (size/buffer mismatch)");
                return;
            }
            Err(e) => {
                log::warn!("wallpaper: cannot decode {path}: {e}");
                return;
            }
        };

        // One root-sized BGRX buffer; every monitor's region is filled with
        // its share of the image (same mapping as the compositor's GL quads).
        let stride = root_w as usize * 4;
        let mut buf = vec![0u8; stride * root_h as usize];
        for (dst, src) in compute_wallpaper_rects(img.w, img.h, mode, &outputs) {
            blit_bilinear(&img, src, dst, &mut buf, root_w, root_h, depth);
        }

        match self.install_root_pixmap(root_w, root_h, depth, &buf) {
            Ok(()) => log::info!("wallpaper: root pixmap set ({path})"),
            Err(e) => log::warn!("wallpaper: failed to set root pixmap: {e}"),
        }
    }

    /// Upload `buf` as a pixmap and install it as the root background with the
    /// ESETROOT protocol. `buf` is BGRX (depth 24) or BGRX-with-alpha (32).
    fn install_root_pixmap(
        &mut self,
        w: u32,
        h: u32,
        depth: u8,
        buf: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = &self.conn;
        let root = self.root;
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(depth, pixmap, root, w as u16, h as u16)?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, pixmap, &CreateGCAux::new())?;

        // `put_image` has no automatic chunking: split by rows so one request
        // never exceeds the server's maximum request size (1 MiB is safely
        // below every server's limit, including non-BigRequests ones).
        const CHUNK_BYTES: usize = 1 << 20;
        let stride = w as usize * 4;
        let rows_per_chunk = (CHUNK_BYTES / stride).clamp(1, h as usize);
        let mut y = 0usize;
        while y < h as usize {
            let n = rows_per_chunk.min(h as usize - y);
            conn.put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                gc,
                w as u16,
                n as u16,
                0,
                y as i16,
                0,
                depth,
                &buf[y * stride..(y + n) * stride],
            )?;
            y += n;
        }
        conn.free_gc(gc)?;

        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?
        .check()?;
        conn.clear_area(true, root, 0, 0, 0, 0)?;

        // ESETROOT protocol: publish the pixmap so a later `feh`/`xsetroot`
        // can find (and free) it instead of leaking the old background.
        let pmap = conn.intern_atom(false, b"_XROOTPMAP_ID")?.reply()?.atom;
        let setroot = conn.intern_atom(false, b"_XSETROOT_ID")?.reply()?.atom;
        let _ = conn.change_property32(PropMode::REPLACE, root, pmap, AtomEnum::PIXMAP, &[pixmap]);
        let _ = conn.change_property32(
            PropMode::REPLACE,
            root,
            setroot,
            AtomEnum::PIXMAP,
            &[pixmap],
        );

        // The root's `background_pixmap` attribute now points at the new
        // pixmap, so our own previous one (if any) is no longer referenced by
        // anything but our own creation of it — free it. Bug: this call was
        // entirely missing before, so every `apply_root_wallpaper` (startup,
        // reload, monitor reconfig, GL-fallback) leaked a full root-sized
        // pixmap in the X server for the rest of the session.
        if let Some(old) = self.last_root_pixmap.replace(pixmap) {
            let _ = conn.free_pixmap(old);
        }

        conn.flush()?;
        Ok(())
    }
}
/// Sample the image's `src` UV-rect into `dst` (root coordinates) with
/// bilinear filtering — feh-class quality, no external tools.
fn blit_bilinear(
    img: &maverick_img::Rgba8,
    src: [f32; 4],
    dst: Rect,
    buf: &mut [u8],
    root_w: u32,
    root_h: u32,
    depth: u8,
) {
    let (iw, ih) = (img.w as f32, img.h as f32);
    let (u0, v0, u1, v1) = (src[0], src[1], src[2], src[3]);
    let (dw, dh) = (dst.w as f32, dst.h as f32);
    if dw <= 0.0 || dh <= 0.0 {
        return;
    }
    let has_alpha = depth == 32;
    for dy in 0..dst.h as i32 {
        let py = dst.y + dy;
        if py < 0 || py >= root_h as i32 {
            continue;
        }
        let fy = v0 + (dy as f32 + 0.5) / dh * (v1 - v0);
        for dx in 0..dst.w as i32 {
            let px = dst.x + dx;
            if px < 0 || px >= root_w as i32 {
                continue;
            }
            let fx = u0 + (dx as f32 + 0.5) / dw * (u1 - u0);
            let (r, g, b) = sample_bilinear(img, iw, ih, fx, fy);
            let off = (py as usize * root_w as usize + px as usize) * 4;
            buf[off] = b;
            buf[off + 1] = g;
            buf[off + 2] = r;
            // depth 24: padding byte (ignored); depth 32: opaque alpha.
            buf[off + 3] = if has_alpha { 0xff } else { 0x00 };
        }
    }
}

/// Bilinear sample of `img` at normalized `(fx, fy)`, clamped at the edges,
/// returning `(r, g, b)`.
fn sample_bilinear(img: &maverick_img::Rgba8, iw: f32, ih: f32, fx: f32, fy: f32) -> (u8, u8, u8) {
    let x = (fx * iw - 0.5).clamp(0.0, iw - 1.0);
    let y = (fy * ih - 0.5).clamp(0.0, ih - 1.0);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let px = |xx: u32, yy: u32| -> [u8; 3] {
        let o = (yy as usize * img.w as usize + xx as usize) * 4;
        [img.data[o], img.data[o + 1], img.data[o + 2]]
    };
    let mix = |a: u8, b: u8, t: f32| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round() as u8 };
    let (c00, c10, c01, c11) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0u8; 3];
    for i in 0..3 {
        let top = mix(c00[i], c10[i], tx);
        let bot = mix(c01[i], c11[i], tx);
        out[i] = mix(top, bot, ty);
    }
    (out[0], out[1], out[2])
}
