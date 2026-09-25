// Properties of `maverick-gl`'s public surface, checked the way a downstream
// crate sees it: the extension-token matcher and the capability classifier are
// the two pieces of pure decision logic a window manager calls before it
// decides whether to composite at all.
//
// Nothing here opens a display, loads a driver, or needs one — which is the
// point: the window manager must be able to ask "is there a GL driver?" and
// "does this server advertise the extension I need?" on a machine that has
// neither, and get an answer it can act on.

use maverick_gl::glx::has_extension;
use maverick_gl::renderer::classify_acceleration;
use maverick_gl::{probe, Acceleration, Filter, RendererBackend, RendererInfo, VisualFormat};
use proptest::prelude::*;

/// The markers the classifier keys off, per its own contract: purely
/// string-based, no guessing about GPU vendor names.
const SOFTWARE_MARKERS: [&str; 6] = [
    "llvmpipe",
    "softpipe",
    "swrast",
    "software renderer",
    "software rasterizer",
    "swiftshader",
];

/// A run of whitespace. A server separates its extension names with plain
/// spaces, but nothing in the protocol promises only spaces or only one of
/// them, and a matcher that splits on `' '` alone then finds a different set
/// of extensions from the one it is meant to be reading.
fn whitespace() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(" ".to_string()),
        Just("  ".to_string()),
        Just("\t".to_string()),
        Just("\n".to_string()),
        Just("\r\n".to_string()),
        Just(" \t \n ".to_string()),
    ]
}

/// A single extension name: never empty, never containing whitespace, which is
/// what makes it one token rather than two.
fn extension_name() -> impl Strategy<Value = String> {
    prop::collection::vec(
        any::<char>().prop_filter("a name is not whitespace", |c| !c.is_whitespace()),
        1..12,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

/// A name to look for, together with a list of names in which it appears only
/// as part of something longer: the extra names are filtered so that none of
/// them is the needle itself, and may well contain it.
fn arb_needing_a_longer_name(
) -> impl Strategy<Value = (String, String, String, String, Vec<String>)> {
    extension_name().prop_flat_map(|needle| {
        let not_the_needle = extension_name().prop_filter("a longer name", {
            let wanted = needle.clone();
            move |name| *name != wanted
        });
        (
            Just(needle),
            extension_name(),
            extension_name(),
            whitespace(),
            prop::collection::vec(not_the_needle, 0..3),
        )
    })
}

proptest! {
    /// Which extensions a string contains depends on its names, not on how it
    /// was spaced.
    ///
    /// Every optional path in the GL backend is gated on this: a matcher that
    /// only split on a single space finds nothing in a list a server printed
    /// with a double space or a tab, and the compositor quietly turns off
    /// vsync, buffer ageing and every optional entry point at once — with
    /// nothing in the log to say why. An empty name is not a name, so it never
    /// matches anything.
    #[test]
    fn extension_matching_reads_names_not_whitespace(
        tokens in prop::collection::vec(extension_name(), 0..6),
        sep in whitespace(),
        lead in prop::option::of(whitespace()),
        trail in prop::option::of(whitespace()),
        needle in prop::option::of(extension_name()),
    ) {
        let mut haystack = String::new();
        haystack.push_str(lead.as_deref().unwrap_or(""));
        haystack.push_str(&tokens.join(&sep));
        haystack.push_str(trail.as_deref().unwrap_or(""));
        let needle = needle.unwrap_or_default();
        prop_assert_eq!(
            has_extension(&haystack, &needle),
            tokens.contains(&needle),
            "{:?} against {:?}", haystack, needle
        );
    }

    /// A name that is only *part* of one the list does carry is not found.
    ///
    /// This is the whole reason the matcher compares whole tokens. The
    /// extensions that matter here share prefixes by design:
    /// `GLX_EXT_swap_control_tear` also contains `GLX_EXT_swap_control`, and
    /// taking adaptive-vsync support from a server that only offers the tear
    /// extension gets an interval of -1 the server never agreed to, so the
    /// frame is left waiting on a vblank that does not happen.
    #[test]
    fn an_extension_that_is_only_part_of_a_name_is_not_found(
        (needle, prefix, suffix, sep, extra) in arb_needing_a_longer_name()
    ) {
        // Every name in the list contains the needle strictly, and none of them
        // is the needle: the answer has to be "not found" throughout.
        let mut names = vec![
            format!("{prefix}{needle}"),
            format!("{needle}{suffix}"),
            format!("{prefix}{needle}{suffix}"),
        ];
        names.extend(extra);
        let haystack = names.join(&sep);
        prop_assert!(
            !has_extension(&haystack, &needle),
            "{:?} found inside a longer name in {:?}", needle, haystack
        );
        // Spreading the same characters over two names changes nothing
        // either: only a whole name counts, never a run of one that happens to
        // be adjacent to another.
        let chars: Vec<char> = needle.chars().collect();
        if chars.len() > 1 {
            let (head, rest) = chars.split_at(chars.len() / 2);
            let split = [head, rest]
                .iter()
                .map(|part| part.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join(&sep);
            prop_assert!(
                !has_extension(&split, &needle),
                "{:?} matched across two names", needle
            );
        }
    }

    /// A software renderer is recognised wherever either field names it.
    ///
    /// The classification runs on the two strings joined, because drivers split
    /// the answer any way they like: `GL_RENDERER` says "llvmpipe" with
    /// `GL_VENDOR` blank on Mesa, and says "Mesa" with `GL_RENDERER` carrying
    /// "softpipe" on other builds. Missing either spelling is what makes a
    /// machine with no GPU composite through a software rasterizer, at one
    /// frame of CPU per redraw.
    #[test]
    fn a_software_renderer_is_recognised_whichever_field_names_it(
        marker in prop::sample::select(&SOFTWARE_MARKERS[..]),
        before in any::<String>(),
        after in any::<String>(),
        in_vendor in any::<bool>(),
    ) {
        let named = format!("{before}{marker}{after}");
        let (vendor, renderer) = if in_vendor {
            (named, before.clone())
        } else {
            (before.clone(), named)
        };
        prop_assert_eq!(
            classify_acceleration(&vendor, &renderer),
            Acceleration::Software,
            "{:?} / {:?} did not read as software", vendor, renderer
        );
    }

    /// The classification does not depend on how the driver cased its strings.
    ///
    /// The comparison is made on a lowercased copy precisely so that
    /// `SWIFTSHADER` and `swiftshader` cannot be told apart, and the answer has
    /// to survive any mixture of case in either field.
    #[test]
    fn acceleration_classification_ignores_ascii_case(
        vendor in any::<String>(),
        renderer in any::<String>(),
    ) {
        prop_assert_eq!(
            classify_acceleration(&vendor, &renderer),
            classify_acceleration(
                &vendor.to_ascii_lowercase(),
                &renderer.to_ascii_lowercase()
            )
        );
    }

    /// Acceleration is `Unknown` exactly when the driver told us nothing at
    /// all — which is what the compositor prints to explain a lack of
    /// information, so reporting it alongside a vendor string it already has
    /// would be self-contradictory, and hiding it behind an empty vendor with
    /// a real renderer would throw away a real answer.
    #[test]
    fn an_unknown_acceleration_needs_both_fields_empty(
        vendor in any::<String>(),
        renderer in any::<String>(),
    ) {
        let got = classify_acceleration(&vendor, &renderer);
        prop_assert_eq!(
            got == Acceleration::Unknown,
            vendor.is_empty() && renderer.is_empty(),
            "{:?} / {:?} read as {:?}", vendor, renderer, got
        );
    }

    /// The startup report is one line per field, each on the line that names
    /// it.
    ///
    /// It is what a user is shown when compositing is off, and it is read
    /// top-down against the window manager's own log around it, so a field
    /// printed on the wrong line or missing altogether turns a diagnosable
    /// driver problem into a mystery.
    #[test]
    fn the_startup_report_has_one_line_per_field(
        vendor in "[a-zA-Z0-9 ._()-]{0,40}",
        renderer in "[a-zA-Z0-9 ._()-]{0,40}",
        version in "[a-zA-Z0-9 ._()-]{0,40}",
        accelerated in prop::sample::select(
            &[Acceleration::Gpu, Acceleration::Software, Acceleration::Unknown][..]
        ),
    ) {
        let info = RendererInfo {
            backend: RendererBackend::OpenGlGlx,
            vendor: vendor.to_owned(),
            renderer: renderer.to_owned(),
            version: version.to_owned(),
            accelerated,
        };
        let report = info.to_string();
        let lines: Vec<&str> = report.lines().collect();
        prop_assert_eq!(lines.len(), 6, "{:?}", report);
        prop_assert_eq!(lines[0], "Compositor:");
        for (line, label, value) in [
            (lines[1], "Backend", RendererBackend::OpenGlGlx.to_string()),
            (lines[2], "Vendor", vendor.to_owned()),
            (lines[3], "Renderer", renderer.to_owned()),
            (lines[4], "Version", version.to_owned()),
            (lines[5], "Acceleration", accelerated.to_string()),
        ] {
            prop_assert_eq!(line, format!("  {}: {}", label, value), "{:?}", report);
        }
        prop_assert!(report.ends_with('\n'), "the report is a log line, not a file");
    }

    /// A visual says it carries colour only when it does.
    ///
    /// `has_alpha` is what tells the fbconfig decision to ask for
    /// `GLX_TEXTURE_FORMAT_RGBA_EXT` and to require an alpha-capable config,
    /// so a visual that claimed alpha it does not have would be bound through
    /// a config chosen for a different channel layout.
    #[test]
    fn a_visual_only_claims_colour_it_carries(
        r in any::<u8>(),
        g in any::<u8>(),
        b in any::<u8>(),
        a in any::<u8>(),
    ) {
        let v = VisualFormat {
            id: 0x21,
            depth: r.saturating_add(g).saturating_add(b).saturating_add(a),
            red_bits: r,
            green_bits: g,
            blue_bits: b,
            alpha_bits: a,
            direct: true,
        };
        prop_assert_eq!(v.has_alpha(), a > 0);
        prop_assert_eq!(v.color_bits(), u32::from(r) + u32::from(g) + u32::from(b));
    }
}

/// The extension string is whatever a driver printed, so the matcher has to
/// answer for anything at all rather than panic on it.
///
/// The generated cases above cover the interesting shapes; these are the ones
/// a hand-written matcher is most likely to mishandle — an empty name, a name
/// that is only whitespace, and a string that is nothing but separators.
#[test]
fn extension_matching_answers_odd_driver_strings() {
    for (haystack, needle, expected) in [
        ("", "", false),
        ("", "GLX_EXT_buffer_age", false),
        ("   \t\n  ", "", false),
        ("GLX_EXT_buffer_age", "", false),
        // A NUL is part of a name, not a separator, so this is a different
        // extension than the one being looked for.
        ("GLX_EXT_swap_control\0_ext", "GLX_EXT_swap_control", false),
        // A four-byte character is not a separator, so this name is one
        // token; in the next line the same character *is* a name of its own.
        ("\u{1f600}_GLX_EXT_buffer_age", "GLX_EXT_buffer_age", false),
        ("\u{1f600} GLX_EXT_buffer_age", "GLX_EXT_buffer_age", true),
        // A no-break space is whitespace, so it separates names like any
        // other space does.
        ("\u{a0}GLX_EXT_buffer_age\u{a0}", "GLX_EXT_buffer_age", true),
    ] {
        assert_eq!(
            has_extension(haystack, needle),
            expected,
            "{haystack:?} against {needle:?}"
        );
    }
}

/// Each filter mode asks the driver for a different sampler.
///
/// `draw` compares the cached filter against the one a quad asks for and only
/// then re-issues `glTexParameteri`. Two modes collapsing onto one GL constant
/// would leave the driver sampling a window that an animation is scaling with
/// `GL_NEAREST`, with nothing in the frame to show it.
#[test]
fn filter_modes_map_to_distinct_gl_samplers() {
    assert_eq!(Filter::Nearest.to_gl(), maverick_gl::gl::GL_NEAREST);
    assert_eq!(Filter::Linear.to_gl(), maverick_gl::gl::GL_LINEAR);
    assert_ne!(Filter::Nearest.to_gl(), Filter::Linear.to_gl());
}

/// Capability detection is answerable and does not change its mind.
///
/// The window manager asks before it claims `_NET_WM_CM_S0` and before it
/// redirects anything, possibly from more than one place, and libGL is loaded
/// once and never unloaded — so a second answer that disagreed with the first
/// would mean the decision to composite was made on a different answer than
/// the one the compositor is built on. A machine with no driver must still get
/// an answer rather than an error.
#[test]
fn capability_probe_answers_the_same_every_time() {
    let first = probe();
    for _ in 0..4 {
        assert_eq!(probe(), first, "the driver came or went under us");
    }
}
