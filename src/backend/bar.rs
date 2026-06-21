// maverick/src/bar.rs
// Status bar rendered with plain X11 GC + XDrawString.
// No deps beyond x11rb. Layout: [tags] [layout] [window title ... ] [status]
//
// Catppuccin Mocha colors — looks clean, reads clearly.
//
// Performance: all GC changes and draw calls are fire-and-forget (no .check()).
// A single flush() at the end batches everything into one round-trip.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::backend::atoms::Atoms;
use crate::config::Cfg;
use crate::types::{Monitor, State};

const FONT: &[u8] = b"-misc-fixed-medium-r-normal--13-120-75-75-c-70-iso8859-1\0";

// Padding inside bar for tag labels
pub(crate) const TAG_PAD: i16 = 10;
const SEP_W:              u16 = 8;

pub struct Bar {
    pub font_id: u32,
    pub font_ascent: i32,
    pub char_w: u32,  // approximate average char width for monospace
}

impl Bar {
    pub fn load(conn: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let font_id = conn.generate_id()?;
        conn.open_font(font_id, FONT)?.check().unwrap_or_else(|_| {
            // fallback: try any fixed font
            let _ = conn.open_font(font_id, b"fixed\0");
        });

        let fi = conn.query_font(font_id)?.reply()?;
        let font_ascent = fi.font_ascent as i32;
        let char_w = fi.min_bounds.character_width.abs() as u32;
        let char_w = if char_w == 0 { 7 } else { char_w };

        Ok(Self { font_id, font_ascent, char_w })
    }

    #[allow(dead_code)]
    pub fn create_window(
        &self,
        conn: &RustConnection,
        mon: &mut Monitor,
        atoms: &Atoms,
        cfg: &Cfg,
        root: Window,
        screen_depth: u8,
        screen_visual: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bar_win = conn.generate_id()?;
        let bar_y   = mon.bar_y();
        let bar_h   = cfg.bar_height;
        let bar_w   = mon.screen.w;

        conn.create_window(
            screen_depth,
            bar_win,
            root,
            mon.screen.x as i16,
            bar_y as i16,
            bar_w as u16,
            bar_h as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            screen_visual,
            &CreateWindowAux::new()
                .background_pixel(cfg.col_bar_bg)
                .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS)
                .override_redirect(1u32),
        )?.check()?;

        // EWMH: mark as dock so other WMs/compositors know what it is
        conn.change_property32(
            PropMode::REPLACE, bar_win,
            atoms.net_wm_window_type, AtomEnum::ATOM,
            &[atoms.net_wm_window_type_dock],
        )?.check()?;

        // _NET_WM_STRUT_PARTIAL: reserve bar area
        let strut = if cfg.top_bar {
            [0u32, 0, bar_h, 0,
             0, 0, 0, 0,
             mon.screen.x as u32, (mon.screen.x + mon.screen.w as i32) as u32,
             0, 0]
        } else {
            [0u32, 0, 0, bar_h,
             0, 0, 0, 0,
             0, 0,
             mon.screen.x as u32, (mon.screen.x + mon.screen.w as i32) as u32]
        };
        conn.change_property32(
            PropMode::REPLACE, bar_win,
            atoms.net_wm_strut_partial, AtomEnum::CARDINAL, &strut,
        )?.check()?;

        let gc_id = conn.generate_id()?;
        conn.create_gc(
            gc_id, bar_win,
            &CreateGCAux::new()
                .foreground(cfg.col_bar_fg)
                .background(cfg.col_bar_bg)
                .font(self.font_id),
        )?.check()?;

        conn.map_window(bar_win)?.check()?;

        mon.bar_win = Some(bar_win);
        mon.bar_gc  = Some(gc_id);

        Ok(())
    }

    pub fn draw(
        &self,
        conn: &RustConnection,
        state: &State,
        mon_idx: usize,
        cfg: &Cfg,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &state.monitors[mon_idx];
        let bar_win = match mon.bar_win { Some(w) => w, None => return Ok(()) };
        let gc      = match mon.bar_gc  { Some(g) => g, None => return Ok(()) };
        if !mon.show_bar { return Ok(()); }

        let bar_w = mon.screen.w as u16;
        let bar_h = cfg.bar_height as u16;
        let text_y = (bar_h as i32 / 2 + self.font_ascent / 2) as i16;

        // ── clear background ──
        // Fire-and-forget: no .check() on draw calls — batched by the flush() at the end.
        let _ = conn.change_gc(gc, &ChangeGCAux::new()
            .foreground(cfg.col_bar_bg)
            .background(cfg.col_bar_bg));
        let _ = conn.poly_fill_rectangle(bar_win, gc, &[Rectangle {
            x: 0, y: 0, width: bar_w, height: bar_h,
        }]);

        let mut x: i16 = 4;

        // ── workspace tags ──
        for (i, ws) in mon.workspaces.iter().enumerate() {
            let name = cfg.tag_names.get(i).unwrap_or(&"?");
            let is_active   = i == mon.active_ws;
            let is_occupied = !ws.is_empty();
            let has_urgent  = ws.columns.iter().flat_map(|c| &c.windows)
                .chain(ws.floats.iter())
                .any(|&w| state.clients.get(&w)
                    .map(|c| c.flags.has(crate::types::WinFlags::URGENT))
                    .unwrap_or(false));

            let bg = if is_active      { cfg.col_bar_sel }
                     else if has_urgent { cfg.col_urgent  }
                     else               { cfg.col_bar_bg  };
            let fg = if is_active || has_urgent { cfg.col_bar_bg  }
                     else if is_occupied        { cfg.col_bar_occ }
                     else                       { 0x585b70 }; // surface2

            // Convert UTF-8 → Latin-1 and cap at 255 glyphs before computing geometry.
            let tag_l1 = to_latin1(name, 255);
            let label_w = (tag_l1.len() as u16) * (self.char_w as u16) + TAG_PAD as u16 * 2;

            if is_active || has_urgent {
                let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(bg));
                let _ = conn.poly_fill_rectangle(bar_win, gc, &[Rectangle {
                    x, y: 2, width: label_w, height: bar_h - 4,
                }]);
            }

            let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(fg).background(bg));
            let _ = conn.image_text8(bar_win, gc, x + TAG_PAD, text_y, &tag_l1);

            if is_occupied && !is_active && !has_urgent {
                let dot_x = x + TAG_PAD + tag_l1.len() as i16 * self.char_w as i16 + 2;
                let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(cfg.col_bar_occ));
                let _ = conn.poly_fill_rectangle(bar_win, gc, &[Rectangle {
                    x: dot_x, y: bar_h as i16 / 2 - 1, width: 3, height: 3,
                }]);
            }

            x += label_w as i16 + 2;
        }

        // ── separator ──
        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(0x313244));
        let _ = conn.poly_fill_rectangle(bar_win, gc, &[Rectangle {
            x, y: 4, width: 1, height: bar_h - 8,
        }]);
        x += SEP_W as i16;

        // ── layout symbol ──  (always ASCII, no conversion needed)
        let layout_sym = state.layout.symbol();
        let _ = conn.change_gc(gc, &ChangeGCAux::new()
            .foreground(0x89dceb)
            .background(cfg.col_bar_bg));
        let _ = conn.image_text8(bar_win, gc, x, text_y, layout_sym.as_bytes());
        x += (layout_sym.len() as i16) * self.char_w as i16 + SEP_W as i16;

        // ── separator ──
        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(0x313244));
        let _ = conn.poly_fill_rectangle(bar_win, gc, &[Rectangle {
            x, y: 4, width: 1, height: bar_h - 8,
        }]);
        x += SEP_W as i16;

        // ── status text (right-aligned) ──
        // Convert to Latin-1 first so char count = byte count = pixel width.
        let status_l1 = to_latin1(&state.status, 255);
        let status_w  = (status_l1.len() as u16) * self.char_w as u16 + 8;
        let status_x  = bar_w.saturating_sub(status_w) as i16;

        if status_x > x + 8 && !status_l1.is_empty() {
            let _ = conn.change_gc(gc, &ChangeGCAux::new()
                .foreground(0xa6adc8)
                .background(cfg.col_bar_bg));
            let _ = conn.image_text8(bar_win, gc, status_x, text_y, &status_l1);
        }

        // ── focused window title ──
        if let Some(focused) = mon.focused {
            if let Some(client) = state.clients.get(&focused) {
                let avail_glyphs = ((status_x - x - 4).max(0) as usize / self.char_w as usize)
                    .min(255);
                if avail_glyphs > 0 {
                    let title_l1 = to_latin1(&client.name, avail_glyphs);
                    if !title_l1.is_empty() {
                        let _ = conn.change_gc(gc, &ChangeGCAux::new()
                            .foreground(cfg.col_bar_fg)
                            .background(cfg.col_bar_bg));
                        let _ = conn.image_text8(bar_win, gc, x, text_y, &title_l1);
                    }
                }
            }
        }

        // flush() is NOT called here — the event loop calls conn.flush() after
        // flush_bars() returns, batching all X11 output in one syscall.
        Ok(())
    }

    /// Given a bar-relative x coordinate, return the workspace index that was clicked,
    /// or None if the click was outside all tag buttons. Mirrors draw() tag geometry exactly.
    pub fn tag_at_x(&self, x: i16, tag_names: &[&'static str]) -> Option<usize> {
        let mut cur_x: i16 = 4;
        for (i, name) in tag_names.iter().enumerate() {
            let glyph_count = name.chars()
                .filter(|&c| c as u32 <= 0xFF)
                .count()
                .min(255) as i16;
            let label_w = glyph_count * self.char_w as i16 + TAG_PAD * 2;
            let right = cur_x + label_w + 2; // +2 is the inter-tag gap from draw()
            if x >= cur_x && x < right {
                return Some(i);
            }
            cur_x = right;
        }
        None
    }
}

/// Convert a UTF-8 string to a Latin-1 byte vector for use with image_text8.
///
/// image_text8 uses the X11 font encoding (ISO 8859-1 / Latin-1).  Raw UTF-8
/// bytes passed to it produce garbage because multi-byte sequences each render
/// as separate Latin-1 glyphs.  The correct approach is to convert code points:
///
///   U+0000..U+00FF  → same value as Latin-1 byte  (covers all European scripts)
///   U+0100+          → replaced with '?' (CJK, emoji, etc.)
///
/// `max_glyphs` is the maximum number of output bytes (= display characters).
/// image_text8 has a hard CARD8 limit of 255 bytes, so always pass ≤ 255.
fn to_latin1(s: &str, max_glyphs: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(max_glyphs.min(s.len()));
    for ch in s.chars() {
        if out.len() >= max_glyphs { break; }
        let cp = ch as u32;
        out.push(if cp <= 0xFF { cp as u8 } else { b'?' });
    }
    out
}
