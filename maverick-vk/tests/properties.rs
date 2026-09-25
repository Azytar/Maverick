//! Property-based coverage of the backend's pure, deterministic logic: the four
//! swapchain parameter helpers, the error type and the device report.
//!
//! Every input is a `vk` struct built in memory, so this file needs no Vulkan
//! loader, no driver, no X connection and no display — it runs wherever
//! `tests/unit.rs` runs. What it adds over hand-picked cases is the *shape* of
//! the input space: zero, one and `u32::MAX`, degenerate and inverted bounds,
//! empty and duplicated driver lists, and the whole `i32` range behind
//! `vk::Result`.

use ash::vk;
use maverick_vk::{
    choose_image_count, choose_present_mode, choose_surface_format, clamp_extent, DeviceReport,
    VkError,
};
use proptest::prelude::*;

/// The format the backend asks for, and its colour space. Spelled out instead
/// of imported because the constants are crate-private: the property has to pin
/// the contract the public docs state, not the constant behind it.
const PREFERRED: vk::Format = vk::Format::B8G8R8A8_SRGB;
const PREFERRED_SPACE: vk::ColorSpaceKHR = vk::ColorSpaceKHR::SRGB_NONLINEAR;

/// Formats a surface can list for an X11 window. `UNDEFINED` is the sentinel
/// that hands the choice back to us; the two sRGB entries and the two UNORM
/// entries let a one-element list stand for "the preferred pair", "the
/// preferred format in a colour space we do not prefer" and "nothing we
/// prefer" without the test having to say which.
static FORMATS: [vk::Format; 5] = [
    vk::Format::UNDEFINED,
    vk::Format::B8G8R8A8_SRGB,
    vk::Format::B8G8R8A8_UNORM,
    vk::Format::R8G8B8A8_SRGB,
    vk::Format::R8G8B8A8_UNORM,
];

/// `SRGB_NONLINEAR` is the only core colour space for a KHR surface; the P3
/// one is what a driver that enabled `VK_EXT_swapchain_colorspace` adds, and it
/// is what keeps "preferred format, colour space we do not prefer" reachable.
static COLOR_SPACES: [vk::ColorSpaceKHR; 2] = [
    vk::ColorSpaceKHR::SRGB_NONLINEAR,
    vk::ColorSpaceKHR::DISPLAY_P3_NONLINEAR_EXT,
];

/// The four present modes `VK_KHR_swapchain` defines. `MAILBOX` and `FIFO` are
/// the two the backend is allowed to name; the other two are what it has to
/// survive being offered instead.
static PRESENT_MODES: [vk::PresentModeKHR; 4] = [
    vk::PresentModeKHR::IMMEDIATE,
    vk::PresentModeKHR::MAILBOX,
    vk::PresentModeKHR::FIFO,
    vk::PresentModeKHR::FIFO_RELAXED,
];

/// Extents worth generating: both ends of the range, a few window sizes, and
/// `u32::MAX`, the sentinel that means "the surface, not us, owns this size".
static WIDTHS: [u32; 6] = [0, 1, 320, 4095, 8192, u32::MAX];
static HEIGHTS: [u32; 5] = [0, 1, 240, 4096, 8192];

/// Words `DeviceReport` renders a device type as. Anything else in the report
/// would mean the startup log no longer parses by eye.
static TYPE_WORDS: [&str; 5] = ["discrete", "integrated", "virtual", "cpu", "other"];

/// A `(format, colour space)` pair as a driver would list it.
fn surface_format() -> impl Strategy<Value = vk::SurfaceFormatKHR> {
    (
        prop::sample::select(&FORMATS[..]),
        prop::sample::select(&COLOR_SPACES[..]),
    )
        .prop_map(|(format, color_space)| vk::SurfaceFormatKHR {
            format,
            color_space,
        })
}

/// A driver's format list, including the empty one: the surface reporting
/// nothing is exactly the case the helper has to survive.
fn surface_format_list() -> impl Strategy<Value = Vec<vk::SurfaceFormatKHR>> {
    prop::collection::vec(surface_format(), 0..6)
}

/// A driver's present-mode list. Duplicates are left in, since a driver that
/// lists the same mode twice is a case a membership test still has to answer.
fn present_mode_list() -> impl Strategy<Value = Vec<vk::PresentModeKHR>> {
    prop::collection::vec(prop::sample::select(&PRESENT_MODES[..]), 0..6)
}

/// One axis of a surface bound: plausible geometry and the full `u32` range, so
/// that a driver reporting `u32::MAX` as its minimum is as reachable as one
/// reporting 256.
fn extent_bound() -> impl Strategy<Value = u32> {
    prop_oneof![
        prop::sample::select(&[0u32, 1, 16, 64, 256, 4096]),
        any::<u32>()
    ]
}

/// An image count. The full `u32` range is included rather than the handful a
/// driver actually reports: the arithmetic downstream is where a saturating
/// value bites, and a strategy that only offered 2 or 3 could not reach it.
fn count_bound() -> impl Strategy<Value = u32> {
    prop_oneof![prop::sample::select(&[0u32, 1, 2, 3, 8]), any::<u32>()]
}

/// A driver's `(minImageExtent, maxImageExtent)` pair, ordered by
/// construction. Vulkan requires each minimum to be at most its maximum, so an
/// inverted window is a driver that broke the spec rather than an input the
/// helpers are contracted to answer; what the ordering keeps reachable is the
/// degenerate window (`min == max`), the unbounded one (`max == u32::MAX`) and
/// the zero minimum real drivers report.
fn extent_window() -> impl Strategy<Value = (vk::Extent2D, vk::Extent2D)> {
    (extent_bound(), extent_bound()).prop_map(|(a, b)| {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        (
            vk::Extent2D {
                width: lo,
                height: lo,
            },
            vk::Extent2D {
                width: hi,
                height: hi,
            },
        )
    })
}

/// A driver's `(minImageCount, maxImageCount)` pair. `max` is `0` whenever the
/// driver stated no upper bound, which is the case the cap arithmetic has to
/// recognise; otherwise it is a real cap, raised to the minimum when it is
/// below it so the pair is one a driver can report.
fn count_window() -> impl Strategy<Value = (u32, u32)> {
    (
        count_bound(),
        prop::sample::select(&[None, Some(0u32), Some(1), Some(2), Some(3), Some(4)]),
    )
        .prop_map(|(min_count, cap)| {
            let max_count = match cap {
                None => 0,
                Some(c) => c.max(min_count),
            };
            (min_count, max_count)
        })
}

/// Assemble capabilities from their parts, leaving the fields the properties
/// under test do not read at their defaults.
fn caps(
    min_image_extent: vk::Extent2D,
    max_image_extent: vk::Extent2D,
    min_image_count: u32,
    max_image_count: u32,
    current_extent: vk::Extent2D,
) -> vk::SurfaceCapabilitiesKHR {
    vk::SurfaceCapabilitiesKHR {
        min_image_extent,
        max_image_extent,
        min_image_count,
        max_image_count,
        current_extent,
        ..Default::default()
    }
}

/// Capabilities with no constraint tying the fields together: the bounds, the
/// counts and the current extent are drawn independently, inverted windows and
/// all. Only for the properties whose contract is stated without regard to the
/// rest of the capabilities.
fn surface_caps() -> impl Strategy<Value = vk::SurfaceCapabilitiesKHR> {
    (
        extent_bound(),
        extent_bound(),
        extent_bound(),
        extent_bound(),
        count_bound(),
        count_bound(),
        prop::sample::select(&WIDTHS[..]),
        prop::sample::select(&HEIGHTS[..]),
    )
        .prop_map(
            |(min_w, min_h, max_w, max_h, min_count, max_count, cur_w, cur_h)| {
                caps(
                    vk::Extent2D {
                        width: min_w,
                        height: min_h,
                    },
                    vk::Extent2D {
                        width: max_w,
                        height: max_h,
                    },
                    min_count,
                    max_count,
                    vk::Extent2D {
                        width: cur_w,
                        height: cur_h,
                    },
                )
            },
        )
}

/// Capabilities for the branch where the surface leaves the size to us: the
/// `u32::MAX` sentinel on both axes is the documented signal for it, so it is
/// set here rather than left to a filter, and the size windows are the ones a
/// driver can actually report.
fn self_sized_caps() -> impl Strategy<Value = vk::SurfaceCapabilitiesKHR> {
    (extent_window(), count_window()).prop_map(
        |((min_extent, max_extent), (min_count, max_count))| {
            caps(
                min_extent,
                max_extent,
                min_count,
                max_count,
                vk::Extent2D {
                    width: u32::MAX,
                    height: u32::MAX,
                },
            )
        },
    )
}

/// Capabilities for the branch where the surface leaves the size to us, paired
/// with a requested size.
///
/// Half the time the request is an unconstrained value; the other half it sits
/// on one of the surface's own bounds or one step either side of it. A request
/// drawn independently of the bounds essentially never lands on one, and a
/// boundary is the only place a clamp and a range check disagree — which is why
/// a generator that misses it cannot tell the two apart.
fn self_sized_caps_and_request() -> impl Strategy<Value = (vk::SurfaceCapabilitiesKHR, u32, u32)> {
    (
        extent_window(),
        count_window(),
        any::<u32>(),
        0u8..=2,
        -1i8..=1,
    )
        .prop_map(
            |((min_extent, max_extent), (min_count, max_count), free, bound, delta)| {
                let request = |axis_min: u32, axis_max: u32| match bound {
                    0 => free,
                    1 => step_off(axis_min, delta),
                    _ => step_off(axis_max, delta),
                };
                let caps = caps(
                    min_extent,
                    max_extent,
                    min_count,
                    max_count,
                    vk::Extent2D {
                        width: u32::MAX,
                        height: u32::MAX,
                    },
                );
                (
                    caps,
                    request(min_extent.width, max_extent.width),
                    request(min_extent.height, max_extent.height),
                )
            },
        )
}

/// `bound` moved by `delta`, kept inside the `u32` range. Both edges are
/// reachable: a surface whose maximum is `u32::MAX` has no "one past it".
fn step_off(bound: u32, delta: i8) -> u32 {
    (bound as i64 + delta as i64).clamp(0, u32::MAX as i64) as u32
}

/// Capabilities for the image-count contract, where the extent fields are left
/// unconstrained: nothing in the count arithmetic reads them, and letting them
/// vary keeps the property from accidentally depending on them.
fn image_count_caps() -> impl Strategy<Value = vk::SurfaceCapabilitiesKHR> {
    (
        extent_bound(),
        extent_bound(),
        extent_bound(),
        extent_bound(),
        count_window(),
    )
        .prop_map(|(min_w, min_h, max_w, max_h, (min_count, max_count))| {
            caps(
                vk::Extent2D {
                    width: min_w,
                    height: min_h,
                },
                vk::Extent2D {
                    width: max_w,
                    height: max_h,
                },
                min_count,
                max_count,
                vk::Extent2D {
                    width: 800,
                    height: 600,
                },
            )
        })
}

proptest! {
    /// A surface that lists at least one format can always back a swapchain,
    /// so the selection may only come up empty for a surface that lists
    /// nothing. Refusing a working driver is a worse outcome than picking an
    /// unusual format, and this is the property that keeps the two apart.
    #[test]
    fn surface_format_is_unavailable_only_when_the_surface_lists_nothing(
        formats in surface_format_list()
    ) {
        let chosen = choose_surface_format(&formats);
        prop_assert_eq!(
            chosen.is_none(),
            formats.is_empty(),
            "a surface listing {} format(s) was treated as unusable: {:?}",
            formats.len(),
            chosen
        );
    }

    /// The preference order the docs spell out — lone `UNDEFINED` means "you
    /// choose", otherwise the exact sRGB pair, otherwise that format in any
    /// colour space, otherwise whatever the driver listed first — is a total
    /// decision: for every list the answer is one of those four outcomes, never
    /// a fifth and never an absence.
    #[test]
    fn surface_format_follows_the_documented_preference_order(
        formats in surface_format_list()
    ) {
        let exact = vk::SurfaceFormatKHR {
            format: PREFERRED,
            color_space: PREFERRED_SPACE,
        };
        let lone_undefined =
            formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED;
        let chosen = choose_surface_format(&formats);

        if lone_undefined {
            prop_assert_eq!(chosen, Some(exact), "the surface offered us a free choice");
        } else if formats.contains(&exact) {
            prop_assert_eq!(
                chosen,
                Some(exact),
                "the preferred pair was on offer and something else was picked"
            );
        } else if formats.iter().any(|f| f.format == PREFERRED) {
            let chosen = chosen.expect("a listed format is always selectable");
            prop_assert_eq!(chosen.format, PREFERRED, "the preferred format was on offer");
            prop_assert!(
                formats.contains(&chosen),
                "picked {:?}, which the surface never listed",
                chosen
            );
        } else {
            prop_assert_eq!(
                chosen,
                formats.first().copied(),
                "with nothing preferred on offer the driver's first entry is the fallback"
            );
        }
    }

    /// `MAILBOX` is the low-latency choice and `FIFO` is mandatory on every
    /// driver, so those are the only two the backend may ever ask for —
    /// whatever the driver reported, including a list with neither in it and a
    /// list that is empty.
    #[test]
    fn present_mode_never_names_a_mode_outside_mailbox_and_fifo(
        modes in present_mode_list()
    ) {
        let chosen = choose_present_mode(&modes);
        prop_assert!(
            chosen == vk::PresentModeKHR::MAILBOX || chosen == vk::PresentModeKHR::FIFO,
            "asked for {:?}, which is neither the preferred nor the guaranteed mode (offered: {:?})",
            chosen,
            modes
        );
    }

    /// The preference itself, over every list: `MAILBOX` exactly when the
    /// driver offers it, the mandatory `FIFO` otherwise. An unsupported mode
    /// falling back to the documented default is the whole point of the
    /// helper, so this is the property that fails if the two branches are ever
    /// swapped or merged.
    #[test]
    fn present_mode_prefers_mailbox_exactly_when_the_driver_offers_it(
        modes in present_mode_list()
    ) {
        let expected = if modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else {
            vk::PresentModeKHR::FIFO
        };
        prop_assert_eq!(choose_present_mode(&modes), expected, "offered: {:?}", modes);
    }

    /// "A `current_extent` other than `u32::MAX` means the window manager (not
    /// us) owns the size, so it wins and the request is ignored." Nothing
    /// else in the capabilities may interfere: a surface-owned size is returned
    /// untouched whatever was asked for and whatever bounds were advertised.
    #[test]
    fn surface_owned_extent_overrides_any_request(
        caps in surface_caps(),
        w in prop::sample::select(&WIDTHS[..]),
        h in prop::sample::select(&HEIGHTS[..]),
    ) {
        prop_assume!(caps.current_extent.width != u32::MAX);
        prop_assert_eq!(
            clamp_extent(&caps, w, h),
            caps.current_extent,
            "the surface owns the size, so a {}x{} request must not be honoured",
            w,
            h
        );
    }

    /// When the surface leaves the size to us the answer has to land inside
    /// the advertised window, whatever was requested — 0, 1, `u32::MAX`, or a
    /// size on the far side of the bounds. A zero result is only legitimate
    /// when the surface's own minimum is zero, which is the `(0, 0)` real
    /// drivers report; otherwise a non-zero request may not be silently turned
    /// into a swapchain with no pixels in it.
    #[test]
    fn extent_is_clamped_to_the_surface_limits((caps, w, h) in self_sized_caps_and_request()) {
        let got = clamp_extent(&caps, w, h);

        prop_assert!(
            got.width >= caps.min_image_extent.width
                && got.width <= caps.max_image_extent.width,
            "width {} for a {w}x{h} request is outside [{}, {}]",
            got.width,
            caps.min_image_extent.width,
            caps.max_image_extent.width
        );
        prop_assert!(
            got.height >= caps.min_image_extent.height
                && got.height <= caps.max_image_extent.height,
            "height {} for a {w}x{h} request is outside [{}, {}]",
            got.height,
            caps.min_image_extent.height,
            caps.max_image_extent.height
        );
        if w != 0 && caps.min_image_extent.width > 0 {
            prop_assert!(
                got.width > 0,
                "a {w}-wide request became a zero-width swapchain, whose minimum is {}",
                caps.min_image_extent.width
            );
        }
        if h != 0 && caps.min_image_extent.height > 0 {
            prop_assert!(
                got.height > 0,
                "a {h}-tall request became a zero-height swapchain, whose minimum is {}",
                caps.min_image_extent.height
            );
        }
    }

    /// Clamping is monotone: a larger request may never yield a smaller image.
    /// An implementation that answered the maximum with the minimum would still
    /// land inside the advertised bounds, so this is the property that
    /// separates a real clamp from a range check.
    #[test]
    fn extent_clamp_is_monotone_in_the_request((caps, w, h) in self_sized_caps_and_request()) {
        let small = clamp_extent(&caps, w, h);
        let large = clamp_extent(&caps, w.saturating_add(1), h.saturating_add(1));
        prop_assert!(
            large.width >= small.width,
            "growing the request from {w} to {} shrank the width from {} to {}",
            w.saturating_add(1),
            small.width,
            large.width
        );
        prop_assert!(
            large.height >= small.height,
            "growing the request from {h} to {} shrank the height from {} to {}",
            h.saturating_add(1),
            small.height,
            large.height
        );
    }

    /// The image count is `min + 1` — a frame can be recorded while another is
    /// presented — capped at `max`, with `0` meaning the driver stated no upper
    /// bound. It is the only thing between a resize storm and a `create_swapchain`
    /// refusal, so it may never leave the advertised window and may never be
    /// zero: a swapchain with no images can never present a frame.
    #[test]
    fn image_count_stays_inside_what_the_driver_allows(caps in image_count_caps()) {
        // `min + 1` cannot be evaluated on the saturating minimum.
        prop_assume!(caps.min_image_count < u32::MAX);
        let count = choose_image_count(&caps);

        prop_assert!(count >= 1, "a swapchain of {count} images can never present");
        prop_assert!(
            count >= caps.min_image_count,
            "asked for {count} images, fewer than the minimum {}",
            caps.min_image_count
        );
        if caps.max_image_count == 0 {
            prop_assert_eq!(count, caps.min_image_count + 1, "no upper bound was stated");
        } else {
            prop_assert!(
                count <= caps.max_image_count,
                "asked for {count} images, more than the maximum {}",
                caps.max_image_count
            );
            prop_assert!(
                count == caps.min_image_count + 1 || count == caps.max_image_count,
                "{count} is neither one past the minimum ({}) nor the maximum ({})",
                caps.min_image_count,
                caps.max_image_count
            );
        }
    }

    /// The four helpers are documented as touching no Vulkan handle, so the
    /// same capabilities and the same request must always produce the same
    /// answer: no driver query, no cache keyed on a previous call, no state
    /// carried over from the frame before.
    #[test]
    fn selection_helpers_are_pure_functions_of_their_input(
        caps in self_sized_caps(),
        w in any::<u32>(),
        h in any::<u32>(),
        modes in present_mode_list(),
        formats in surface_format_list(),
    ) {
        prop_assume!(caps.min_image_count < u32::MAX);
        prop_assert_eq!(clamp_extent(&caps, w, h), clamp_extent(&caps, w, h));
        prop_assert_eq!(choose_image_count(&caps), choose_image_count(&caps));
        prop_assert_eq!(choose_present_mode(&modes), choose_present_mode(&modes));
        prop_assert_eq!(
            choose_surface_format(&formats),
            choose_surface_format(&formats)
        );
    }

    /// The blanket `From<vk::Result>` has to accept every status a loader can
    /// return — successes, the documented errors, and codes this build of
    /// `ash` has never heard of — and it has to keep the status in the
    /// message, because that message is the only record left of what the
    /// driver said once the error has been logged.
    #[test]
    fn every_result_code_maps_to_an_error_that_names_it(raw in any::<i32>()) {
        let result = vk::Result::from_raw(raw);
        let err: VkError = result.into();
        prop_assert!(
            matches!(err, VkError::Unsupported(_)),
            "{:?} did not reach the blanket From impl's variant: {err}",
            result
        );
        let rendered = err.to_string();
        prop_assert_eq!(VkError::from(result), err, "the mapping must be deterministic");
        prop_assert!(
            rendered.contains(&format!("{result:?}")),
            "the status was lost on the way into the message: {rendered}"
        );
    }

    /// "Every fallible step of the Vulkan bootstrap maps to one of these
    /// variants, so a caller learns which step failed": two variants may never
    /// render the same message, whatever the status text carries. The status
    /// text itself is kept verbatim, line breaks and NUL bytes included — a
    /// driver message is not ours to reformat.
    #[test]
    fn each_error_variant_is_identifiable_from_its_message(
        status in status_text()
    ) {
        let variants = [
            VkError::Loader(status.clone()),
            VkError::Instance(status.clone()),
            VkError::Surface(status.clone()),
            VkError::NoPhysicalDevice,
            VkError::Device(status.clone()),
            VkError::Swapchain(status.clone()),
            VkError::Acquire(status.clone()),
            VkError::Present(status.clone()),
            VkError::Unsupported(status.clone()),
            VkError::Incompatible(status.clone()),
        ];
        let messages: Vec<String> = variants.iter().map(ToString::to_string).collect();

        for (variant, message) in variants.iter().zip(&messages) {
            let carries_status = !matches!(variant, VkError::NoPhysicalDevice);
            if carries_status {
                prop_assert!(
                    message.contains(&status),
                    "{variant} did not carry its status verbatim: {message:?}"
                );
            }
            let twins = messages.iter().filter(|other| *other == message).count();
            prop_assert_eq!(
                twins,
                1,
                "{:?} and another variant render the same message: {:?}",
                variant,
                message
            );
        }
        // The one variant that carries no status still has to say something a
        // reader can act on; that it differs from every other variant is the
        // loop's business, not a second assertion here.
        let bare = variants
            .iter()
            .find(|variant| matches!(variant, VkError::NoPhysicalDevice))
            .expect("the variant list names it")
            .to_string();
        prop_assert!(!bare.trim().is_empty(), "NoPhysicalDevice renders nothing");
    }

    /// The report exists so a startup log identifies the GPU it booted on, so
    /// every field it was built from has to survive into the rendered text:
    /// the name as the driver spelled it, the two PCI ids and the driver
    /// version in their fixed hex widths, and the type as one of the five words
    /// the report documents. An unknown type is "other", never a number.
    #[test]
    fn device_report_shows_every_field_it_was_built_from(
        name in device_name(),
        vendor in any::<u32>(),
        device in any::<u32>(),
        driver in any::<u32>(),
        device_type in physical_device_type(),
    ) {
        let report = DeviceReport {
            name: name.clone(),
            vendor_id: vendor,
            device_id: device,
            device_type,
            driver_version: driver,
        };
        let text = report.to_string();

        prop_assert!(text.contains(&format!("  Name: {name}")), "lost the name: {text}");
        prop_assert!(
            text.contains(&format!("  Vendor: 0x{vendor:04x}")),
            "lost the vendor id: {text}"
        );
        prop_assert!(
            text.contains(&format!("  Device: 0x{device:04x}")),
            "lost the device id: {text}"
        );
        prop_assert!(
            text.contains(&format!("  Driver: 0x{driver:08x}")),
            "lost the driver version: {text}"
        );
        let type_line = text
            .lines()
            .find(|line| line.trim_start().starts_with("Type:"))
            .expect("the report names a device type");
        let word = type_line
            .trim_start()
            .trim_start_matches("Type:")
            .trim();
        prop_assert!(
            TYPE_WORDS.contains(&word),
            "{:?} was rendered as {word:?}, which is not one of {:?}",
            device_type,
            TYPE_WORDS
        );
        prop_assert_eq!(text, report.to_string(), "the report renders deterministically");
    }
}

/// Status text as a driver hands it over: arbitrary Unicode plus the line
/// breaks, NUL bytes and separators a character generator rarely produces on
/// its own. None of it may be dropped, escaped or reformatted on the way into
/// an error message.
fn status_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![any::<char>(), Just('\n'), Just('\r'), Just('\0'), Just(':'),],
        0..16,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// A device name as `VkPhysicalDeviceProperties` delivers it: printable text
/// with the odd non-ASCII character a vendor string is free to contain. Line
/// breaks are excluded on purpose — one embedded in a name would make the
/// report's own lines indistinguishable from it.
fn device_name() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(&[
            'a',
            'R',
            'e',
            'N',
            '0',
            '9',
            ' ',
            '-',
            '.',
            '_',
            'é',
            'ü',
            '\u{1F600}',
        ]),
        0..12,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Every device type, the five Vulkan names and the raw values behind them:
/// `ash` models the type as a newtype, so a driver reporting a type this build
/// has no name for is expressible and has to stay harmless.
fn physical_device_type() -> impl Strategy<Value = vk::PhysicalDeviceType> {
    prop_oneof![
        prop::sample::select(&[
            vk::PhysicalDeviceType::OTHER,
            vk::PhysicalDeviceType::INTEGRATED_GPU,
            vk::PhysicalDeviceType::DISCRETE_GPU,
            vk::PhysicalDeviceType::VIRTUAL_GPU,
            vk::PhysicalDeviceType::CPU,
        ]),
        any::<i32>().prop_map(vk::PhysicalDeviceType::from_raw)
    ]
}
