//! `decode`'s dispatch, which is the only part of the public API the rest of
//! the property suite cannot reach: the native decoders are private, and every
//! case here therefore has to be one the native decoder *accepts*.
//!
//! That restriction is the point. `decode` falls back to `decode_external` on
//! any native failure, and that path execs `ffmpeg`, `convert` or `magick`, so
//! a case built around a decode *failure* would depend on which of those happen
//! to be installed. Feeding it files it can decode natively keeps the test
//! hermetic and deterministic: no subprocess, no network, no fixture file.
//!
//! What is under test is the dispatch rule the crate documents - the decoder
//! is chosen from the lowercased file extension - so each case writes the same
//! small image under a differently cased extension and requires the identical
//! pixels back. A case-sensitive lookup would route `.PNG` to the converters.

use maverick_img::{decode, Rgba8};
use proptest::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Two pixels, distinct in every channel, so a decoder that reads the wrong
/// bytes cannot accidentally agree.
const PIXELS: [[u8; 4]; 2] = [[0x21, 0x43, 0x65, 0x87], [0xfe, 0xdc, 0xba, 0x98]];

fn png() -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut plane = vec![0u8]; // filter: none
    for px in PIXELS {
        plane.extend_from_slice(&px);
    }
    // zlib header, then one final uncompressed DEFLATE block holding the plane.
    let mut idat = vec![0x78, 0x01, 0x01];
    idat.extend_from_slice(&(plane.len() as u16).to_le_bytes());
    idat.extend_from_slice(&(!(plane.len() as u16)).to_le_bytes());
    idat.extend_from_slice(&plane);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (typ, payload) in [
        (&b"IHDR"[..], &ihdr[..]),
        (&b"IDAT"[..], &idat[..]),
        (&b"IEND"[..], &[][..]),
    ] {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(typ);
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0u8; 4]); // CRC is not consulted by the decoder
    }
    out
}

fn ppm() -> Vec<u8> {
    let mut v = b"P6\n2 1\n255\n".to_vec();
    for px in PIXELS {
        v.extend_from_slice(&px[..3]);
    }
    v
}

fn qoi() -> Vec<u8> {
    let mut v = b"qoif".to_vec();
    v.extend_from_slice(&2u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.push(4); // channels
    v.push(0); // sRGB with linear alpha
    for px in PIXELS {
        v.push(0xfe); // QOI_OP_RGBA
        v.extend_from_slice(&px);
    }
    v.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    v
}

fn bmp() -> Vec<u8> {
    let mut out = vec![0u8; 54 + 8]; // 2 px of BGRA is already a multiple of 4
    out[0..2].copy_from_slice(b"BM");
    out[10..14].copy_from_slice(&54u32.to_le_bytes());
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&2i32.to_le_bytes());
    out[22..26].copy_from_slice(&1i32.to_le_bytes());
    out[26..28].copy_from_slice(&1u16.to_le_bytes());
    out[28..30].copy_from_slice(&32u16.to_le_bytes());
    for (i, px) in PIXELS.iter().enumerate() {
        let p = 54 + i * 4;
        out[p..p + 4].copy_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    out
}

fn farbfeld() -> Vec<u8> {
    let mut v = b"farbfeld".to_vec();
    v.extend_from_slice(&2u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    for px in PIXELS {
        for c in px {
            v.extend_from_slice(&((c as u16) << 8).to_be_bytes());
        }
    }
    v
}

/// Every natively decodable extension, with a file body the matching decoder
/// accepts, and whether the format carries an alpha channel: PPM has none, so
/// its alpha comes back opaque whatever the source pixels said. An extension
/// absent from this table is one that routes to the external converters, which
/// is exactly what this test must not exercise.
type NativeCase = (&'static [&'static str], fn() -> Vec<u8>, bool);

const CASES: [NativeCase; 5] = [
    (&["png"], png, true),
    (&["ppm", "pnm"], ppm, false),
    (&["qoi"], qoi, true),
    (&["bmp"], bmp, true),
    (&["ff", "farbfeld"], farbfeld, true),
];

fn expected(has_alpha: bool) -> Rgba8 {
    let mut data = Vec::new();
    for px in PIXELS {
        data.extend_from_slice(&[px[0], px[1], px[2], if has_alpha { px[3] } else { 255 }]);
    }
    Rgba8 { data, w: 2, h: 1 }
}

/// A scratch file removed on drop, named uniquely per case: the test binary
/// runs its cases in parallel, so a shared name would have them overwriting
/// each other.
struct Scratch(PathBuf);

impl Scratch {
    fn put(ext: &str, bytes: &[u8]) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "maverick-img-dispatch-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        p.set_extension(ext);
        std::fs::write(&p, bytes).expect("scratch write");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Whichever native extension the file carries, in whichever case it is
    /// written, `decode` must return that format's pixels. Without the
    /// lowercasing, an upper-case wallpaper name is a decode failure at
    /// startup rather than a background.
    #[test]
    fn decode_selects_the_native_decoder_named_by_the_extension(
        case in 0usize..CASES.len(),
        alias in 0usize..4,
        casing in 0usize..3,
    ) {
        let (exts, build, has_alpha) = CASES[case];
        let ext = exts[alias % exts.len()];
        let ext = match casing {
            0 => ext.to_ascii_uppercase(),
            1 => {
                let mut c = ext.to_ascii_uppercase().into_bytes();
                c[0] = ext.as_bytes()[0].to_ascii_lowercase();
                String::from_utf8(c).unwrap()
            }
            _ => ext.to_string(),
        };
        let file = Scratch::put(&ext, &build());
        let img = decode(&file.0).unwrap_or_else(|e| panic!(".{} failed to decode: {e}", ext));
        prop_assert!(img.is_valid(), ".{}: buffer is not w*h*4", ext);
        prop_assert_eq!(img, expected(has_alpha), ".{} decoded to the wrong image", ext);
    }
}

/// The same dispatch rule with no random input at all, kept as an explicit
/// list so the covered extensions are readable without running a generator.
#[test]
fn every_documented_extension_reaches_its_native_decoder() {
    for (exts, build, has_alpha) in CASES {
        for ext in exts {
            let file = Scratch::put(ext, &build());
            assert_eq!(decode(&file.0).expect(ext), expected(has_alpha), ".{ext}");
        }
    }
}
