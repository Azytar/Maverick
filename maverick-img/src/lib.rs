// maverick-img — dependency-free image decode for Maverick's native wallpaper.
//
// Decodes the formats the compositor needs without pulling in any runtime
// crate: a from-scratch PNG decoder (zlib/DEFLATE inflater + filters + all
// bit-depths/colour-types), plus trivial formats (PPM, QOI, BMP, farbfeld).
// Anything we don't decode natively (JPEG/WebP/AVIF/…) is delegated to an
// external converter (ffmpeg/convert) that dumps raw RGBA — the *hybrid* path
// mandated by the plan. The native path is the fast, no-fork default; the
// external one is the safety net.

use std::path::Path;

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
        self.w as usize * self.h as usize * 4 == self.data.len()
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

// ─── trivial formats ────────────────────────────────────────────────────────

fn decode_ppm(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("ppm: {e}"))?;
    let mut i = 2usize;
    if bytes.len() < 2 || &bytes[0..2] != b"P6" {
        return Err("ppm: not a P6 file".into());
    }
    let read_int = |i: &mut usize| -> Result<i64, String> {
        // skip whitespace and comments
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
        }
        if *i == start {
            return Err("ppm: malformed header".into());
        }
        let s = std::str::from_utf8(&bytes[start..*i]).map_err(|_| "ppm: bad int".to_string())?;
        s.parse::<i64>().map_err(|_| "ppm: bad int".to_string())
    };
    let w = read_int(&mut i)? as u32;
    let h = read_int(&mut i)? as u32;
    let maxval = read_int(&mut i)?;
    if maxval != 255 {
        return Err("ppm: only maxval 255 supported".into());
    }
    if !(w > 0 && h > 0) {
        return Err("ppm: zero dimension".into());
    }
    // single whitespace after maxval, then binary data.
    if i >= bytes.len() || !bytes[i].is_ascii_whitespace() {
        return Err("ppm: missing separator before data".into());
    }
    i += 1;
    let need = w as usize * h as usize * 3;
    if bytes.len() - i < need {
        return Err("ppm: truncated pixel data".into());
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for p in 0..need / 3 {
        let r = bytes[i + p * 3];
        let g = bytes[i + p * 3 + 1];
        let b = bytes[i + p * 3 + 2];
        out.extend_from_slice(&[r, g, b, 255]);
    }
    Ok(Rgba8 { data: out, w, h })
}

fn decode_farbfeld(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("farbfeld: {e}"))?;
    if bytes.len() < 16 || &bytes[0..8] != b"farbfeld" {
        return Err("farbfeld: bad magic".into());
    }
    let u32be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let w = u32be(8) as usize;
    let h = u32be(12) as usize;
    let need = 16 + w * h * 8;
    if bytes.len() < need {
        return Err("farbfeld: truncated".into());
    }
    let mut out = Vec::with_capacity(w * h * 4);
    let mut o = 16;
    for _ in 0..w * h {
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

// ─── QOI ────────────────────────────────────────────────────────────────────

fn decode_qoi(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("qoi: {e}"))?;
    if bytes.len() < 14 || &bytes[0..4] != b"qoif" {
        return Err("qoi: bad magic".into());
    }
    let u32be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let w = u32be(4) as usize;
    let h = u32be(8) as usize;
    let channels = bytes[12];
    let _colorspace = bytes[13];
    if w == 0 || h == 0 || (channels != 3 && channels != 4) {
        return Err("qoi: bad header".into());
    }
    let mut px = [0u8, 0, 0, 255];
    let mut index = [[0u8; 4]; 64];
    let mut out = Vec::with_capacity(w * h * 4);
    let mut o = 14;
    let end = bytes.len().saturating_sub(8);
    while out.len() < w * h * 4 && o < end {
        let b1 = bytes[o];
        o += 1;
        if b1 == 0b1111_1111 {
            // QOI_OP_RGB
            px[0] = bytes[o];
            px[1] = bytes[o + 1];
            px[2] = bytes[o + 2];
            o += 3;
        } else if b1 == 0b1111_1110 {
            // QOI_OP_RGBA
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
                    if out.len() < w * h * 4 {
                        out.extend_from_slice(&px);
                    }
                }
                // index update for the repeated pixel
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
    if out.len() < w * h * 4 {
        return Err("qoi: truncated stream".into());
    }
    Ok(Rgba8 {
        data: out,
        w: w as u32,
        h: h as u32,
    })
}

// ─── BMP (24/32-bit BI_RGB) ──────────────────────────────────────────────────

fn decode_bmp(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("bmp: {e}"))?;
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
    let bpp = u32le(28) as u16;
    let compression = u32le(30);
    if compression != 0 {
        return Err("bmp: only BI_RGB supported".into());
    }
    if bpp != 24 && bpp != 32 {
        return Err(format!("bmp: unsupported bit-depth {bpp}"));
    }
    let bw = w.unsigned_abs() as usize;
    let top_down = h < 0;
    let bh = h.unsigned_abs() as usize;
    let channels = bpp as usize / 8;
    let row_bytes = (bw * channels + 3) & !3; // 4-byte row stride
    let mut out = Vec::with_capacity(bw * bh * 4);
    for row in 0..bh {
        let src_row = if top_down { row } else { bh - 1 - row };
        let base = data_offset + src_row * row_bytes;
        for col in 0..bw {
            let p = base + col * channels;
            if p + 3 > bytes.len() {
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

// ─── PNG (native, from scratch) ──────────────────────────────────────────────

fn decode_png(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("png: {e}"))?;
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
        let typ = &bytes[i + 4..i + 8];
        let data_start = i + 8;
        let data_end = data_start + len;
        if data_end > bytes.len() {
            return Err("png: chunk overruns file".into());
        }
        match typ {
            b"IHDR" => {
                if len < 13 {
                    return Err("png: short IHDR".into());
                }
                width = u32::from_be_bytes(bytes[data_start..data_start + 4].try_into().unwrap());
                height =
                    u32::from_be_bytes(bytes[data_start + 4..data_start + 8].try_into().unwrap());
                bit_depth = bytes[data_start + 8];
                color_type = bytes[data_start + 9];
            }
            b"PLTE" => palette.extend_from_slice(&bytes[data_start..data_end]),
            b"tRNS" => trns.extend_from_slice(&bytes[data_start..data_end]),
            b"IDAT" => idat.extend_from_slice(&bytes[data_start..data_end]),
            b"IEND" => break,
            _ => {}
        }
        i = data_end + 4; // skip CRC
    }
    if width == 0 || height == 0 {
        return Err("png: missing/zero IHDR".into());
    }
    if !(bit_depth == 1 || bit_depth == 2 || bit_depth == 4 || bit_depth == 8 || bit_depth == 16) {
        return Err(format!("png: unsupported bit depth {bit_depth}"));
    }
    let channels = match color_type {
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
    let bpp = (channels * bd).div_ceil(8); // bytes per pixel (filter neighbour distance)
    let stride = (w * channels * bd).div_ceil(8); // bytes per scanline (raw)

    // Unfilter byte-by-byte into a per-row byte buffer.
    let mut unfiltered = vec![0u8; h * stride];
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

    // Unpack samples (handles bit depths) then map to RGBA.
    let samples_per_row = w * channels;
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &unfiltered[y * stride..y * stride + stride];
        let mut samples = Vec::with_capacity(samples_per_row);
        if bd == 16 {
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
            // Packed bit depths (1/2/4): pull `bd` MSB-first bits per sample.
            let max = (1u32 << bd) - 1;
            let mut bit_pos = 0usize;
            for _ in 0..samples_per_row {
                let mut v = 0u32;
                for _ in 0..bd {
                    let byte = *row.get(bit_pos / 8).unwrap_or(&0) as u32;
                    let bit = (byte >> (7 - (bit_pos % 8))) & 1;
                    v = (v << 1) | bit;
                    bit_pos += 1;
                }
                samples.push(((v * 255 + max / 2) / max) as u8);
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

// ─── zlib/DEFLATE inflater (puff.c-style, public-domain port) ─────────────────

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
            let last = bits.take(1)? == 1;
            let btype = bits.take(2)?;
            if btype == 0 {
                // Stored: align to byte, then LEN/NLEN + LEN bytes.
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

// ─── external converter fallback (hybrid path) ───────────────────────────────

/// Try an external converter (ffmpeg/convert) and parse its PPM output to RGBA.
/// Used when no native decoder applies (JPEG/WebP/AVIF/…) or the native one
/// errored. Returns `Err` only when every converter is unavailable or fails.
fn decode_external(path: &Path) -> Result<Rgba8, String> {
    let path = path.to_string_lossy().into_owned();
    let mut last_err = String::from("no external image converter found");
    for cmd in ["ffmpeg", "convert", "magick"] {
        let which = std::process::Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !which {
            continue;
        }
        let output = if cmd == "ffmpeg" {
            std::process::Command::new(cmd)
                .args([
                    "-i",
                    &path,
                    "-vframes",
                    "1",
                    "-f",
                    "image2pipe",
                    "-pix_fmt",
                    "rgb24",
                    "-",
                ])
                .output()
        } else {
            std::process::Command::new(cmd)
                .args([&path, "ppm:-"])
                .output()
        };
        match output {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                if let Ok(img) = ppm_from_bytes(&out.stdout) {
                    return Ok(img);
                }
                last_err = format!("{cmd}: could not parse PPM output");
            }
            Ok(out) => {
                last_err = format!(
                    "{cmd} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .next()
                        .unwrap_or("")
                );
            }
            Err(e) => last_err = format!("{cmd}: {e}"),
        }
    }
    Err(format!("maverick-img: {last_err}"))
}

/// Parse a P6 PPM byte stream (as produced by the external converters).
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
        }
        if *i == start {
            return Err("external PPM: malformed header".into());
        }
        std::str::from_utf8(&bytes[start..*i])
            .map_err(|_| "external PPM: bad int".to_string())?
            .parse::<i64>()
            .map_err(|_| "external PPM: bad int".to_string())
    };
    let w = read_int(&mut i)? as u32;
    let h = read_int(&mut i)? as u32;
    let maxval = read_int(&mut i)?;
    if maxval != 255 {
        return Err("external PPM: maxval != 255".into());
    }
    if i >= bytes.len() || !bytes[i].is_ascii_whitespace() {
        return Err("external PPM: missing separator".into());
    }
    i += 1;
    let need = w as usize * h as usize * 3;
    if bytes.len() - i < need {
        return Err("external PPM: truncated".into());
    }
    let mut out = Vec::with_capacity(need / 3 * 4);
    for p in 0..need / 3 {
        out.extend_from_slice(&[
            bytes[i + p * 3],
            bytes[i + p * 3 + 1],
            bytes[i + p * 3 + 2],
            255,
        ]);
    }
    Ok(Rgba8 { data: out, w, h })
}

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
        let tmp = std::env::temp_dir().join("maverick-img-test.ppm");
        std::fs::write(&tmp, b"P6\n1 1\n255\n\x10\x14\x1e").unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (1, 1));
        assert_eq!(&img.data[..3], &[0x10, 0x14, 0x1e]);
        std::fs::remove_file(&tmp).ok();
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
        let tmp = std::env::temp_dir().join("maverick-img-test.qoi");
        std::fs::write(&tmp, &buf).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (2, 1));
        assert_eq!(&img.data[0..4], &red);
        assert_eq!(&img.data[4..8], &red);
        std::fs::remove_file(&tmp).ok();
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
        let tmp = std::env::temp_dir().join("maverick-img-test.bmp");
        std::fs::write(&tmp, &b).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!((img.w, img.h), (1, 1));
        assert_eq!(&img.data[0..4], &[30, 20, 10, 255]);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn farbfeld_inline() {
        let mut b = vec![];
        b.extend_from_slice(b"farbfeld");
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&[0x10u8, 0, 0x20, 0, 0x30, 0, 0x40, 0]); // 16-bit → 0x10,0x20,0x30,0x40
        let tmp = std::env::temp_dir().join("maverick-img-test.ff");
        std::fs::write(&tmp, &b).unwrap();
        let img = decode(&tmp).unwrap();
        assert_eq!(&img.data[0..4], &[0x10, 0x20, 0x30, 0x40]);
        std::fs::remove_file(&tmp).ok();
    }
}
