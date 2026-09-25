//! Property tests for the native decoders.
//!
//! This file is pulled into `src/lib.rs` as a `#[cfg(test)]` module rather than
//! compiled as an integration test of its own, because every property targets
//! the private `*_bytes` parsers: the file-reading wrappers are the only
//! public entry point and they always fall through to `decode_external`, which
//! spawns a converter. The subdirectory keeps cargo from also picking the file
//! up as a standalone test target (only `tests/*.rs` and `tests/*/main.rs` are
//! auto-discovered).
//!
//! Two things are deliberately out of scope. `decode_external` and
//! `decode` are never called: the first runs `ffmpeg`/`convert`/`magick` as
//! child processes, and the second always reaches it once the native decoder
//! has failed, so neither can appear in a deterministic test. Every input
//! below is a byte vector built in memory, and `decode`'s own dispatch is
//! covered separately by `tests/decode_dispatch.rs`, which only feeds the
//! native decoders files they accept so the fallback is never reached.

use super::*;
use proptest::prelude::*;
use std::sync::Mutex;

// ------------------------------------------------------------------ plumbing

/// Run `f` with the panic hook muted.
///
/// A decoder panic has to surface as one shrunk property failure rather than a
/// backtrace interleaved with the output of every other thread cargo is
/// running in this binary in parallel, and it has to be catchable at all:
/// letting it unwind out of the property would abort the run before the
/// counterexample reaches `proptest-regressions/`. The mutex keeps two
/// properties from swapping the process-wide hook underneath each other.
fn catch_panic<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    static HOOK: Mutex<()> = Mutex::new(());
    let _held = HOOK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous);
    out
}

/// The contract every `Ok` owes its caller, as stated by `Rgba8::is_valid` and
/// the `MAX_DIM`/`MAX_PIXELS` caps: an image that got decoded at all has
/// exactly `w*h*4` bytes and dimensions inside the caps. Returned as a
/// `Result` so a property can attach its own context to the failure.
fn well_formed(img: &Rgba8) -> Result<(), String> {
    if !img.is_valid() {
        return Err(format!(
            "Ok image is not w*h*4: {}x{} with {} bytes",
            img.w,
            img.h,
            img.data.len()
        ));
    }
    if img.w as usize > MAX_DIM || img.h as usize > MAX_DIM {
        return Err(format!("Ok image exceeds MAX_DIM: {}x{}", img.w, img.h));
    }
    if (img.w as usize) * (img.h as usize) > MAX_PIXELS {
        return Err(format!("Ok image exceeds MAX_PIXELS: {}x{}", img.w, img.h));
    }
    Ok(())
}

/// The outcome of one decode, with a panic kept as its own case: reporting it
/// as a plain rejection would let a decoder that panics on every input satisfy
/// a property that only checks for errors.
enum Decoded {
    Image(Rgba8),
    Rejected(String),
    Panicked,
}

fn decode_one(
    name: &'static str,
    f: impl FnOnce() -> Result<Rgba8, String>,
) -> (&'static str, Decoded) {
    match catch_panic(f) {
        Ok(Ok(img)) => (name, Decoded::Image(img)),
        Ok(Err(e)) => (name, Decoded::Rejected(e)),
        Err(_) => (name, Decoded::Panicked),
    }
}

/// Run one byte string through all five native parsers, each guarded
/// separately so a panic names the parser that produced it.
fn decode_all(bytes: &[u8]) -> Vec<(&'static str, Decoded)> {
    vec![
        decode_one("png", || decode_png_bytes(bytes)),
        decode_one("ppm", || decode_ppm_bytes(bytes)),
        decode_one("qoi", || decode_qoi_bytes(bytes)),
        decode_one("bmp", || decode_bmp_bytes(bytes)),
        decode_one("farbfeld", || decode_farbfeld_bytes(bytes)),
    ]
}

/// How much of a valid file to keep. The short offsets are the interesting
/// ones - they land inside a magic number or a dimension field - and a uniform
/// draw over the file's length reaches them only by accident.
fn cut_point() -> impl Strategy<Value = usize> {
    prop_oneof![
        1u32 => Just(0usize),
        1u32 => Just(1),
        1u32 => Just(2),
        1u32 => Just(3),
        4u32 => 0usize..16,
        120u32 => 0usize..512,
    ]
}

/// A dimension field, drawn from the whole integer range but weighted towards
/// the values that decide whether a header is accepted: a zero side, a side
/// just past the cap, and the extremes of the type. Two independent uniform
/// draws would almost never pair a zero with an in-range partner, which is
/// exactly the pair the checks under test care about.
fn hostile_u32() -> impl Strategy<Value = u32> {
    prop_oneof![
        4u32 => 0u32..4,
        4u32 => (MAX_DIM as u32 - 1)..=(MAX_DIM as u32 + 1),
        4u32 => (u32::MAX / 2)..=u32::MAX,
        240u32 => any::<u32>(),
    ]
}

fn hostile_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        4u32 => -2i64..2,
        4u32 => (MAX_DIM as i64 - 1)..=(MAX_DIM as i64 + 1),
        4u32 => i64::MIN..=(i64::MIN / 2),
        240u32 => any::<i64>(),
    ]
}

/// A byte string of any length, biased towards the very short inputs: every
/// parser has a fixed-size magic and header, and the panics that matter live
/// in the first few bytes, which a uniform 0..512 draw almost never reaches.
fn arbitrary_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        1u32 => Just(Vec::new()),
        8u32 => proptest::collection::vec(any::<u8>(), 0..8),
        15u32 => proptest::collection::vec(any::<u8>(), 0..512),
    ]
}

// ----------------------------------------------------------- format builders

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn png_chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    let mut crc_input = typ.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// A zlib stream of uncompressed DEFLATE "stored" blocks.
///
/// The properties must not need a compressor, and stored blocks exercise the
/// same reconstruction path as compressed ones while leaving the inflated
/// bytes byte-identical to the input, so a property can state the expected
/// pixels directly instead of predicting a compressor's output.
fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib: deflate, 32K window, no preset dict
    let mut rest = data;
    loop {
        let n = rest.len().min(65535);
        let (head, tail) = rest.split_at(n);
        let last = tail.is_empty();
        out.push(u8::from(last)); // BFINAL, BTYPE=00 (stored)
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(head);
        rest = tail;
        if last {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// The Paeth predictor, from the PNG spec. Written out again here on purpose:
/// the property is only worth anything if the reference direction is derived
/// from the format rather than copied from the implementation.
fn paeth_spec(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Samples per pixel for a PNG colour type, from the format spec.
fn png_channels(color_type: u8) -> usize {
    match color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => 0,
    }
}

/// The filter predictor distance in bytes for a row: `channels * bit_depth`
/// bits, rounded up, and at least one byte for the sub-byte depths.
fn png_bpp(channels: usize, bit_depth: u8) -> usize {
    (channels * bit_depth as usize).div_ceil(8).max(1)
}

/// One packed scanline: `w * channels` samples at `bit_depth`, MSB first.
fn pack_row(samples: &[u16], bit_depth: u8) -> Vec<u8> {
    match bit_depth {
        16 => samples
            .iter()
            .flat_map(|&s| [(s >> 8) as u8, s as u8])
            .collect(),
        8 => samples.iter().map(|&s| s as u8).collect(),
        bd => {
            let mut out = vec![0u8; (samples.len() * bd as usize).div_ceil(8)];
            for (i, &s) in samples.iter().enumerate() {
                for b in 0..bd as usize {
                    if s >> (bd as usize - 1 - b) & 1 == 1 {
                        let pos = i * bd as usize + b;
                        out[pos / 8] |= 1 << (7 - pos % 8);
                    }
                }
            }
            out
        }
    }
}

/// The MSB-first bit string of a packed row.
///
/// Building the expected samples from a string of `0`/`1` characters keeps the
/// reference side free of the `bit_pos / 8` and `7 - bit_pos % 8` index
/// arithmetic, so an off-by-one in the packing cannot be mirrored here.
fn bits_of(row: &[u8]) -> String {
    row.iter()
        .map(|b| format!("{b:08b}"))
        .collect::<Vec<_>>()
        .concat()
}

/// The raw `bit_depth`-bit sample at index `i` of a packed row, with no
/// rescaling at all: this is the value a colour type 3 image uses directly as
/// a palette index.
fn packed_sample(row: &[u8], bit_depth: u8, i: usize) -> u8 {
    let from = i * bit_depth as usize;
    let bits = bits_of(row);
    u8::from_str_radix(&bits[from..from + bit_depth as usize], 2).unwrap_or(0)
}

/// Unpack `count` samples out of one reconstructed row, per the PNG
/// bit-depth rules: a 16-bit sample keeps its high byte, an 8-bit sample is
/// used as is, and a sub-byte sample is rescaled to 0..=255 round-half-up.
///
/// The rescale is the *greyscale* rule, for the colour types whose sub-byte
/// field is a shade. A colour type 3 field is a palette position and the
/// format defines no rescaling for it, so a caller mapping those to pixels has
/// to read the field with [`packed_sample`] instead.
fn unpack_row(row: &[u8], bit_depth: u8, count: usize) -> Vec<u8> {
    match bit_depth {
        16 => (0..count)
            .map(|i| row.get(i * 2).copied().unwrap_or(0))
            .collect(),
        8 => (0..count)
            .map(|i| row.get(i).copied().unwrap_or(0))
            .collect(),
        bd => {
            let max = (1u32 << bd) - 1;
            let bits = bits_of(row);
            (0..count)
                .map(|i| {
                    let from = i * bd as usize;
                    let raw = u32::from_str_radix(&bits[from..from + bd as usize], 2).unwrap_or(0);
                    ((raw * 255 + max / 2) / max) as u8
                })
                .collect()
        }
    }
}

/// A PNG file plus the RGBA it must decode to.
///
/// `expected` is computed from the *unfiltered* scanlines, so a property
/// compares the decoder against the pixels that went in rather than against a
/// second run of the decoder's own logic.
struct PngCase {
    bytes: Vec<u8>,
    expected: Vec<u8>,
    w: u32,
    h: u32,
    /// Offset of the first byte after the `IDAT` chunk: everything from here on
    /// is irrelevant to the pixels.
    end_of_idat: usize,
    /// Offset of the `IDAT` chunk's last data byte, before its CRC.
    end_of_idat_data: usize,
}

/// Everything a generated PNG declares and carries.
struct PngSpec<'a> {
    w: usize,
    h: usize,
    bit_depth: u8,
    color_type: u8,
    samples: &'a [u16],
    filters: &'a [u8],
    palette: Option<&'a [u8]>,
    trns: Option<&'a [u8]>,
}

fn build_png(spec: &PngSpec<'_>) -> PngCase {
    let PngSpec {
        w,
        h,
        bit_depth,
        color_type,
        samples,
        filters,
        palette,
        trns,
    } = *spec;
    // A header the decoder has to refuse makes the pixel payload irrelevant,
    // so the plane is still laid out - it has to be, for the file to be
    // well formed - but with a depth and colour type the reference unpacker
    // can handle. `expected` stays empty and no property compares it.
    let supported = matches!(bit_depth, 1 | 2 | 4 | 8 | 16) && png_channels(color_type) > 0;
    let depth = if supported { bit_depth } else { 8 };
    let plane_type = if supported { color_type } else { 0 };
    let channels = png_channels(plane_type);
    let stride = (w * channels * depth as usize).div_ceil(8);
    let bpp = png_bpp(channels, depth);

    // Filter forward, keeping the reconstruction: that is the plane the
    // decoder has to recover, and the source of the expected pixels.
    let mut plane = Vec::with_capacity(h * (stride + 1));
    let mut prev = vec![0u8; stride];
    let mut expected = Vec::new();
    if supported {
        expected.reserve(w * h * 4);
    }
    for y in 0..h {
        let packed = pack_row(&samples[y * w * channels..(y + 1) * w * channels], depth);
        let f = filters[y];
        plane.push(f);
        let mut recon = vec![0u8; stride];
        for (x, &v) in packed.iter().enumerate() {
            let a = if x >= bpp { recon[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => paeth_spec(a, b, c),
                _ => 0,
            };
            recon[x] = v;
            plane.push(v.wrapping_sub(pred));
        }
        let row_samples = unpack_row(&recon, depth, w * channels);
        for p in 0..w {
            let base = p * channels;
            let px = match plane_type {
                0 => {
                    let g = row_samples[base];
                    [g, g, g, 255]
                }
                2 => [
                    row_samples[base],
                    row_samples[base + 1],
                    row_samples[base + 2],
                    255,
                ],
                3 => {
                    // A colour type 3 field is a PLTE position, not a shade, so
                    // it is read out of the packed row verbatim: the rescale
                    // inside `unpack_row` is the greyscale rule and would turn
                    // an index into something else entirely.
                    let idx = packed_sample(&recon, depth, base) as usize;
                    let rgb = palette
                        .and_then(|pl| pl.get(idx * 3..idx * 3 + 3))
                        .map_or([0, 0, 0], |s| [s[0], s[1], s[2]]);
                    let a = trns.and_then(|t| t.get(idx)).copied().unwrap_or(255);
                    [rgb[0], rgb[1], rgb[2], a]
                }
                4 => [
                    row_samples[base],
                    row_samples[base],
                    row_samples[base],
                    row_samples[base + 1],
                ],
                _ => [
                    row_samples[base],
                    row_samples[base + 1],
                    row_samples[base + 2],
                    row_samples[base + 3],
                ],
            };
            expected.extend_from_slice(&px);
        }
        prev = recon;
    }

    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.push(bit_depth);
    ihdr.push(color_type);
    ihdr.extend_from_slice(&[0, 0, 0]); // deflate, adaptive filtering, no interlace
    bytes.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    if let Some(pl) = palette {
        bytes.extend_from_slice(&png_chunk(b"PLTE", pl));
    }
    if let Some(t) = trns {
        bytes.extend_from_slice(&png_chunk(b"tRNS", t));
    }
    let idat = png_chunk(b"IDAT", &deflate_stored(&plane));
    let end_of_idat_data = bytes.len() + idat.len() - 4;
    bytes.extend_from_slice(&idat);
    let end_of_idat = bytes.len();
    bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
    PngCase {
        bytes,
        expected,
        w: w as u32,
        h: h as u32,
        end_of_idat,
        end_of_idat_data,
    }
}

/// A QOI encoder written from the format spec, so the round-trip property
/// compares two independent readings of the op stream rather than a decoder
/// against itself.
fn qoi_hash(px: [u8; 4]) -> usize {
    (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11) % 64
}

fn encode_qoi(w: u32, h: u32, channels: u8, pixels: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::from(&b"qoif"[..]);
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.push(channels);
    out.push(0); // sRGB with linear alpha
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut run = 0u32;
    for (i, &next) in pixels.iter().enumerate() {
        if next == px {
            run += 1;
            if run == 62 || i + 1 == pixels.len() {
                out.push(0xc0 | (run - 1) as u8);
                run = 0;
            }
        } else {
            if run > 0 {
                out.push(0xc0 | (run - 1) as u8);
                run = 0;
            }
            let slot = qoi_hash(next);
            if index[slot] == next {
                out.push(slot as u8);
            } else {
                index[slot] = next;
                if next[3] == px[3] {
                    let dr = next[0] as i32 - px[0] as i32;
                    let dg = next[1] as i32 - px[1] as i32;
                    let db = next[2] as i32 - px[2] as i32;
                    let (dr_dg, db_dg) = (dr - dg, db - dg);
                    if (-1..=1).contains(&dr) && (-1..=1).contains(&dg) && (-1..=1).contains(&db) {
                        out.push(
                            0x40 | (((dr + 1) as u8) << 4)
                                | (((dg + 1) as u8) << 2)
                                | ((db + 1) as u8),
                        );
                    } else if (-32..=31).contains(&dg)
                        && (-8..=7).contains(&dr_dg)
                        && (-8..=7).contains(&db_dg)
                    {
                        out.push(0x80 | (dg + 32) as u8);
                        out.push(((dr_dg + 8) as u8) << 4 | (db_dg + 8) as u8);
                    } else {
                        out.push(0xff);
                        out.extend_from_slice(&next[..3]);
                    }
                } else {
                    out.push(0xfe);
                    out.extend_from_slice(&next);
                }
            }
        }
        px = next;
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

/// A `BI_RGB` BMP carrying `region` verbatim, so a property can put arbitrary
/// bytes in the row padding as well as in the pixels.
fn build_bmp(w: i32, h: i32, bpp: u16, data_offset: u32, region: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 54];
    out[0..2].copy_from_slice(b"BM");
    out[2..6].copy_from_slice(&(54u32 + region.len() as u32).to_le_bytes());
    out[10..14].copy_from_slice(&data_offset.to_le_bytes());
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&w.to_le_bytes());
    out[22..26].copy_from_slice(&h.to_le_bytes());
    out[26..28].copy_from_slice(&1u16.to_le_bytes());
    out[28..30].copy_from_slice(&bpp.to_le_bytes());
    out[30..34].copy_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(region);
    out
}

/// The BMP row stride: `w * bytes_per_pixel` rounded up to a 4-byte boundary.
fn bmp_row_bytes(w: usize, channels: usize) -> usize {
    (w * channels + 3) & !3
}

// ----------------------------------------------------- format-agnostic input

#[derive(Clone, Copy, Debug)]
enum Format {
    Png,
    Ppm,
    Qoi,
    Bmp,
    Farbfeld,
}

const FORMATS: [Format; 5] = [
    Format::Png,
    Format::Ppm,
    Format::Qoi,
    Format::Bmp,
    Format::Farbfeld,
];

fn decode_with(fmt: Format, bytes: &[u8]) -> Result<Rgba8, String> {
    match fmt {
        Format::Png => decode_png_bytes(bytes),
        Format::Ppm => decode_ppm_bytes(bytes),
        Format::Qoi => decode_qoi_bytes(bytes),
        Format::Bmp => decode_bmp_bytes(bytes),
        Format::Farbfeld => decode_farbfeld_bytes(bytes),
    }
}

/// A small valid file of `fmt`.
fn valid_file(fmt: Format) -> Vec<u8> {
    // Four distinct pixels, so a row read from the wrong offset shifts
    // different values rather than an unremarkable repeat.
    let px: [[u8; 4]; 4] = [
        [255, 0, 0, 255],
        [0, 255, 0, 128],
        [0, 0, 255, 255],
        [17, 34, 51, 200],
    ];
    match fmt {
        Format::Png => {
            let samples: Vec<u16> = px
                .iter()
                .flat_map(|p| p.iter().map(|&c| c as u16))
                .collect();
            build_png(&PngSpec {
                w: 2,
                h: 2,
                bit_depth: 8,
                color_type: 6,
                samples: &samples,
                filters: &[0, 0],
                palette: None,
                trns: None,
            })
            .bytes
        }
        Format::Ppm => {
            let mut v = b"P6\n2 2\n255\n".to_vec();
            for p in px {
                v.extend_from_slice(&p[..3]);
            }
            v
        }
        Format::Qoi => encode_qoi(2, 2, 4, &px),
        Format::Bmp => {
            let row = bmp_row_bytes(2, 4);
            let mut region = vec![0u8; row * 2];
            for (i, p) in px.iter().enumerate() {
                let off = (i / 2) * row + (i % 2) * 4;
                region[off..off + 4].copy_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
            build_bmp(2, 2, 32, 54, &region)
        }
        Format::Farbfeld => {
            let mut v = b"farbfeld".to_vec();
            v.extend_from_slice(&2u32.to_be_bytes());
            v.extend_from_slice(&2u32.to_be_bytes());
            for p in px {
                for c in p {
                    v.extend_from_slice(&((c as u16) << 8).to_be_bytes());
                }
            }
            v
        }
    }
}

// ----------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// The single most important property: no byte sequence, however
    /// malformed, may panic or hand back an image that does not satisfy the
    /// `w*h*4` contract. Every parser is run over the same bytes, because a
    /// wrong magic only steers `decode` to the next decoder - a PNG parser
    /// that tolerates arbitrary junk is still reachable in production.
    #[test]
    fn arbitrary_bytes_never_panic_or_decode_to_a_malformed_image(
        bytes in arbitrary_bytes(),
    ) {
        for (name, outcome) in decode_all(&bytes) {
            match outcome {
                Decoded::Image(img) => {
                    let verdict = well_formed(&img);
                    prop_assert!(
                        verdict.is_ok(),
                        "{}: {} for {:02x?}",
                        name,
                        verdict.unwrap_err(),
                        bytes
                    );
                }
                // `Err(String)` is the documented failure shape; an empty
                // message would lose the "clear error" promise in the crate
                // docs.
                Decoded::Rejected(e) => prop_assert!(!e.is_empty(), "{}: empty error string", name),
                Decoded::Panicked => {
                    prop_assert!(false, "{}: panicked on {:02x?}", name, bytes)
                }
            }
        }
    }

    /// Truncation is the most common real corruption, and the interesting cut
    /// points are inside a format's header and row bytes rather than at the
    /// end. A cut file must still be a clean `Err` or a well-formed `Ok`.
    #[test]
    fn truncation_never_panics(keep in cut_point()) {
        for fmt in FORMATS {
            let file = valid_file(fmt);
            let cut = keep.min(file.len());
            let res = catch_panic(|| decode_with(fmt, &file[..cut]));
            prop_assert!(res.is_ok(), "{}: decoder panicked on a {} byte prefix", fmt_name(fmt), cut);
            if let Ok(img) = res.unwrap() {
                let verdict = well_formed(&img);
                prop_assert!(verdict.is_ok(), "{}: {}", fmt_name(fmt), verdict.unwrap_err());
            }
        }
    }

    /// Bytes after the last pixel are not part of any of these formats, so
    /// they must not move a single decoded byte: a file that had its tail
    /// rewritten, or that was concatenated, still describes the same image.
    #[test]
    fn trailing_garbage_does_not_change_the_decoded_pixels(
        garbage in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        for fmt in FORMATS {
            let file = valid_file(fmt);
            let mut padded = file.clone();
            padded.extend_from_slice(&garbage);
            let a = catch_panic(|| decode_with(fmt, &file));
            let b = catch_panic(|| decode_with(fmt, &padded));
            prop_assert!(a.is_ok() && b.is_ok(), "{}: decoder panicked", fmt_name(fmt));
            prop_assert_eq!(a.unwrap(), b.unwrap(), "{}: trailing bytes changed the image", fmt_name(fmt));
        }
    }

    /// Decoding must be a pure function of the bytes: no cached state, no
    /// dependence on what was decoded before, no address-derived ordering.
    #[test]
    fn decoding_is_deterministic(repeats in 2usize..5) {
        for fmt in FORMATS {
            let file = valid_file(fmt);
            let first = catch_panic(|| decode_with(fmt, &file));
            prop_assert!(first.is_ok(), "{}: decoder panicked", fmt_name(fmt));
            let first = first.unwrap();
            // Repeating the same decode has to keep returning the same pixels:
            // a table or a buffer carried between calls would drift here.
            for _ in 0..repeats {
                let again = catch_panic(|| decode_with(fmt, &file));
                prop_assert!(again.is_ok(), "{}: decoder panicked", fmt_name(fmt));
                prop_assert_eq!(&again.unwrap(), &first, "{}: decode is not deterministic", fmt_name(fmt));
            }
        }
    }

    /// A hostile dimension header has to fail cleanly. Generating `w`/`h`
    /// across the whole `u32` range is what makes the property interesting:
    /// `u32::MAX` is where a wrapping `usize` multiply or an unchecked
    /// allocation would show up.
    #[test]
    fn farbfeld_rejects_out_of_range_dimensions(
        w in hostile_u32(),
        h in hostile_u32(),
        tail in proptest::collection::vec(any::<u8>(), 0..48),
    ) {
        let mut bytes = b"farbfeld".to_vec();
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.extend_from_slice(&tail);
        let res = catch_panic(|| decode_farbfeld_bytes(&bytes));
        prop_assert!(res.is_ok(), "farbfeld: panicked on {}x{}", w, h);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "farbfeld {}x{}: {}", w, h, verdict.unwrap_err());
        }
        // Both a zero side and a side past the cap have to be refused on their
        // own merits: an empty image is not a wallpaper, and a side past the
        // cap is the one that would otherwise drive a huge allocation. The
        // tail here is far too short to satisfy a large claim either way, so
        // only the header check can be what rejected it.
        let rejected = w == 0 || h == 0 || w > MAX_DIM as u32 || h > MAX_DIM as u32;
        if rejected {
            prop_assert!(decode_farbfeld_bytes(&bytes).is_err(),
                "farbfeld: {}x{} outside the accepted range was not rejected", w, h);
        }
    }

    /// A zero side is not an image, and it is rejected before anything is
    /// multiplied or allocated. farbfeld and QOI are the formats that hand a
    /// raw `u32` width straight to `check_dims`, so for them that guard is the
    /// only thing between a 0xN header and an image with an empty buffer - and
    /// an empty buffer passes every size check, because 0*h*4 is 0 bytes.
    #[test]
    fn a_zero_side_is_rejected(w in 1usize..6, h in 1usize..5, zero_width in any::<bool>()) {
        let (dw, dh) = if zero_width {
            (0u32, h as u32)
        } else {
            (w as u32, 0)
        };
        let mut ff = b"farbfeld".to_vec();
        ff.extend_from_slice(&dw.to_be_bytes());
        ff.extend_from_slice(&dh.to_be_bytes());
        ff.resize(16 + 8 * w * h, 0x5a); // a complete payload: only the header is wrong
        prop_assert!(decode_farbfeld_bytes(&ff).is_err(), "farbfeld: {}x{} was accepted", dw, dh);
        let mut qoi = b"qoif".to_vec();
        qoi.extend_from_slice(&dw.to_be_bytes());
        qoi.extend_from_slice(&dh.to_be_bytes());
        qoi.push(4);
        qoi.push(0);
        qoi.extend_from_slice(&[0xff, 1, 2, 3]);
        qoi.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        prop_assert!(decode_qoi_bytes(&qoi).is_err(), "qoi: {}x{} was accepted", dw, dh);
    }

    /// A dimension just past `MAX_DIM` is small enough to carry a complete
    /// payload, so the cap is the only thing that can reject it. Without the
    /// cap the file decodes happily, which is exactly the wallpaper-sized
    /// allocation the constant exists to prevent.
    #[test]
    fn farbfeld_rejects_dimensions_past_max_dim(w in (MAX_DIM as u32 + 1)..=(MAX_DIM as u32 + 4)) {
        let h = 1u32;
        let mut bytes = b"farbfeld".to_vec();
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.resize(16 + 8 * w as usize * h as usize, 0x5a);
        prop_assert!(decode_farbfeld_bytes(&bytes).is_err(),
            "farbfeld: {}x{} past MAX_DIM decoded with a full payload", w, h);
    }

    /// PPM dimensions arrive as text, so the parser also has to survive
    /// negative signs, leading zeros, and values that do not fit `i64`.
    #[test]
    fn ppm_rejects_out_of_range_dimensions(w in hostile_i64(), h in hostile_i64()) {
        let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
        bytes.resize(96, 0x33);
        let res = catch_panic(|| decode_ppm_bytes(&bytes));
        prop_assert!(res.is_ok(), "ppm: panicked on {}x{}", w, h);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "ppm {}x{}: {}", w, h, verdict.unwrap_err());
        }
        // Dimensions arrive as text, so they also have to survive a sign, a
        // leading zero and a value that does not fit `i64`.
        let bad_dims =
            w <= 0 || (w as u64) > MAX_DIM as u64 || h <= 0 || (h as u64) > MAX_DIM as u64;
        if bad_dims {
            prop_assert!(decode_ppm_bytes(&bytes).is_err(),
                "ppm: {}x{} outside the accepted range was not rejected", w, h);
        }
    }

    /// Only maxval 255 is defined. The payload below is complete for any
    /// dimension the decoder accepts, so an unsupported maxval has to be the
    /// reason the file is refused - nothing else about it is wrong.
    #[test]
    fn ppm_rejects_unsupported_maxval(w in 1usize..5, h in 1usize..4, maxval in hostile_i64()) {
        let mut bytes = format!("P6\n{w} {h}\n{maxval}\n").into_bytes();
        bytes.resize(96, 0x33);
        let res = catch_panic(|| decode_ppm_bytes(&bytes));
        prop_assert!(res.is_ok(), "ppm: panicked on maxval {}", maxval);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "ppm maxval={}: {}", maxval, verdict.unwrap_err());
        }
        if maxval != 255 {
            prop_assert!(decode_ppm_bytes(&bytes).is_err(), "ppm: maxval {} was not rejected", maxval);
        }
    }

    /// Every header field the BMP path consults, generated across a range wide
    /// enough to include the values the decoder is supposed to refuse. The
    /// payload is fully populated, so a rejection can only come from the
    /// header.
    #[test]
    fn bmp_validates_the_dib_depth_compression_and_offset_fields(
        field in 0usize..4,
        value in any::<u32>(),
    ) {
        // A payload big enough for either supported depth, so a rejection can
        // only be about the header field and never about a short read.
        let region = vec![0x5a; 48 * 4];
        let mut bytes = build_bmp(12, 4, 24, 54, &region);
        let accepted = match field {
            0 => {
                bytes[14..18].copy_from_slice(&value.to_le_bytes()); // DIB header size
                value >= 40
            }
            1 => {
                let bpp = value as u16;
                bytes[28..30].copy_from_slice(&bpp.to_le_bytes());
                bpp == 24 || bpp == 32
            }
            2 => {
                bytes[30..34].copy_from_slice(&value.to_le_bytes()); // compression
                value == 0 // BI_RGB and nothing else
            }
            _ => {
                bytes[10..14].copy_from_slice(&value.to_le_bytes()); // pixel data offset
                (54..=bytes.len() as u32).contains(&value)
            }
        };
        let res = catch_panic(|| decode_bmp_bytes(&bytes));
        prop_assert!(res.is_ok(), "bmp: panicked with header field {} = {}", field, value);
        match res.unwrap() {
            Ok(img) => {
                let verdict = well_formed(&img);
                prop_assert!(verdict.is_ok(), "bmp field={} value={}: {}", field, value, verdict.unwrap_err());
                prop_assert!(accepted, "bmp: accepted header field {} set to {}", field, value);
            }
            Err(_) => prop_assert!(!accepted, "bmp: refused header field {} set to {}", field, value),
        }
    }

    /// A zero width or a zero height is not a BMP. The payload is sized for
    /// the dimensions under test, so the dimension is the only possible reason
    /// to refuse the file. A negative height is a different matter - it is a
    /// top-down image, and `bmp_row_stride_and_orientation_...` covers that.
    #[test]
    fn bmp_rejects_a_zero_side(w in 1usize..6, h in 1usize..5, side in 0usize..2) {
        let (bw, bh) = if side == 0 { (0usize, h) } else { (w, 0) };
        let region = vec![0x5a; bmp_row_bytes(bw.max(1), 3) * bh.max(1)];
        let bytes = build_bmp(bw as i32, bh as i32, 24, 54, &region);
        let res = catch_panic(|| decode_bmp_bytes(&bytes));
        prop_assert!(res.is_ok(), "bmp: panicked on {}x{}", bw, bh);
        prop_assert!(res.unwrap().is_err(), "bmp: {}x{} was accepted", bw, bh);
    }

    /// A QOI pixel is 3 or 4 bytes, so the channel count decides the size of
    /// every later read. Only 3 and 4 are defined; anything else has to be
    /// refused rather than reinterpreted. The rest of the stream is a valid
    /// one, so the channel byte is the only possible reason to fail.
    #[test]
    fn qoi_rejects_unsupported_channel_counts(channels in any::<u8>(), w in 1usize..6, h in 1usize..4) {
        let pixels: Vec<[u8; 4]> = (0..w * h)
            .map(|i| [i as u8, (i * 3) as u8, (i * 7) as u8, 200])
            .collect();
        let mut bytes = encode_qoi(w as u32, h as u32, 4, &pixels);
        bytes[12] = channels; // the only header byte that changes
        let res = catch_panic(|| decode_qoi_bytes(&bytes));
        prop_assert!(res.is_ok(), "qoi: panicked on channel count {}", channels);
        if channels == 3 || channels == 4 {
            let img = res.unwrap().expect("a defined channel count must decode");
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "qoi ch={}: {}", channels, verdict.unwrap_err());
        } else {
            prop_assert!(res.unwrap().is_err(), "qoi: channel count {} was not rejected", channels);
        }
    }

    /// QOI dimensions come straight out of the header as `u32`, and a stream
    /// far too short for what it claims has to be refused, not read past.
    #[test]
    fn qoi_rejects_out_of_range_dimensions(
        w in hostile_u32(),
        h in hostile_u32(),
        tail in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
        let mut bytes = b"qoif".to_vec();
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        bytes.extend_from_slice(&tail);
        let res = catch_panic(|| decode_qoi_bytes(&bytes));
        prop_assert!(res.is_ok(), "qoi: panicked on {}x{}", w, h);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "qoi {}x{}: {}", w, h, verdict.unwrap_err());
        }
        let rejected = w == 0 || h == 0 || w > MAX_DIM as u32 || h > MAX_DIM as u32;
        if rejected {
            prop_assert!(decode_qoi_bytes(&bytes).is_err(),
                "qoi: {}x{} outside the accepted range was not rejected", w, h);
        }
    }

    /// Bit depth and colour type are the two PNG fields that decide how the
    /// scanline is laid out. The pixel payload here is complete and correct for
    /// whatever pair is claimed, so the property is two-sided: an unsupported
    /// pair must be refused, and a supported one must decode. "Either" would
    /// let a decoder that refused everything pass.
    #[test]
    fn png_rejects_unsupported_bit_depths_and_colour_types(
        bit_depth in any::<u8>(),
        color_type in any::<u8>(),
        w in 1usize..6,
        h in 1usize..4,
    ) {
        let channels = png_channels(color_type);
        let samples = vec![1u16; (w * h * channels.max(1)).min(512)];
        let filters = vec![0u8; h];
        let palette = (color_type == 3).then(|| vec![1u8, 2, 3, 4, 5, 6]);
        let case = build_png(&PngSpec {
                w,
                h,
                bit_depth,
                color_type,
                samples: &samples,
                filters: &filters,
                palette: palette.as_deref(),
                trns: None,
            });
        let supported_depth = matches!(bit_depth, 1 | 2 | 4 | 8 | 16);
        let supported_type = png_channels(color_type) > 0;
        let res = catch_panic(|| decode_png_bytes(&case.bytes));
        prop_assert!(res.is_ok(), "png: panicked on bd={} ct={}", bit_depth, color_type);
        if supported_depth && supported_type {
            let img = res.unwrap().expect("supported header must decode");
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(),
                "png bd={} ct={} {}x{}: {}", bit_depth, color_type, w, h, verdict.unwrap_err());
            prop_assert_eq!((img.w, img.h), (case.w, case.h), "png: wrong dimensions");
        } else {
            prop_assert!(res.unwrap().is_err(),
                "png: accepted bit depth {} with colour type {}", bit_depth, color_type);
        }
    }

    /// The chunk length is a hostile `u32` read before the chunk is known to
    /// fit. Anything longer than the rest of the file has to be refused
    /// without a 4 GiB `extend_from_slice`.
    #[test]
    fn png_validates_the_chunk_length_before_using_it(len in any::<u32>()) {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(b"IDAT");
        bytes.extend_from_slice(&[0u8; 8]);
        let res = catch_panic(|| decode_png_bytes(&bytes));
        prop_assert!(res.is_ok(), "png: panicked on chunk length {}", len);
        if len > 8 {
            prop_assert!(res.unwrap().is_err(), "png: accepted an {} byte chunk from an 8 byte payload", len);
        }
    }

    /// A short pixel region must be an error, never a partially filled image:
    /// silently returning the pixels that happened to be there is how a
    /// wallpaper turns into a grey rectangle.
    #[test]
    fn ppm_rejects_a_short_pixel_region(w in 1usize..5, h in 1usize..5, drop in 1usize..12) {
        let header = format!("P6\n{w} {h}\n255\n");
        let mut bytes = header.clone().into_bytes();
        bytes.resize(bytes.len() + w * h * 3, 0x5a);
        let keep = bytes.len() - drop.min(w * h * 3);
        let res = catch_panic(|| decode_ppm_bytes(&bytes[..keep]));
        prop_assert!(res.is_ok(), "ppm: panicked on a {} byte short read", drop);
        prop_assert!(res.unwrap().is_err(), "ppm: {}x{} accepted {} bytes too few", w, h, drop);
    }

    /// Same for farbfeld, where the 16-bit payload is the only thing between
    /// the header and the pixels.
    #[test]
    fn farbfeld_rejects_a_short_pixel_region(w in 1usize..5, h in 1usize..5, drop in 1usize..12) {
        let mut bytes = b"farbfeld".to_vec();
        bytes.extend_from_slice(&(w as u32).to_be_bytes());
        bytes.extend_from_slice(&(h as u32).to_be_bytes());
        bytes.resize(16 + 8 * w * h, 0x5a);
        let keep = bytes.len() - drop.min(8 * w * h);
        let res = catch_panic(|| decode_farbfeld_bytes(&bytes[..keep]));
        prop_assert!(res.is_ok(), "farbfeld: panicked on a {} byte short read", drop);
        prop_assert!(res.unwrap().is_err(), "farbfeld: {}x{} accepted {} bytes too few", w, h, drop);
    }

    /// BMP row layout: a row starts at `row_bytes`, not at `w * channels`, and
    /// rows are stored bottom-up unless the height is negative. Widths whose
    /// pixel span is not a multiple of four are what make the padding
    /// observable, and the padding bytes are random here so an implementation
    /// that read them as pixels would be caught.
    #[test]
    fn bmp_row_stride_and_orientation_match_the_bytes_written(
        w in 1usize..12,
        h in 1usize..6,
        channels in proptest::sample::select(vec![3usize, 4]),
        top_down in any::<bool>(),
        region in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let row_bytes = bmp_row_bytes(w, channels);
        let mut region = region;
        region.resize(row_bytes * h, 0x5a);
        let bytes = build_bmp(w as i32, if top_down { -(h as i32) } else { h as i32 },
            (channels * 8) as u16, 54, &region);
        let img = decode_bmp_bytes(&bytes).expect("well-formed BMP");
        prop_assert_eq!((img.w, img.h), (w as u32, h as u32), "bmp: wrong dimensions");
        prop_assert!(img.is_valid(), "bmp: buffer is not w*h*4");
        let mut expected = Vec::new();
        for row in 0..h {
            let src_row = if top_down { row } else { h - 1 - row };
            for col in 0..w {
                let p = src_row * row_bytes + col * channels;
                expected.extend_from_slice(&[
                    region[p + 2],
                    region[p + 1],
                    region[p],
                    if channels == 4 { region[p + 3] } else { 255 },
                ]);
            }
        }
        prop_assert_eq!(img.data, expected, "bmp {}x{} channels={} top_down={}", w, h, channels, top_down);
    }

    /// QOI's six ops encode the same pixel stream in very different ways. A
    /// property over random pixels only means something if the encoder
    /// actually emits every op, so the pixels are drawn from a handful of
    /// values that force runs, index hits, small diffs and luma changes.
    #[test]
    fn qoi_decodes_to_the_pixels_the_encoder_was_given(
        w in 1usize..6,
        h in 1usize..5,
        channels in proptest::sample::select(vec![3u8, 4]),
        pixels in proptest::collection::vec(any::<u8>(), 0..48),
    ) {
        let count = w * h;
        // A small alphabet keeps runs and index hits frequent; the occasional
        // far-away value pulls in RGB/RGBA and LUMA.
        let pool: [[u8; 4]; 4] = [
            [0, 0, 0, 255],
            [10, 20, 30, 255],
            [11, 21, 29, 255],
            [200, 100, 40, 128],
        ];
        let mut rgba = Vec::with_capacity(count);
        for i in 0..count {
            let pick = pixels.get(i).copied().unwrap_or(0) as usize;
            let mut px = pool[pick % pool.len()];
            if pick >= 8 {
                px = [pick as u8, (pick >> 3) as u8, (pick >> 5) as u8, if pick.is_multiple_of(3) { 7 } else { 255 }];
            }
            if channels == 3 {
                px[3] = 255;
            }
            rgba.push(px);
        }
        let bytes = encode_qoi(w as u32, h as u32, channels, &rgba);
        let img = decode_qoi_bytes(&bytes).expect("well-formed QOI");
        prop_assert_eq!((img.w, img.h), (w as u32, h as u32), "qoi: wrong dimensions");
        prop_assert!(img.is_valid(), "qoi: buffer is not w*h*4");
        let mut expected = Vec::new();
        for px in &rgba {
            expected.extend_from_slice(&if channels == 3 {
                [px[0], px[1], px[2], 255]
            } else {
                *px
            });
        }
        prop_assert_eq!(img.data, expected, "qoi {}x{} channels={}", w, h, channels);
    }

    /// farbfeld stores 16 bits per channel, big-endian. The decoder keeps the
    /// high byte, so a random payload pins down both the byte order and the
    /// truncation at once.
    #[test]
    fn farbfeld_keeps_the_high_byte_of_every_16_bit_sample(
        w in 1usize..6,
        h in 1usize..5,
        samples in proptest::collection::vec(any::<u16>(), 0..48),
    ) {
        let mut bytes = b"farbfeld".to_vec();
        bytes.extend_from_slice(&(w as u32).to_be_bytes());
        bytes.extend_from_slice(&(h as u32).to_be_bytes());
        let mut expected = Vec::new();
        for i in 0..w * h * 4 {
            let s = *samples.get(i).unwrap_or(&0x1234);
            bytes.extend_from_slice(&s.to_be_bytes());
            expected.push((s >> 8) as u8);
        }
        let img = decode_farbfeld_bytes(&bytes).expect("well-formed farbfeld");
        prop_assert_eq!((img.w, img.h), (w as u32, h as u32), "farbfeld: wrong dimensions");
        prop_assert!(img.is_valid(), "farbfeld: buffer is not w*h*4");
        prop_assert_eq!(img.data, expected, "farbfeld {}x{}", w, h);
    }

    /// A PPM header may carry `#` comments and arbitrary whitespace between
    /// every pair of tokens, and exactly one whitespace byte separates the
    /// header from binary data that may itself begin with a whitespace byte.
    /// A pixel whose first byte is 0x20 is the case that catches a parser
    /// which skips leading data bytes instead of exactly one.
    #[test]
    fn ppm_header_comments_and_whitespace_do_not_shift_the_pixels(
        w in 1usize..6,
        h in 1usize..5,
        comment in any::<bool>(),
        first in proptest::sample::select(vec![0x20u8, 0x0a, 0x09, 0x00, 0xff]),
        pixels in proptest::collection::vec(any::<u8>(), 0..48),
    ) {
        let mut header = String::from("P6\n");
        if comment {
            header.push_str("# a comment, and a second one\n#\n");
        }
        header.push_str(&format!("  {w}\t{h}  \n# trailing comment\n255 "));
        let mut bytes = header.into_bytes();
        let mut rgb = Vec::with_capacity(w * h * 3);
        for i in 0..w * h * 3 {
            let c = if i == 0 {
                first
            } else {
                *pixels.get(i).unwrap_or(&0x5a)
            };
            bytes.push(c);
            rgb.push(c);
        }
        let img = decode_ppm_bytes(&bytes).expect("well-formed PPM");
        prop_assert_eq!((img.w, img.h), (w as u32, h as u32), "ppm: wrong dimensions");
        let mut want = Vec::new();
        for px in rgb.chunks(3) {
            want.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        prop_assert_eq!(img.data, want, "ppm {}x{} comment={}", w, h, comment);
    }

    /// The five scanline filters, over random dimensions, colour types, bit
    /// depths and a per-row filter choice. The reference side filters forward
    /// from the spec, so this pins the reconstruction, the row stride and the
    /// left/above/upper-left byte positions together.
    #[test]
    fn png_filters_reconstruct_the_scanlines_they_encode(
        w in 1usize..9,
        h in 1usize..6,
        color_type in prop_oneof![Just(0u8), Just(2), Just(4), Just(6)],
        bit_depth in prop_oneof![Just(1u8), Just(2), Just(4), Just(8), Just(16)],
        samples in proptest::collection::vec(any::<u16>(), 0..512),
        filters in proptest::collection::vec(any::<u8>(), 0..6),
    ) {
        let channels = png_channels(color_type);
        let count = w * h * channels;
        let mut vals: Vec<u16> = (0..count)
            .map(|i| match bit_depth {
                16 => *samples.get(i).unwrap_or(&0xbeef),
                8 => *samples.get(i).unwrap_or(&0x5a) & 0xff,
                bd => *samples.get(i).unwrap_or(&1) & ((1u16 << bd) - 1),
            })
            .collect();
        if bit_depth == 8 {
            // Guarantee at least one zero: an all-255 row is indistinguishable
            // from a broken filter that returns the source unchanged.
            vals[0] = 0;
        }
        let filters: Vec<u8> = (0..h).map(|y| *filters.get(y).unwrap_or(&0) % 5).collect();
        let case = build_png(&PngSpec {
                w,
                h,
                bit_depth,
                color_type,
                samples: &vals,
                filters: &filters,
                palette: None,
                trns: None,
            });
        let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        prop_assert_eq!((img.w, img.h), (case.w, case.h), "png: wrong dimensions");
        prop_assert!(img.is_valid(), "png: buffer is not w*h*4");
        prop_assert_eq!(img.data, case.expected,
            "png {}x{} bd={} ct={} filters={:?}", w, h, bit_depth, color_type, filters);
    }

    /// The same reconstruction, checked through the bit-depth expansion: a
    /// sub-byte sample is rescaled to 0..=255 round-half-up and a 16-bit one
    /// keeps its high byte. Bit depths below eight are what make the packed
    /// row stride and the MSB-first order observable.
    #[test]
    fn png_sample_expansion_matches_the_declared_bit_depth(
        w in 1usize..9,
        h in 1usize..5,
        color_type in prop_oneof![Just(0u8), Just(2), Just(4), Just(6)],
        bit_depth in prop_oneof![Just(1u8), Just(2), Just(4), Just(8), Just(16)],
        values in proptest::collection::vec(any::<u8>(), 0..128),
    ) {
        let channels = png_channels(color_type);
        let count = w * h * channels;
        let max = match bit_depth {
            16 => 0xffffu16,
            8 => 0xff,
            bd => (1u16 << bd) - 1,
        };
        let vals: Vec<u16> = (0..count)
            .map(|i| match bit_depth {
                // Cover both extremes of every depth so the rescale bounds
                // are always hit, not just the middle.
                0 => if i % 2 == 0 { 0 } else { max },
                _ => (*values.get(i).unwrap_or(&0x5a) as u16) & max,
            })
            .collect();
        let case = build_png(&PngSpec {
                w,
                h,
                bit_depth,
                color_type,
                samples: &vals,
                filters: &vec![0u8; h],
                palette: None,
                trns: None,
            });
        let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        prop_assert!(img.is_valid(), "png: buffer is not w*h*4");
        prop_assert_eq!(img.data, case.expected,
            "png {}x{} bd={} ct={}", w, h, bit_depth, color_type);
    }

    /// Only filter types 0..=4 exist. A larger value in a row's filter byte
    /// has to be reported, not applied as some default: a filter that is not
    /// rejected silently shifts every later row.
    #[test]
    fn png_rejects_unknown_filter_bytes(
        bad in 5u8..=255,
        w in 1usize..6,
        h in 1usize..4,
        color_type in prop_oneof![Just(0u8), Just(2), Just(4), Just(6)],
        bit_depth in prop_oneof![Just(8u8), Just(16)],
    ) {
        let channels = png_channels(color_type);
        let vals = vec![0x33u16; w * h * channels];
        let mut filters = vec![0u8; h];
        filters[0] = bad;
        let case = build_png(&PngSpec {
                w,
                h,
                bit_depth,
                color_type,
                samples: &vals,
                filters: &filters,
                palette: None,
                trns: None,
            });
        prop_assert!(decode_png_bytes(&case.bytes).is_err(),
            "png: filter byte {} was accepted", bad);
    }

    /// Colour type 3 samples are palette indices and the format does not
    /// rescale them: a 4-bit sample of 1 means palette entry 1. The decoder
    /// used to expand every sub-byte sample into 0..=255 before using it as an
    /// index, so an index of 1 became 255, landed outside the palette and
    /// decoded to black.
    ///
    /// Minimized counterexample: 2x1, bit depth 4, colour type 3, palette
    /// [10,20,30, 200,150,160, 170,180], samples [0, 1]. The second pixel
    /// decoded to `00 00 00 ff` where the palette says `96 a0 aa ff`.
    ///
    /// The expected pixels are derived from the packed bytes rather than from
    /// `build_png`'s own `expected`, so the reference side of this property is
    /// the format's MSB-first bit layout and nothing else.
    #[test]
    fn png_palette_indices_are_used_verbatim(
        w in 1usize..6,
        h in 1usize..3,
        bit_depth in prop_oneof![Just(1u8), Just(2), Just(4)],
        samples in proptest::collection::vec(any::<u8>(), 0..24),
    ) {
        let entries = 1usize << bit_depth;
        let palette: Vec<u8> = (0..entries * 3).map(|i| (i as u8).wrapping_mul(37) | 1).collect();
        let vals: Vec<u16> = (0..w * h)
            .map(|i| (*samples.get(i).unwrap_or(&0) as u16) & (entries as u16 - 1))
            .collect();
        let case = build_png(&PngSpec {
            w,
            h,
            bit_depth,
            color_type: 3,
            samples: &vals,
            filters: &vec![0u8; h],
            palette: Some(&palette),
            trns: None,
        });
        let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        let mut expected = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let packed = pack_row(&vals[y * w..(y + 1) * w], bit_depth);
            for x in 0..w {
                let idx = packed_sample(&packed, bit_depth, x) as usize;
                expected.extend_from_slice(&[
                    palette[idx * 3],
                    palette[idx * 3 + 1],
                    palette[idx * 3 + 2],
                    255,
                ]);
            }
        }
        prop_assert_eq!(img.data, expected,
            "png {}x{} bd={} ct=3: palette indices are not looked up verbatim", w, h, bit_depth);
    }

    /// The DEFLATE decoder is the only table-driven state machine in the crate
    /// - canonical Huffman codes, length/distance tables, back-references into
    /// the output produced so far - so its input is fuzzed directly: a header
    /// that says how many pixels are expected, and arbitrary bytes where the
    /// compressed stream should be. Everything from the first bad bit onwards
    /// has to be reported rather than followed.
    #[test]
    fn png_survives_hostile_compressed_data(
        stream in proptest::collection::vec(any::<u8>(), 0..256),
        w in 1usize..8,
        h in 1usize..6,
        bit_depth in prop_oneof![Just(8u8), Just(16)],
        color_type in prop_oneof![Just(0u8), Just(2), Just(4), Just(6)],
    ) {
        let mut idat = vec![0x78, 0x01]; // zlib header, skipped by the decoder
        idat.extend_from_slice(&stream);
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
        bytes.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        bytes.extend_from_slice(&png_chunk(b"IDAT", &idat));
        bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
        let res = catch_panic(|| decode_png_bytes(&bytes));
        prop_assert!(res.is_ok(), "png: panicked on {} bytes of compressed data", stream.len());
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(),
                "png {}x{} bd={} ct={}: {}", w, h, bit_depth, color_type, verdict.unwrap_err());
        }
    }

    /// The header fixes the pixel count, so ops that keep producing pixels
    /// past it must not lengthen the buffer. A stream that overproduces is
    /// still a well-formed image as far as the caller is concerned - it just
    /// has to be exactly `w*h*4` bytes, which is what `is_valid` checks.
    #[test]
    fn qoi_rejects_ops_past_the_declared_pixel_count(
        w in 1usize..6,
        h in 1usize..4,
        extra in 1usize..40,
    ) {
        let pixels: Vec<[u8; 4]> = (0..w * h).map(|i| [i as u8, 1, 2, 255]).collect();
        let mut bytes = encode_qoi(w as u32, h as u32, 4, &pixels);
        bytes.truncate(bytes.len() - 8); // drop the end marker
        for i in 0..extra {
            bytes.push(0xff); // QOI_OP_RGB, well past the last pixel
            bytes.extend_from_slice(&[i as u8, i as u8, i as u8]);
        }
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        let res = catch_panic(|| decode_qoi_bytes(&bytes));
        prop_assert!(res.is_ok(), "qoi: panicked with {} extra ops", extra);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "qoi {}x{} with {} extra ops: {}",
                w, h, extra, verdict.unwrap_err());
        }
    }

    /// A `RUN` op may be longer than the pixels the header asked for, up to
    /// the format's 62. The decoder has to stop at the declared count rather
    /// than emit the whole run.
    #[test]
    fn qoi_run_length_cannot_overrun_the_declared_pixel_count(
        w in 1usize..4,
        h in 1usize..3,
        run in 2u8..=62,
    ) {
        let mut bytes = b"qoif".to_vec();
        bytes.extend_from_slice(&(w as u32).to_be_bytes());
        bytes.extend_from_slice(&(h as u32).to_be_bytes());
        bytes.push(4);
        bytes.push(0);
        bytes.push(0xc0 | (run - 1)); // QOI_OP_RUN
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        let res = catch_panic(|| decode_qoi_bytes(&bytes));
        prop_assert!(res.is_ok(), "qoi: panicked on a run of {}", run);
        if let Ok(img) = res.unwrap() {
            let verdict = well_formed(&img);
            prop_assert!(verdict.is_ok(), "qoi {}x{} with a run of {}: {}",
                w, h, run, verdict.unwrap_err());
        }
    }

    /// The `IDAT` payload is the whole image; anything after it - a missing
    /// `IEND`, a rewritten CRC, concatenated data - cannot change a pixel.
    #[test]
    fn a_png_cut_after_its_pixel_data_still_decodes(w in 1usize..6, h in 1usize..4) {
        let vals: Vec<u16> = (0..w * h * 4).map(|i| (i * 37) as u16).collect();
        let case = build_png(&PngSpec {
                w,
                h,
                bit_depth: 8,
                color_type: 6,
                samples: &vals,
                filters: &vec![0u8; h],
                palette: None,
                trns: None,
            });
        let full = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        let without_iend = decode_png_bytes(&case.bytes[..case.end_of_idat])
            .expect("PNG without IEND");
        prop_assert_eq!(&without_iend, &full, "png {}x{}: dropping IEND changed the image", w, h);
        // The CRC is not consulted, so a chunk whose CRC never arrived is not a
        // reason to refuse the pixels that did.
        let without_crc = decode_png_bytes(&case.bytes[..case.end_of_idat_data])
            .expect("PNG without the IDAT CRC");
        prop_assert_eq!(&without_crc, &full, "png {}x{}: dropping the CRC changed the image", w, h);
    }
}

fn fmt_name(fmt: Format) -> &'static str {
    match fmt {
        Format::Png => "png",
        Format::Ppm => "ppm",
        Format::Qoi => "qoi",
        Format::Bmp => "bmp",
        Format::Farbfeld => "farbfeld",
    }
}

// ---------------------------------------------------------- deterministic runs

/// Reach the lengths a uniform random draw essentially never produces: empty
/// input, a single byte, and each format's magic with nothing behind it.
#[test]
fn degenerate_inputs_are_rejected_cleanly() {
    for bytes in [
        vec![],
        vec![0u8],
        vec![0xffu8],
        b"P6".to_vec(),
        b"qoif".to_vec(),
        b"BM".to_vec(),
        b"farbfeld".to_vec(),
        b"\x89PNG\r\n\x1a\n".to_vec(),
    ] {
        for (name, outcome) in decode_all(&bytes) {
            match outcome {
                Decoded::Image(img) => panic!(
                    "{name}: decoded {} bytes to {}x{}",
                    bytes.len(),
                    img.w,
                    img.h
                ),
                Decoded::Panicked => panic!("{name}: panicked on {} bytes", bytes.len()),
                Decoded::Rejected(_) => {}
            }
        }
    }
}

/// Cutting a valid file at every single offset, rather than at a handful of
/// hand-picked points: the interesting cuts are inside a header field and
/// inside the row bytes, and only an exhaustive sweep hits all of them.
#[test]
fn every_truncation_of_a_valid_file_is_safe() {
    for fmt in FORMATS {
        let file = valid_file(fmt);
        let full = decode_with(fmt, &file);
        assert!(full.is_ok(), "{}: fixture does not decode", fmt_name(fmt));
        for cut in 0..=file.len() {
            match catch_panic(|| decode_with(fmt, &file[..cut])) {
                Ok(Ok(img)) => assert!(
                    well_formed(&img).is_ok(),
                    "{}: cut at {} gave {}",
                    fmt_name(fmt),
                    cut,
                    well_formed(&img).unwrap_err()
                ),
                Ok(Err(_)) => {}
                Err(_) => panic!("{}: panicked on a {} byte prefix", fmt_name(fmt), cut),
            }
        }
        // Flipping a single byte anywhere in the file is the cheapest
        // corruption there is; the contract is that it stays clean.
        for i in 0..file.len() {
            for bit in 0..8 {
                let mut corrupt = file.clone();
                corrupt[i] ^= 1 << bit;
                match catch_panic(|| decode_with(fmt, &corrupt)) {
                    Ok(Ok(img)) => assert!(
                        well_formed(&img).is_ok(),
                        "{}: byte {} bit {} gave {}",
                        fmt_name(fmt),
                        i,
                        bit,
                        well_formed(&img).unwrap_err()
                    ),
                    Ok(Err(_)) => {}
                    Err(_) => panic!(
                        "{}: panicked with byte {} bit {} flipped",
                        fmt_name(fmt),
                        i,
                        bit
                    ),
                }
            }
        }
    }
}

// ----------------------------------------------------- packed palette indices

/// A palette whose entry `i` is `(i, 255 - i, 128)`. The red channel alone
/// identifies the entry, so an index that arrives shifted, rescaled, or out of
/// range cannot decode to the colour a test is looking for - and neither can
/// the black the decoder substitutes for an index past the end of the palette,
/// because no entry here is black.
fn ramp_palette(entries: usize) -> Vec<u8> {
    (0..entries)
        .flat_map(|i| [i as u8, (255 - i) as u8, 128])
        .collect()
}

/// The RGBA an indexed image owes its caller: palette entry `i` verbatim,
/// opaque.
fn indexed_rgba(palette: &[u8], indices: &[u16]) -> Vec<u8> {
    indices
        .iter()
        .flat_map(|&i| {
            let k = i as usize * 3;
            [palette[k], palette[k + 1], palette[k + 2], 255]
        })
        .collect()
}

/// Eight one-bit indices share a single byte, so each bit of it is a different
/// pixel. Bit depth 1 leaves the least room for a mistake: a field rescaled
/// into the byte range sends index 1 to 255, which is nowhere near entry 1 of
/// a two-entry palette.
#[test]
fn one_bit_indices_share_one_byte() {
    let palette = ramp_palette(2);
    let indices = [0u16, 1, 1, 0, 1, 0, 0, 1];
    assert_eq!(pack_row(&indices, 1), vec![0b0110_1001u8]);
    let case = build_png(&PngSpec {
        w: 8,
        h: 1,
        bit_depth: 1,
        color_type: 3,
        samples: &indices,
        filters: &[0],
        palette: Some(&palette),
        trns: None,
    });
    let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
    assert_eq!((img.w, img.h), (8, 1));
    assert!(img.is_valid());
    assert_eq!(img.data, indexed_rgba(&palette, &indices));
}

/// Four two-bit indices share a byte. The row walks all four entries and then
/// walks them back, so a lookup that is off by a single index shows up as a
/// wrong colour on a specific pixel instead of as a uniform shift of the row.
#[test]
fn two_bit_indices_share_one_byte() {
    let palette = ramp_palette(4);
    let indices = [0u16, 1, 2, 3, 0, 3, 2, 1];
    assert_eq!(pack_row(&indices, 2), vec![0b0001_1011u8, 0b0011_1001u8]);
    let case = build_png(&PngSpec {
        w: 8,
        h: 1,
        bit_depth: 2,
        color_type: 3,
        samples: &indices,
        filters: &[0],
        palette: Some(&palette),
        trns: None,
    });
    let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
    assert_eq!((img.w, img.h), (8, 1));
    assert!(img.is_valid());
    assert_eq!(img.data, indexed_rgba(&palette, &indices));
}

/// The minimized counterexample behind `png_palette_indices_are_used_verbatim`:
/// 2x1 at bit depth 4, so both pixels live in the one byte `0x01`. Entry 0 is
/// `[10, 20, 30]` and entry 1 is `[200, 150, 160]`; rescaling the field first
/// sent index 1 past the end of an eight-entry palette and it decoded to
/// `00 00 00 ff`.
#[test]
fn four_bit_indices_in_one_byte_are_not_rescaled() {
    let palette = [10u8, 20, 30, 200, 150, 160, 170, 180];
    let indices = [0u16, 1];
    assert_eq!(pack_row(&indices, 4), vec![0x01u8]);
    let case = build_png(&PngSpec {
        w: 2,
        h: 1,
        bit_depth: 4,
        color_type: 3,
        samples: &indices,
        filters: &[0],
        palette: Some(&palette),
        trns: None,
    });
    let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
    assert_eq!((img.w, img.h), (2, 1));
    assert_eq!(&img.data[0..4], &[10, 20, 30, 255]);
    assert_eq!(&img.data[4..8], &[200, 150, 160, 255]);
}

/// At bit depth 8 the index *is* the byte, so this is the reference the
/// sub-byte depths have to agree with rather than a rescaled fraction of it.
#[test]
fn eight_bit_indices_use_one_byte_per_index() {
    let palette = ramp_palette(5);
    let indices = [0u16, 4, 2, 1, 3];
    let case = build_png(&PngSpec {
        w: 5,
        h: 1,
        bit_depth: 8,
        color_type: 3,
        samples: &indices,
        filters: &[0],
        palette: Some(&palette),
        trns: None,
    });
    let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
    assert_eq!((img.w, img.h), (5, 1));
    assert!(img.is_valid());
    assert_eq!(img.data, indexed_rgba(&palette, &indices));
}

/// Every index a depth allows has to be reachable, at every depth. One row
/// enumerates `0..2^bd` in order, so an index that is dropped or overshot
/// leaves a wrong colour behind rather than shifting the whole row - which is
/// what a uniform rescale or an off-by-one field would do. The row is padded to
/// a whole number of bytes, so the two-entry case really is two pixels in one.
#[test]
fn every_index_of_every_depth_is_reachable() {
    for bit_depth in [1u8, 2, 4, 8] {
        let entries = 1usize << bit_depth;
        let palette = ramp_palette(entries);
        let indices: Vec<u16> = (0..entries).map(|i| i as u16).collect();
        let padded = (entries * bit_depth as usize).div_ceil(8);
        let case = build_png(&PngSpec {
            w: entries,
            h: 1,
            bit_depth,
            color_type: 3,
            samples: &indices,
            filters: &[0],
            palette: Some(&palette),
            trns: None,
        });
        let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        assert_eq!((img.w, img.h), (entries as u32, 1));
        assert!(img.is_valid());
        assert_eq!(
            pack_row(&indices, bit_depth).len(),
            padded,
            "bit depth {bit_depth}: row does not fill a whole number of bytes"
        );
        assert_eq!(
            img.data,
            indexed_rgba(&palette, &indices),
            "bit depth {bit_depth}: palette indices are not looked up verbatim"
        );
    }
}

/// A row is `ceil(w * bd / 8)` bytes, so an odd width ends part way through its
/// last byte and the row below has to start on a byte boundary of its own.
/// A bit cursor carried across rows, or a stride rounded the wrong way, slides
/// every later pixel. The three rows use different filters so the reconstruction
/// has to see the same stride the sample reading does.
#[test]
fn a_row_ending_mid_byte_does_not_carry_its_bits_into_the_next_row() {
    for (bit_depth, w) in [
        (1u8, 3usize),
        (1, 5),
        (1, 9),
        (2, 3),
        (2, 5),
        (2, 7),
        (4, 3),
        (4, 5),
        (4, 7),
    ] {
        let entries = 1usize << bit_depth;
        let palette = ramp_palette(entries);
        let indices: Vec<u16> = (0..w * 3)
            .map(|i| (i as u16 * 3) % entries as u16)
            .collect();
        let case = build_png(&PngSpec {
            w,
            h: 3,
            bit_depth,
            color_type: 3,
            samples: &indices,
            filters: &[0, 1, 2],
            palette: Some(&palette),
            trns: None,
        });
        let img = decode_png_bytes(&case.bytes).expect("well-formed PNG");
        assert_eq!((img.w, img.h), (w as u32, 3));
        assert!(img.is_valid());
        assert_eq!(
            img.data,
            indexed_rgba(&palette, &indices),
            "png {w}x3 bd={bit_depth}: packed rows drifted across a row boundary"
        );
    }
}

/// Cutting an indexed file at every offset must stay a clean `Err` or a
/// well-formed `Ok` - never a panic, never a short buffer. The sub-byte depths
/// are the interesting ones because the decoder reads a whole packed row out of
/// the inflated plane: a cut between two rows is where a missing byte could
/// otherwise be read as a row of index 0.
#[test]
fn every_truncation_of_a_packed_indexed_file_is_safe() {
    for bit_depth in [1u8, 2, 4, 8] {
        let entries = 1usize << bit_depth;
        let palette = ramp_palette(entries);
        // An odd width, so the last byte of every row is partly padding, and
        // rows that start on the opposite phase of the byte from the row above.
        let w = 5usize;
        let indices: Vec<u16> = (0..w * 3)
            .map(|i| (i as u16 * 5) % entries as u16)
            .collect();
        let case = build_png(&PngSpec {
            w,
            h: 3,
            bit_depth,
            color_type: 3,
            samples: &indices,
            filters: &[0, 0, 0],
            palette: Some(&palette),
            trns: None,
        });
        for cut in 0..=case.bytes.len() {
            match catch_panic(|| decode_png_bytes(&case.bytes[..cut])) {
                Ok(Ok(img)) => {
                    let verdict = well_formed(&img);
                    assert!(
                        verdict.is_ok(),
                        "png bd={bit_depth}: {}",
                        verdict.unwrap_err()
                    );
                }
                Ok(Err(_)) => {}
                Err(_) => panic!("png bd={bit_depth}: panicked on a {cut} byte prefix"),
            }
        }
        // A cut that reaches into the compressed plane loses the tail of the
        // deflate stream, and the pixels that did arrive must not be handed
        // back as a shorter image.
        for drop in 1..=4 {
            let cut = case.end_of_idat_data - drop;
            assert!(
                decode_png_bytes(&case.bytes[..cut]).is_err(),
                "png bd={bit_depth}: accepted a file {drop} bytes short of a whole plane"
            );
        }
    }
}
