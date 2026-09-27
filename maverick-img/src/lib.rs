//! Dependency-free image decode for wallpapers.
//!
//! Returns [`Rgba8`] (8-bit RGBA, row-major, top-left origin). Native decoders:
//! PNG (zlib/DEFLATE inflater, filters, all bit depths and colour types) plus
//! PPM/PNM (P6), QOI, BMP (24/32-bit `BI_RGB`) and farbfeld (16 → 8 bit).
//! Anything else (JPEG/WebP/AVIF/…) is delegated to [`decode_external`].
//!
//! The PNG path parses `IHDR`/`PLTE`/`tRNS`/`IDAT`/`IEND`, inflates the
//! zlib-wrapped DEFLATE stream ([`inflate`], a port of Mark Adler's `puff.c`),
//! unfilters the scanlines (`None`/`Sub`/`Up`/`Average`/`Paeth`, with `a` =
//! left, `b` = above, `c` = upper-left), unpacks bit depths 1/2/4/8/16 and
//! colour types 0/2/3/4/6 including `tRNS`, and emits RGBA.
//!
//! [`decode`] picks the decoder from the lowercased file extension and falls
//! back to [`decode_external`] on an unknown extension or any native error.
//! `decode_external` resolves `ffmpeg`/`convert`/`magick` against `PATH`
//! itself (no `which` subprocess), reads each converter's PPM output to EOF
//! without waiting for it, and parses it through `ppm_from_bytes`.
//! [`Rgba8::is_valid`] checks `data.len() == w*h*4`.
//!
//! # Invariants
//!
//! Every decoder validates magic, dimensions and truncation before emitting
//! anything, and no dimension is multiplied before it is bounds- and
//! overflow-checked. `inflate` rejects over-subscribed Huffman trees, invalid
//! codes, bad length/distance symbols, and back-references past the start of
//! the output. QOI uses the wrapping arithmetic `DIFF`/`LUMA` require.
//! External converters run as child processes whose output is bounded *while
//! being read* and only parsed after header validation. None of them is waited
//! for, because the process that embeds this crate — the window manager — sets
//! `SA_NOCLDWAIT` and therefore cannot obtain an exit status at all; see
//! [`decode_external`].
//!
//! # Errors
//!
//! Malformed input is always `Err(String)`. [`decode`] only reports an error
//! once every decoder, native and external, has failed.

use std::path::Path;

/// Hard caps that turn hostile dimension headers into a clean `Err` instead of
/// a wrap-around allocation or an OOM abort. 16384px per side is far beyond any
/// wallpaper; 64M pixels (256 MiB of RGBA) bounds the total allocation.
pub const MAX_DIM: usize = 16_384;
pub const MAX_PIXELS: usize = 64_000_000;
/// Byte budget for anything read from outside the file format itself: a
/// converter's stdout, one PNG chunk, the accumulated `IDAT`, and the inflated
/// stream. Sized as the PPM pixel budget times 3 plus slack.
const MAX_EXTERNAL_BYTES: usize = 200_000_000;

/// Validate `w x h` against [`MAX_DIM`]/[`MAX_PIXELS`] with checked math.
fn check_dims(w: usize, h: usize) -> Result<(usize, usize), String> {
    if w == 0 || h == 0 {
        return Err("zero dimension".into());
    }
    if w > MAX_DIM || h > MAX_DIM {
        return Err(format!("dimensions too large: {w}x{h}"));
    }
    w.checked_mul(h)
        .filter(|&n| n <= MAX_PIXELS)
        .map(|_| (w, h))
        .ok_or_else(|| format!("pixel count too large: {w}x{h}"))
}

/// `w * h * n` with an overflow check, reported under `what`.
fn checked_buf(w: usize, h: usize, n: usize, what: &str) -> Result<usize, String> {
    w.checked_mul(h)
        .and_then(|p| p.checked_mul(n))
        .ok_or_else(|| format!("{what}: size overflow"))
}

/// Convert a parsed `i64` header int to a bounded dimension. Negatives are
/// rejected *before* the `usize` cast: `-1` would otherwise wrap to a huge
/// value and still pass the `> 0` style checks, driving a giant allocation.
fn dim_from_i64(v: i64, what: &str) -> Result<usize, String> {
    if v <= 0 || v as u64 > MAX_DIM as u64 {
        return Err(format!("{what}: bad dimension {v}"));
    }
    Ok(v as usize)
}

/// A decoded image in 8-bit-per-channel RGBA, row-major, top-left origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8 {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

impl Rgba8 {
    /// `true` only when the buffer has exactly `w*h*4` bytes.
    pub fn is_valid(&self) -> bool {
        (self.w as usize)
            .checked_mul(self.h as usize)
            .and_then(|p| p.checked_mul(4))
            .is_some_and(|n| n == self.data.len())
    }
}

/// Decode `path` into RGBA8. Tries the native decoder for the file's extension
/// first; on any native failure falls back to an external converter. Returns a
/// clear error only when every path failed.
pub fn decode(path: &Path) -> Result<Rgba8, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let native = match ext.as_str() {
        "png" => decode_png(path),
        "ppm" | "pnm" => decode_ppm(path),
        "qoi" => decode_qoi(path),
        "bmp" => decode_bmp(path),
        "ff" | "farbfeld" => decode_farbfeld(path),
        _ => Err(format!("maverick-img: no native decoder for '.{ext}'")),
    };
    if let Ok(img) = native {
        return Ok(img);
    }
    decode_external(path)
}

fn decode_ppm(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("ppm: {e}"))?;
    decode_ppm_bytes(&bytes)
}

/// PPM body of [`decode_ppm`], split out so the parser is exercised from an
/// in-memory buffer: every input-domain test can then feed a byte string
/// directly instead of standing up a file.
fn decode_ppm_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    let mut i = 2usize;
    if bytes.len() < 2 || &bytes[0..2] != b"P6" {
        return Err("ppm: not a P6 file".into());
    }
    let read_int = |i: &mut usize| -> Result<i64, String> {
        // PPM headers allow `#` comments between any two tokens.
        while *i < bytes.len() {
            let c = bytes[*i];
            if c == b'#' {
                while *i < bytes.len() && bytes[*i] != b'\n' {
                    *i += 1;
                }
            } else if c.is_ascii_whitespace() {
                *i += 1;
            } else {
                break;
            }
        }
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            // Bound the digit run: `i64` holds at most 19 digits, so a longer
            // one is a hostile header burning CPU in the parse path.
            if *i - start > 19 {
                return Err("ppm: bad int".into());
            }
        }
        if *i == start {
            return Err("ppm: malformed header".into());
        }
        let s = std::str::from_utf8(&bytes[start..*i]).map_err(|_| "ppm: bad int".to_string())?;
        s.parse::<i64>().map_err(|_| "ppm: bad int".to_string())
    };
    let w_raw = read_int(&mut i)?;
    let h_raw = read_int(&mut i)?;
    let maxval = read_int(&mut i)?;
    if maxval != 255 {
        return Err("ppm: only maxval 255 supported".into());
    }
    let w = dim_from_i64(w_raw, "ppm").map_err(|e| format!("ppm: {e}"))?;
    let h = dim_from_i64(h_raw, "ppm").map_err(|e| format!("ppm: {e}"))?;
    check_dims(w, h).map_err(|e| format!("ppm: {e}"))?;
    // Exactly one whitespace byte separates the header from the binary
    // samples; the spec allows only a single delimiter, and pixel data may
    // itself start with whitespace bytes.
    if i >= bytes.len() || !bytes[i].is_ascii_whitespace() {
        return Err("ppm: missing separator before data".into());
    }
    i += 1;
    let need = checked_buf(w, h, 3, "ppm")?;
    if bytes.len().saturating_sub(i) < need {
        return Err("ppm: truncated pixel data".into());
    }
    let mut out = Vec::with_capacity(checked_buf(w, h, 4, "ppm")?);
    for p in 0..need / 3 {
        let r = bytes[i + p * 3];
        let g = bytes[i + p * 3 + 1];
        let b = bytes[i + p * 3 + 2];
        out.extend_from_slice(&[r, g, b, 255]);
    }
    Ok(Rgba8 {
        data: out,
        w: w as u32,
        h: h as u32,
    })
}

fn decode_farbfeld(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("farbfeld: {e}"))?;
    decode_farbfeld_bytes(&bytes)
}

/// farbfeld body of [`decode_farbfeld`]; see [`decode_ppm_bytes`].
fn decode_farbfeld_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    if bytes.len() < 16 || &bytes[0..8] != b"farbfeld" {
        return Err("farbfeld: bad magic".into());
    }
    let u32be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let w = u32be(8) as usize;
    let h = u32be(12) as usize;
    check_dims(w, h).map_err(|e| format!("farbfeld: {e}"))?;
    let need = checked_buf(w, h, 8, "farbfeld")?
        .checked_add(16)
        .ok_or_else(|| "farbfeld: size overflow".to_string())?;
    if bytes.len() < need {
        return Err("farbfeld: truncated".into());
    }
    let mut out = Vec::with_capacity(checked_buf(w, h, 4, "farbfeld")?);
    let mut o = 16;
    for _ in 0..w.checked_mul(h).ok_or("farbfeld: size overflow")? {
        let r = (u16::from_be_bytes([bytes[o], bytes[o + 1]]) >> 8) as u8;
        let g = (u16::from_be_bytes([bytes[o + 2], bytes[o + 3]]) >> 8) as u8;
        let b = (u16::from_be_bytes([bytes[o + 4], bytes[o + 5]]) >> 8) as u8;
        let a = (u16::from_be_bytes([bytes[o + 6], bytes[o + 7]]) >> 8) as u8;
        out.extend_from_slice(&[r, g, b, a]);
        o += 8;
    }
    Ok(Rgba8 {
        data: out,
        w: w as u32,
        h: h as u32,
    })
}

fn decode_qoi(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("qoi: {e}"))?;
    decode_qoi_bytes(&bytes)
}

/// QOI body of [`decode_qoi`]; see [`decode_ppm_bytes`].
fn decode_qoi_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    if bytes.len() < 14 || &bytes[0..4] != b"qoif" {
        return Err("qoi: bad magic".into());
    }
    let u32be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let w = u32be(4) as usize;
    let h = u32be(8) as usize;
    let channels = bytes[12];
    let _colorspace = bytes[13];
    if channels != 3 && channels != 4 {
        return Err("qoi: bad header".into());
    }
    check_dims(w, h).map_err(|e| format!("qoi: {e}"))?;
    let total = checked_buf(w, h, 4, "qoi")?;
    let mut px = [0u8, 0, 0, 255];
    let mut index = [[0u8; 4]; 64];
    let mut out = Vec::with_capacity(total);
    let mut o = 14;
    // `end` stops 8 bytes short of the file: the QOI end marker is 7 zero
    // bytes plus a `0x01`, and the loop must not read into it.
    let end = bytes.len().saturating_sub(8);
    // Bail out unless `n` more bytes are readable at `o` and still before
    // `end`.
    macro_rules! need {
        ($n:expr) => {
            if o + ($n) > end || o + ($n) > bytes.len() {
                return Err("qoi: truncated stream".into());
            }
        };
    }
    while out.len() < total && o < end {
        let b1 = bytes[o];
        o += 1;
        if b1 == 0b1111_1111 {
            // QOI_OP_RGB
            need!(3);
            px[0] = bytes[o];
            px[1] = bytes[o + 1];
            px[2] = bytes[o + 2];
            o += 3;
        } else if b1 == 0b1111_1110 {
            // QOI_OP_RGBA
            need!(4);
            px[0] = bytes[o];
            px[1] = bytes[o + 1];
            px[2] = bytes[o + 2];
            px[3] = bytes[o + 3];
            o += 4;
        } else {
            let tag = b1 >> 6;
            if tag == 0b00 {
                // INDEX
                px = index[(b1 & 0x3f) as usize];
            } else if tag == 0b01 {
                // DIFF
                px[0] = px[0].wrapping_add(((b1 >> 4) & 3).wrapping_sub(1));
                px[1] = px[1].wrapping_add(((b1 >> 2) & 3).wrapping_sub(1));
                px[2] = px[2].wrapping_add((b1 & 3).wrapping_sub(1));
            } else if tag == 0b10 {
                // LUMA: dg then dr=dg+(b2>>4-8), db=dg+(b2&0xf-8), all wrapping.
                need!(1);
                let b2 = bytes[o];
                o += 1;
                let dg = (b1 & 0x3f) as i32 - 32;
                let dr = dg + ((b2 as i32 >> 4) - 8);
                let db = dg + ((b2 as i32 & 0x0f) - 8);
                px[0] = px[0].wrapping_add(dr as i8 as u8);
                px[1] = px[1].wrapping_add(dg as i8 as u8);
                px[2] = px[2].wrapping_add(db as i8 as u8);
            } else {
                // RUN
                let run = (b1 & 0x3f) as usize + 1;
                for _ in 0..run {
                    if out.len() < total {
                        out.extend_from_slice(&px);
                    }
                }
                // RUN repeats `px`; the spec does not require the index to be
                // refreshed here, but keeping every op consistent costs one
                // store and cannot change a conforming stream's output.
                index[(px[0] as usize * 3
                    + px[1] as usize * 5
                    + px[2] as usize * 7
                    + px[3] as usize * 11)
                    % 64] = px;
                continue;
            }
        }
        if channels == 3 {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        } else {
            out.extend_from_slice(&px);
        }
        index[(px[0] as usize * 3
            + px[1] as usize * 5
            + px[2] as usize * 7
            + px[3] as usize * 11)
            % 64] = px;
    }
    if out.len() < total {
        return Err("qoi: truncated stream".into());
    }
    Ok(Rgba8 {
        data: out,
        w: w as u32,
        h: h as u32,
    })
}

fn decode_bmp(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("bmp: {e}"))?;
    decode_bmp_bytes(&bytes)
}

/// BMP body of [`decode_bmp`]; see [`decode_ppm_bytes`].
fn decode_bmp_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return Err("bmp: bad magic".into());
    }
    let u32le = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let _file_size = u32le(2);
    let _reserved = u32le(6);
    let data_offset = u32le(10) as usize;
    // DIB header size at offset 14.
    let dib = u32le(14);
    if dib < 40 {
        return Err("bmp: unsupported DIB header".into());
    }
    let w = u32le(18) as i32;
    let h = u32le(22) as i32;
    if w <= 0 || h == 0 {
        return Err("bmp: bad dimensions".into());
    }
    // A negative height means the rows are stored top-down. A negative width
    // would mean a mirrored image; it is accepted but not un-mirrored.
    // Headers can claim 2G x 2G, so bound before any multiplication.
    let bw_us = w.unsigned_abs() as usize;
    let bh_us = h.unsigned_abs() as usize;
    check_dims(bw_us, bh_us).map_err(|e| format!("bmp: {e}"))?;
    let bpp = u32le(28) as u16;
    let compression = u32le(30);
    if compression != 0 {
        return Err("bmp: only BI_RGB supported".into());
    }
    if bpp != 24 && bpp != 32 {
        return Err(format!("bmp: unsupported bit-depth {bpp}"));
    }
    let bw = bw_us;
    let top_down = h < 0;
    let bh = bh_us;
    let channels = bpp as usize / 8;
    let row_bytes = bw
        .checked_mul(channels)
        .and_then(|r| r.checked_add(3))
        .map(|r| r & !3)
        .ok_or_else(|| "bmp: size overflow".to_string())?;
    // `data_offset` is attacker-controlled: it must sit past the headers and
    // inside the file, and every index derived from it is checked.
    if data_offset < 54 || data_offset > bytes.len() {
        return Err("bmp: bad data offset".into());
    }
    // Only the last pixel byte has to exist, not the trailing row padding:
    // some writers omit it (e.g. a 1x1 24-bit image stored as 3 bytes). The
    // per-pixel bounds check below skips padding whenever it is present.
    let row_data = bw.checked_mul(channels).ok_or("bmp: size overflow")?;
    let last_row_off = (bh - 1)
        .checked_mul(row_bytes)
        .ok_or("bmp: size overflow")?;
    let last_need = last_row_off
        .checked_add(row_data)
        .ok_or("bmp: size overflow")?;
    if data_offset
        .checked_add(last_need)
        .is_none_or(|e| e > bytes.len())
    {
        return Err("bmp: truncated".into());
    }
    let mut out = Vec::with_capacity(checked_buf(bw, bh, 4, "bmp")?);
    for row in 0..bh {
        let src_row = if top_down { row } else { bh - 1 - row };
        let base = data_offset
            .checked_add(src_row.checked_mul(row_bytes).ok_or("bmp: size overflow")?)
            .ok_or("bmp: size overflow")?;
        for col in 0..bw {
            let p = base
                .checked_add(col.checked_mul(channels).ok_or("bmp: size overflow")?)
                .ok_or("bmp: size overflow")?;
            // Bound the row before reading it: for 24bpp `channels == 3` and
            // the alpha read below is never taken, so this is the whole span.
            if p.checked_add(channels).is_none_or(|e| e > bytes.len()) {
                return Err("bmp: truncated".into());
            }
            let b = bytes[p];
            let g = bytes[p + 1];
            let r = bytes[p + 2];
            let a = if channels == 4 { bytes[p + 3] } else { 255 };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(Rgba8 {
        data: out,
        w: bw as u32,
        h: bh as u32,
    })
}

fn decode_png(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("png: {e}"))?;
    decode_png_bytes(&bytes)
}

/// PNG body of [`decode_png`]; see [`decode_ppm_bytes`].
fn decode_png_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    if bytes.len() < 8 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("png: bad signature".into());
    }
    let mut i = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut idat = Vec::new();
    let mut palette: Vec<u8> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        // Bound the chunk before extending from it: a 2 GiB `len` would drive
        // `extend_from_slice` to OOM before the overrun check below runs.
        if len > MAX_EXTERNAL_BYTES {
            return Err("png: chunk too large".into());
        }
        let typ = &bytes[i + 4..i + 8];
        let data_start = i.checked_add(8).ok_or("png: size overflow")?;
        let data_end = data_start.checked_add(len).ok_or("png: size overflow")?;
        // The 4 CRC bytes must fit too; checking the sum instead of the data
        // range keeps a near-2^32 length from wrapping past the test below.
        let next = data_end.checked_add(4).ok_or("png: size overflow")?;
        if data_end > bytes.len() || next > bytes.len().saturating_add(4) {
            return Err("png: chunk overruns file".into());
        }
        if data_end > bytes.len() {
            return Err("png: chunk overruns file".into());
        }
        match typ {
            b"IHDR" => {
                if len < 13 {
                    return Err("png: short IHDR".into());
                }
                let w_b: [u8; 4] = bytes[data_start..data_start + 4]
                    .try_into()
                    .map_err(|_| "png: short IHDR".to_string())?;
                let h_b: [u8; 4] = bytes[data_start + 4..data_start + 8]
                    .try_into()
                    .map_err(|_| "png: short IHDR".to_string())?;
                width = u32::from_be_bytes(w_b);
                height = u32::from_be_bytes(h_b);
                bit_depth = bytes[data_start + 8];
                color_type = bytes[data_start + 9];
            }
            b"PLTE" => {
                if palette.len().checked_add(len).is_none_or(|n| n > 3 * 256) {
                    return Err("png: PLTE too large".into());
                }
                palette.extend_from_slice(&bytes[data_start..data_end]);
            }
            b"tRNS" => {
                if trns.len().checked_add(len).is_none_or(|n| n > 256) {
                    return Err("png: tRNS too large".into());
                }
                trns.extend_from_slice(&bytes[data_start..data_end]);
            }
            b"IDAT" => {
                if idat
                    .len()
                    .checked_add(len)
                    .is_none_or(|n| n > MAX_EXTERNAL_BYTES)
                {
                    return Err("png: IDAT too large".into());
                }
                idat.extend_from_slice(&bytes[data_start..data_end]);
            }
            b"IEND" => break,
            _ => {}
        }
        i = data_end + 4; // skip CRC
    }
    if width == 0 || height == 0 {
        return Err("png: missing/zero IHDR".into());
    }
    check_dims(width as usize, height as usize).map_err(|e| format!("png: {e}"))?;
    if !(bit_depth == 1 || bit_depth == 2 || bit_depth == 4 || bit_depth == 8 || bit_depth == 16) {
        return Err(format!("png: unsupported bit depth {bit_depth}"));
    }
    let channels: usize = match color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => return Err(format!("png: unsupported colour type {other}")),
    };
    // Inflate the zlib-wrapped DEFLATE stream (skip 2-byte zlib header).
    if idat.len() < 2 {
        return Err("png: no IDAT".into());
    }
    let raw = inflate(&idat[2..]).map_err(|e| format!("png: {e}"))?;

    let w = width as usize;
    let h = height as usize;
    let bd = bit_depth as usize;
    // `bpp` is the filter predictor distance in bytes (rounded up, and at
    // least 1 for sub-byte depths); `stride` is one whole scanline. Neither is
    // the BMP decoder's "bits per pixel".
    let bpp = channels
        .checked_mul(bd)
        .map(|n| n.div_ceil(8))
        .ok_or("png: size overflow")?;
    let stride = w
        .checked_mul(channels)
        .and_then(|n| n.checked_mul(bd))
        .map(|n| n.div_ceil(8))
        .ok_or("png: size overflow")?;
    // Bound the decompressed plane and the RGBA output before allocating either.
    let plane = h.checked_mul(stride).ok_or("png: size overflow")?;
    if plane > MAX_PIXELS * 4 + h {
        return Err("png: image too large".into());
    }
    let _ = checked_buf(w, h, 4, "png")?;

    // Unfilter in place: `a`/`b`/`c` are the already-reconstructed left, above
    // and upper-left bytes, so the current row is read back as it is written.
    let mut unfiltered = vec![0u8; plane];
    let mut prev = vec![0u8; stride];
    let mut pos = 0usize;
    for y in 0..h {
        if pos >= raw.len() {
            return Err("png: truncated image data".into());
        }
        let filter = raw[pos];
        pos += 1;
        if pos + stride > raw.len() {
            return Err("png: truncated image data".into());
        }
        let cur = &mut unfiltered[y * stride..y * stride + stride];
        let src = &raw[pos..pos + stride];
        pos += stride;
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            let val = match filter {
                0 => src[x],
                1 => src[x].wrapping_add(a),
                2 => src[x].wrapping_add(b),
                3 => src[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let p = paeth(a, b, c);
                    src[x].wrapping_add(p)
                }
                _ => return Err(format!("png: unknown filter {filter}")),
            };
            cur[x] = val;
        }
        prev.copy_from_slice(cur);
    }

    // Expand one scanline to 8-bit samples, then map those to RGBA below.
    let samples_per_row = w.checked_mul(channels).ok_or("png: size overflow")?;
    let mut out = Vec::with_capacity(checked_buf(w, h, 4, "png")?);
    for y in 0..h {
        let row = unfiltered
            .get(y.checked_mul(stride).ok_or("png: size overflow")?..)
            .and_then(|r| r.get(..stride))
            .ok_or("png: truncated image data")?;
        let mut samples = Vec::with_capacity(samples_per_row);
        if bd == 16 {
            // Keep the high byte: PNG 16-bit samples are big-endian, and
            // dropping the low byte is the usual 16 → 8 truncation.
            for p in 0..samples_per_row {
                let off = p * 2;
                let v = if off + 1 < row.len() {
                    ((row[off] as u32) << 8 | row[off + 1] as u32) >> 8
                } else {
                    0
                };
                samples.push(v as u8);
            }
        } else if bd == 8 {
            for p in 0..samples_per_row {
                samples.push(*row.get(p).unwrap_or(&0));
            }
        } else {
            // Packed bit depths (1/2/4): `bd` bits per sample, packed MSB first,
            // so sample `i` of the row occupies bits [i * bd, (i + 1) * bd) of
            // the row's bit string. A row always starts on a byte boundary and
            // spans `stride` bytes, which is what lets the cursor below restart
            // at every row instead of running through the whole plane.
            //
            // Whether the field is then rescaled depends on the colour type.
            // For the greyscale types the spec defines a sub-byte field as a
            // shade spread over the full 8-bit range, so the round-half-up
            // expansion below is required. A colour type 3 field is instead a
            // PLTE position and the format defines no rescaling for it: the
            // expansion maps every index 1..2^bd to a fixed 255, which is past
            // the end of a 2^bd-entry palette, so only index 0 would still
            // resolve. The field is therefore used as the index it already is.
            let max = (1u32 << bd) - 1;
            let is_index = color_type == 3;
            let mut bit_pos = 0usize;
            for _ in 0..samples_per_row {
                let mut v = 0u32;
                for _ in 0..bd {
                    let byte = *row.get(bit_pos / 8).unwrap_or(&0) as u32;
                    let bit = (byte >> (7 - (bit_pos % 8))) & 1;
                    v = (v << 1) | bit;
                    bit_pos += 1;
                }
                samples.push(if is_index {
                    v as u8
                } else {
                    ((v * 255 + max / 2) / max) as u8
                });
            }
        }
        for p in 0..w {
            let base = p * channels;
            let (r, g, b, a) = match color_type {
                0 => {
                    let gv = samples[base];
                    let a = if !trns.is_empty() && trns.len() >= 2 {
                        if gv as u16 == u16::from_be_bytes([trns[0], trns[1]]) {
                            0
                        } else {
                            255
                        }
                    } else {
                        255
                    };
                    (gv, gv, gv, a)
                }
                2 => (samples[base], samples[base + 1], samples[base + 2], 255),
                3 => {
                    let idx = samples[base] as usize;
                    let (pr, pg, pb) = if idx * 3 + 2 < palette.len() {
                        (palette[idx * 3], palette[idx * 3 + 1], palette[idx * 3 + 2])
                    } else {
                        (0, 0, 0)
                    };
                    let a = if idx < trns.len() { trns[idx] } else { 255 };
                    (pr, pg, pb, a)
                }
                4 => {
                    let gv = samples[base];
                    let av = samples[base + 1];
                    (gv, gv, gv, av)
                }
                6 => (
                    samples[base],
                    samples[base + 1],
                    samples[base + 2],
                    samples[base + 3],
                ),
                _ => unreachable!(),
            };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }

    Ok(Rgba8 {
        data: out,
        w: width,
        h: height,
    })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

mod inflate {
    //! Minimal inflation of a raw DEFLATE stream (no zlib/CRC dependency).
    //! Ported from Mark Adler's `puff.c` (zlib, public domain / zlib license).

    const MAXBITS: usize = 15;
    const MAXDCODES: usize = 30;
    const FIXLCODES: usize = 288;

    struct Bits<'a> {
        d: &'a [u8],
        pos: usize,
        buf: u64,
        cnt: u32,
    }
    impl<'a> Bits<'a> {
        fn new(d: &'a [u8]) -> Self {
            Bits {
                d,
                pos: 0,
                buf: 0,
                cnt: 0,
            }
        }
        fn take(&mut self, n: u32) -> Result<u32, String> {
            while self.cnt < n {
                if self.pos >= self.d.len() {
                    return Err("inflate: out of input".into());
                }
                self.buf |= (self.d[self.pos] as u64) << self.cnt;
                self.pos += 1;
                self.cnt += 8;
            }
            let v = (self.buf & ((1u64 << n) - 1)) as u32;
            self.buf >>= n;
            self.cnt -= n;
            Ok(v)
        }
    }

    struct Huffman {
        count: [i32; MAXBITS + 1],
        symbol: Vec<i32>,
    }
    impl Huffman {
        fn new(n: usize) -> Self {
            Huffman {
                count: [0; MAXBITS + 1],
                symbol: vec![0; n],
            }
        }
        fn construct(&mut self, lengths: &[u16], n: usize) -> Result<(), String> {
            for c in self.count.iter_mut() {
                *c = 0;
            }
            for &l in lengths.iter().take(n) {
                self.count[l as usize] += 1;
            }
            if self.count[0] == n as i32 {
                return Ok(());
            }
            let mut left = 1i32;
            for len in 1..=MAXBITS {
                left <<= 1;
                left -= self.count[len];
                if left < 0 {
                    return Err("inflate: over-subscribed Huffman tree".into());
                }
            }
            let mut offs = [0i32; MAXBITS + 1];
            offs[1] = 0;
            for len in 1..MAXBITS {
                offs[len + 1] = offs[len] + self.count[len];
            }
            for (sym, &len) in lengths.iter().take(n).enumerate() {
                let l = len as usize;
                if l != 0 {
                    self.symbol[offs[l] as usize] = sym as i32;
                    offs[l] += 1;
                }
            }
            Ok(())
        }
    }

    fn decode(bits: &mut Bits, h: &Huffman) -> Result<i32, String> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;
        for len in 1..=MAXBITS {
            code |= bits.take(1)? as i32;
            let count = h.count[len];
            if code - first < count {
                return Ok(h.symbol[index + (code - first) as usize]);
            }
            index += count as usize;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("inflate: invalid Huffman code".into())
    }

    const LENS: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEXT: [u16; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTS: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DEXT: [u16; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    fn codes(
        bits: &mut Bits,
        lencode: &Huffman,
        distcode: &Huffman,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        loop {
            // Zip-bomb guard: DEFLATE expands by ~1000x, so the output budget
            // is checked before every push, not once at the end.
            if out.len() > super::MAX_EXTERNAL_BYTES {
                return Err("inflate: output too large".into());
            }
            let symbol = decode(bits, lencode)?;
            if symbol < 0 {
                return Err("inflate: bad symbol".into());
            }
            if symbol < 256 {
                out.push(symbol as u8);
            } else if symbol > 256 {
                let sym = symbol as usize - 257;
                if sym >= 29 {
                    return Err("inflate: bad length symbol".into());
                }
                let len = (LENS[sym] as u32 + bits.take(LEXT[sym] as u32)?) as usize;
                let dsym = decode(bits, distcode)? as usize;
                if dsym >= 30 {
                    return Err("inflate: bad distance symbol".into());
                }
                let dist = (DISTS[dsym] as u32 + bits.take(DEXT[dsym] as u32)?) as usize;
                if dist > out.len() {
                    return Err("inflate: back-reference past start".into());
                }
                let start = out.len() - dist;
                for k in 0..len {
                    let b = out[start + (k % dist)];
                    out.push(b);
                }
            } else {
                break; // 256 = end of block
            }
        }
        Ok(())
    }

    fn fixed_trees() -> Result<(Huffman, Huffman), String> {
        let mut llengths = [0u16; FIXLCODES];
        for v in &mut llengths[0..144] {
            *v = 8;
        }
        for v in &mut llengths[144..256] {
            *v = 9;
        }
        for v in &mut llengths[256..280] {
            *v = 7;
        }
        for v in &mut llengths[280..288] {
            *v = 8;
        }
        let mut dlengths = [0u16; MAXDCODES];
        for v in dlengths.iter_mut().take(MAXDCODES) {
            *v = 5;
        }
        let mut lencode = Huffman::new(FIXLCODES);
        let mut distcode = Huffman::new(MAXDCODES);
        lencode.construct(&llengths, FIXLCODES)?;
        distcode.construct(&dlengths, MAXDCODES)?;
        Ok((lencode, distcode))
    }

    fn dynamic_trees(bits: &mut Bits) -> Result<(Huffman, Huffman), String> {
        let hlit = bits.take(5)? as usize + 257;
        let hdist = bits.take(5)? as usize + 1;
        let hclen = bits.take(4)? as usize + 4;
        const ORDER: [u16; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];
        let mut cl_lengths = [0u16; 19];
        for i in 0..hclen {
            cl_lengths[ORDER[i] as usize] = bits.take(3)? as u16;
        }
        let mut cl = Huffman::new(19);
        cl.construct(&cl_lengths, 19)?;

        let mut lengths = vec![0u16; hlit + hdist];
        let mut index = 0usize;
        while index < lengths.len() {
            let sym = decode(bits, &cl)? as usize;
            match sym {
                0..=15 => {
                    lengths[index] = sym as u16;
                    index += 1;
                }
                16 => {
                    if index == 0 {
                        return Err("inflate: repeat with no previous length".into());
                    }
                    let prev = lengths[index - 1];
                    let rep = 3 + bits.take(2)? as usize;
                    for _ in 0..rep {
                        if index >= lengths.len() {
                            break;
                        }
                        lengths[index] = prev;
                        index += 1;
                    }
                }
                17 => {
                    let rep = 3 + bits.take(3)? as usize;
                    for _ in 0..rep {
                        if index >= lengths.len() {
                            break;
                        }
                        lengths[index] = 0;
                        index += 1;
                    }
                }
                18 => {
                    let rep = 11 + bits.take(7)? as usize;
                    for _ in 0..rep {
                        if index >= lengths.len() {
                            break;
                        }
                        lengths[index] = 0;
                        index += 1;
                    }
                }
                _ => return Err("inflate: bad code-length symbol".into()),
            }
        }
        let llengths = &lengths[..hlit];
        let dlengths = &lengths[hlit..];
        let mut lencode = Huffman::new(hlit);
        let mut distcode = Huffman::new(hdist);
        lencode.construct(llengths, hlit)?;
        distcode.construct(dlengths, hdist)?;
        Ok((lencode, distcode))
    }

    pub fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut bits = Bits::new(data);
        let mut out = Vec::new();
        loop {
            if out.len() > super::MAX_EXTERNAL_BYTES {
                return Err("inflate: output too large".into());
            }
            let last = bits.take(1)? == 1;
            let btype = bits.take(2)?;
            if btype == 0 {
                // Stored block: drop the bit buffer to reach a byte boundary,
                // then LEN/NLEN (little-endian) and LEN raw bytes.
                bits.buf = 0;
                bits.cnt = 0;
                let mut read_byte = || -> Result<u8, String> {
                    if bits.pos >= data.len() {
                        return Err("inflate: out of input".into());
                    }
                    let b = data[bits.pos];
                    bits.pos += 1;
                    Ok(b)
                };
                let len = read_byte()? as usize | ((read_byte()? as usize) << 8);
                let _nlen = read_byte()? as usize | ((read_byte()? as usize) << 8);
                for _ in 0..len {
                    out.push(read_byte()?);
                }
            } else if btype == 1 {
                let (l, d) = fixed_trees()?;
                codes(&mut bits, &l, &d, &mut out)?;
            } else if btype == 2 {
                let (l, d) = dynamic_trees(&mut bits)?;
                codes(&mut bits, &l, &d, &mut out)?;
            } else {
                return Err("inflate: invalid block type".into());
            }
            if last {
                break;
            }
        }
        Ok(out)
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    inflate::inflate(data)
}

/// Run one of the external converters and parse its PPM output as RGBA.
/// Used when no native decoder applies (JPEG/WebP/AVIF/…) or the native one
/// failed; `Err` only when every converter is missing or fails.
///
/// Converters are tried in the order `convert`, `magick`, `ffmpeg`. Every one
/// of them must therefore speak the same output contract: a P6 PPM on stdout,
/// which is what `ppm_from_bytes` below parses and what decides success.
/// A converter that exits zero having written anything else is treated as a
/// failure and the next one is tried, so the order only decides which decoder
/// answers, never whether one does.
///
/// The converter is deliberately **not** waited for. The window manager sets
/// `SA_NOCLDWAIT` on `SIGCHLD` (`maverick-sys`'s `Signal::install`), which
/// makes the kernel discard every child's exit status. The consequence is not
/// that `waitpid` fails fast — it does not. Measured on Linux, a blocking
/// `waitpid` on a child with 1.5 s of life left returns `ECHILD` after 1.40 s,
/// and on one with 0.5 s left after 0.40 s: it waits out the child's whole
/// life and *then* reports that there was no status to collect. The hazard a
/// wait creates here is that **block**, on the window manager's own thread,
/// for a wall-clock span the converter chooses and the WM cannot bound.
/// `Command::output` is `waitpid`-based, so using it meant that inside the
/// running window manager every delegated format — and every native decode
/// failure, which also lands here — stalled for the converter's duration and
/// then reported `No child processes` instead of decoding.
///
/// Reading the pipe to EOF is what synchronises the child instead: the pipe
/// reaches EOF only once the converter has closed it, which it does on the way
/// out. That is a complete barrier and not an approximation of one — measured,
/// a `waitpid` attempted after the read returns `ECHILD` in 0.2 ms, versus the
/// seconds it would otherwise have spent blocked. The `Child` is then dropped
/// and the kernel reaps it. The exit status bought nothing here anyway —
/// success was already decided by whether the output parses as a PPM, and a
/// converter that exits zero having written garbage was previously caught by
/// the very same check.
///
/// `stderr` goes to `/dev/null` rather than to a pipe. A piped `stderr` is
/// never read, so a chatty converter would block forever on a full 64 KiB pipe
/// while this process blocked on `stdout`; `Command::output` avoided that by
/// draining both concurrently, which needs a `wait`.
fn decode_external(path: &Path) -> Result<Rgba8, String> {
    use std::io::Read as _;
    let path_str = path.to_string_lossy().into_owned();
    let mut last_err = String::from("no external image converter found");
    // ImageMagick first, ffmpeg last. ImageMagick is both the more widely
    // installed and the markedly faster of the two for a still image: measured
    // over ten decodes of a 3x1 fixture, `convert` averaged 18 ms per spawn
    // against ffmpeg's 143 ms, so probing ffmpeg first spent ~8x the startup
    // latency of the decoder that would actually answer. ffmpeg stays in the
    // chain as a last resort for hosts that ship no ImageMagick at all.
    for cmd in ["convert", "magick", "ffmpeg"] {
        // Resolve once and exec the absolute path: no `which` probe to race
        // against, and a non-executable match is rejected instead of run.
        let bin = match resolve_exec(cmd) {
            Some(b) => b,
            None => continue,
        };
        let mut command = std::process::Command::new(&bin);
        if cmd == "ffmpeg" {
            // `-vcodec ppm` is what makes this branch able to satisfy the
            // function's contract at all. Asking `image2pipe` for `rgb24`
            // instead leaves the muxer to pick an encoder, and it picks the
            // one matching the input, so ffmpeg wrote a JPEG byte stream to
            // stdout. `ppm_from_bytes` rejected that and the loop moved on, so
            // the bug was invisible in the decoded pixels but cost a full
            // spawn-and-discard on every delegated decode. `-pix_fmt` is
            // omitted because it constrains pixel layout, which a PPM encoder
            // does not accept; naming the codec is what pins the output to the
            // P6 form the parser below already speaks.
            command.args([
                "-i",
                &path_str,
                "-vframes",
                "1",
                "-f",
                "image2pipe",
                "-vcodec",
                "ppm",
                "-",
            ]);
        } else {
            command.args([&path_str, "ppm:-"]);
        }
        let mut child = match command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{cmd}: {e}");
                continue;
            }
        };
        let Some(mut pipe) = child.stdout.take() else {
            last_err = format!("{cmd}: no output pipe");
            continue;
        };
        // `take` caps the read, so a hostile or runaway converter cannot make
        // this process allocate without limit — the one byte past the cap is
        // what distinguishes "exactly at the limit" from "over it".
        let mut out = Vec::new();
        let read =
            std::io::Read::take(&mut pipe, MAX_EXTERNAL_BYTES as u64 + 1).read_to_end(&mut out);
        // Dropped, never waited on: `SA_NOCLDWAIT` makes the status
        // unobtainable, and EOF on the pipe already means the converter is
        // done writing.
        drop(child);
        if let Err(e) = read {
            last_err = format!("{cmd}: {e}");
            continue;
        }
        if out.len() > MAX_EXTERNAL_BYTES {
            last_err = format!("{cmd}: output too large");
            continue;
        }
        if out.is_empty() {
            last_err = format!("{cmd}: no PPM output");
            continue;
        }
        if let Ok(img) = ppm_from_bytes(&out) {
            return Ok(img);
        }
        last_err = format!("{cmd}: could not parse PPM output");
    }
    Err(format!("maverick-img: {last_err}"))
}

/// Resolve `bin` against `PATH` to an absolute executable path, without
/// spawning `which` and without ever exec'ing a bare name. `None` when the
/// name is not found or is not executable.
fn resolve_exec(bin: &str) -> Option<std::path::PathBuf> {
    if bin.is_empty() || bin.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let p = dir.join(bin);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(&p) {
                if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                    return Some(p);
                }
            }
        }
        #[cfg(not(unix))]
        {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Parse a P6 PPM byte stream into RGBA, as emitted by the external
/// converters. The stream is untrusted output, so it gets the same dimension,
/// overflow and truncation checks as a PPM read from disk.
fn ppm_from_bytes(bytes: &[u8]) -> Result<Rgba8, String> {
    if bytes.len() < 2 || &bytes[0..2] != b"P6" {
        return Err("external PPM: not P6".into());
    }
    let mut i = 2usize;
    let read_int = |i: &mut usize| -> Result<i64, String> {
        while *i < bytes.len() {
            let c = bytes[*i];
            if c == b'#' {
                while *i < bytes.len() && bytes[*i] != b'\n' {
                    *i += 1;
                }
            } else if c.is_ascii_whitespace() {
                *i += 1;
            } else {
                break;
            }
        }
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            if *i - start > 19 {
                return Err("external PPM: bad int".into());
            }
        }
        if *i == start {
            return Err("external PPM: malformed header".into());
        }
        std::str::from_utf8(&bytes[start..*i])
            .map_err(|_| "external PPM: bad int".to_string())?
            .parse::<i64>()
            .map_err(|_| "external PPM: bad int".to_string())
    };
    let w_raw = read_int(&mut i)?;
    let h_raw = read_int(&mut i)?;
    let maxval = read_int(&mut i)?;
    if maxval != 255 {
        return Err("external PPM: maxval != 255".into());
    }
    let w = dim_from_i64(w_raw, "external PPM").map_err(|e| format!("external PPM: {e}"))?;
    let h = dim_from_i64(h_raw, "external PPM").map_err(|e| format!("external PPM: {e}"))?;
    check_dims(w, h).map_err(|e| format!("external PPM: {e}"))?;
    if i >= bytes.len() || !bytes[i].is_ascii_whitespace() {
        return Err("external PPM: missing separator".into());
    }
    i += 1;
    let need = checked_buf(w, h, 3, "external PPM")?;
    if bytes.len().saturating_sub(i) < need {
        return Err("external PPM: truncated".into());
    }
    let mut out = Vec::with_capacity(checked_buf(w, h, 4, "external PPM")?);
    for p in 0..need / 3 {
        out.extend_from_slice(&[
            bytes[i + p * 3],
            bytes[i + p * 3 + 1],
            bytes[i + p * 3 + 2],
            255,
        ]);
    }
    Ok(Rgba8 {
        data: out,
        w: w as u32,
        h: h as u32,
    })
}

#[cfg(test)]
#[path = "../tests/properties/mod.rs"]
mod properties;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures");
        p.push(name);
        p
    }

    #[test]
    fn ppm_roundtrip_trivial() {
        // A 1x1 PPM written inline.
        // Private per-test directory: the four tests below used fixed
        // names in $TMPDIR, so two running in parallel deleted each
        // other's input and failed with a bare `unwrap` panic. The same
        // collision the workspace `tempfile` dependency exists to prevent,
        // reintroduced in the one crate without the dev-dependency.
        let dir = tempfile::tempdir().expect("private temp dir");
        let tmp = dir.path().join("image.ppm");
        std::fs::write(&tmp, b"P6\n1 1\n255\n\x10\x14\x1e").unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (1, 1));
        assert_eq!(&img.data[..3], &[0x10, 0x14, 0x1e]);
    }

    #[test]
    fn png_rgba2x2() {
        let img = decode(&fixture("rgba2x2.png")).unwrap();
        assert_eq!((img.w, img.h), (2, 2));
        assert_eq!(&img.data[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.data[4..8], &[0, 255, 0, 255]);
        assert_eq!(&img.data[8..12], &[0, 0, 255, 255]);
        assert_eq!(&img.data[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn png_rgb3x1() {
        let img = decode(&fixture("rgb3x1.png")).unwrap();
        assert_eq!((img.w, img.h), (3, 1));
        assert_eq!(&img.data[0..4], &[10, 20, 30, 255]);
        assert_eq!(&img.data[8..12], &[70, 80, 90, 255]);
    }

    #[test]
    fn png_grayscale_alpha() {
        let img = decode(&fixture("ga1x2.png")).unwrap();
        assert_eq!(&img.data[0..4], &[100, 100, 100, 128]);
        assert_eq!(&img.data[4..8], &[200, 200, 200, 64]);
    }

    #[test]
    fn png_palette_with_trns() {
        let img = decode(&fixture("palette2x1.png")).unwrap();
        assert_eq!(&img.data[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.data[4..8], &[0, 255, 0, 128]);
    }

    #[test]
    fn png_paeth_filter() {
        let img = decode(&fixture("rgba_paeth2x2.png")).unwrap();
        assert_eq!(&img.data[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.data[4..8], &[0, 255, 0, 255]);
        assert_eq!(&img.data[8..12], &[0, 0, 255, 255]);
        assert_eq!(&img.data[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn png_sub_filter() {
        let img = decode(&fixture("rgb_sub3x1.png")).unwrap();
        assert_eq!(&img.data[0..4], &[10, 20, 30, 255]);
        assert_eq!(&img.data[8..12], &[70, 80, 90, 255]);
    }

    #[test]
    fn qoi_inline() {
        // Encode a 2x1 opaque red image as QOI and decode it back.
        // QOI magic + 2x1, 4 channels, then RGB op for 2 pixels, then end marker.
        let red = [255u8, 0, 0, 255];
        let mut buf = vec![];
        buf.extend_from_slice(b"qoif");
        buf.extend_from_slice(&(2u32).to_be_bytes());
        buf.extend_from_slice(&(1u32).to_be_bytes());
        buf.push(4); // channels
        buf.push(0); // colorspace
        buf.push(0b1111_1111); // RGB for pixel 0
        buf.extend_from_slice(&red[0..3]);
        buf.push(0b1111_1111); // RGB for pixel 1
        buf.extend_from_slice(&red[0..3]);
        buf.extend_from_slice(&[0xFF, 0, 0, 0, 0, 0, 0, 1]); // end marker
                                                             // Private per-test directory: the four tests below used fixed
                                                             // names in $TMPDIR, so two running in parallel deleted each
                                                             // other's input and failed with a bare `unwrap` panic. The same
                                                             // collision the workspace `tempfile` dependency exists to prevent,
                                                             // reintroduced in the one crate without the dev-dependency.
        let dir = tempfile::tempdir().expect("private temp dir");
        let tmp = dir.path().join("image.qoi");
        std::fs::write(&tmp, &buf).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (2, 1));
        assert_eq!(&img.data[0..4], &red);
        assert_eq!(&img.data[4..8], &red);
    }

    #[test]
    fn bmp_inline() {
        // 1x1 24-bit BMP.
        let mut b = vec![];
        b.extend_from_slice(b"BM");
        let filesize = 54u32;
        b.extend_from_slice(&filesize.to_le_bytes());
        b.extend_from_slice(&[0u8; 4]); // reserved
        b.extend_from_slice(&54u32.to_le_bytes()); // data offset
        b.extend_from_slice(&40u32.to_le_bytes()); // dib size
        b.extend_from_slice(&1u32.to_le_bytes()); // width
        b.extend_from_slice(&1u32.to_le_bytes()); // height
        b.extend_from_slice(&1u16.to_le_bytes()); // planes
        b.extend_from_slice(&24u16.to_le_bytes()); // bpp
        b.extend_from_slice(&[0u8; 4]); // compression
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]); // resolutions etc.
        b.extend_from_slice(&[10, 20, 30]); // BGR
                                            // Private per-test directory: the four tests below used fixed
                                            // names in $TMPDIR, so two running in parallel deleted each
                                            // other's input and failed with a bare `unwrap` panic. The same
                                            // collision the workspace `tempfile` dependency exists to prevent,
                                            // reintroduced in the one crate without the dev-dependency.
        let dir = tempfile::tempdir().expect("private temp dir");
        let tmp = dir.path().join("image.bmp");
        std::fs::write(&tmp, &b).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (1, 1));
        assert_eq!(&img.data[0..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn farbfeld_inline() {
        let mut b = vec![];
        b.extend_from_slice(b"farbfeld");
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&[0x10u8, 0, 0x20, 0, 0x30, 0, 0x40, 0]); // 16-bit BE
                                                                      // Private per-test directory: the four tests below used fixed
                                                                      // names in $TMPDIR, so two running in parallel deleted each
                                                                      // other's input and failed with a bare `unwrap` panic. The same
                                                                      // collision the workspace `tempfile` dependency exists to prevent,
                                                                      // reintroduced in the one crate without the dev-dependency.
        let dir = tempfile::tempdir().expect("private temp dir");
        let tmp = dir.path().join("image.ff");
        std::fs::write(&tmp, &b).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!(&img.data[0..4], &[0x10, 0x20, 0x30, 0x40]);
    }
}
