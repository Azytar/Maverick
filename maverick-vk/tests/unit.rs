use ash::vk;
use maverick_vk::{
    choose_image_count, choose_present_mode, choose_surface_format, clamp_extent, VkError,
};

fn fmt(format: vk::Format, space: vk::ColorSpaceKHR) -> vk::SurfaceFormatKHR {
    vk::SurfaceFormatKHR {
        format,
        color_space: space,
    }
}

#[test]
fn format_prefers_bgra8_srgb() {
    let formats = vec![
        fmt(vk::Format::R8G8B8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
        fmt(vk::Format::B8G8R8A8_SRGB, vk::ColorSpaceKHR::SRGB_NONLINEAR),
    ];
    assert_eq!(
        choose_surface_format(&formats).expect("formats").format,
        vk::Format::B8G8R8A8_SRGB
    );
}

#[test]
fn format_undefined_single_allows_any() {
    let formats = vec![fmt(
        vk::Format::UNDEFINED,
        vk::ColorSpaceKHR::SRGB_NONLINEAR,
    )];
    let f = choose_surface_format(&formats).expect("formats");
    assert_eq!(f.format, vk::Format::B8G8R8A8_SRGB);
    assert_eq!(f.color_space, vk::ColorSpaceKHR::SRGB_NONLINEAR);
}

#[test]
fn format_falls_back_to_first() {
    let formats = vec![fmt(
        vk::Format::R8G8B8_SRGB,
        vk::ColorSpaceKHR::SRGB_NONLINEAR,
    )];
    assert_eq!(
        choose_surface_format(&formats).expect("formats").format,
        vk::Format::R8G8B8_SRGB
    );
}

#[test]
fn format_empty_is_none_not_panic() {
    assert!(choose_surface_format(&[]).is_none());
}

#[test]
fn present_mode_prefers_mailbox_then_fifo() {
    assert_eq!(
        choose_present_mode(&[vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::FIFO]),
        vk::PresentModeKHR::MAILBOX
    );
    assert_eq!(
        choose_present_mode(&[vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::FIFO]),
        vk::PresentModeKHR::FIFO
    );
    // `choose_present_mode` cannot return anything else: every driver must
    // report `FIFO`, and it is not preferred over `MAILBOX`.
    assert_eq!(
        choose_present_mode(&[vk::PresentModeKHR::FIFO]),
        vk::PresentModeKHR::FIFO
    );
}

#[test]
fn extent_clamps_within_bounds() {
    let caps = vk::SurfaceCapabilitiesKHR {
        min_image_extent: vk::Extent2D {
            width: 16,
            height: 16,
        },
        max_image_extent: vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        current_extent: vk::Extent2D {
            width: u32::MAX,
            height: u32::MAX,
        },
        ..Default::default()
    };
    let e = clamp_extent(&caps, 4096, 0);
    assert_eq!(e.width, 2048);
    assert_eq!(e.height, 16);
}

#[test]
fn extent_uses_current_when_fixed() {
    let caps = vk::SurfaceCapabilitiesKHR {
        current_extent: vk::Extent2D {
            width: 800,
            height: 600,
        },
        ..Default::default()
    };
    let e = clamp_extent(&caps, 10, 10);
    assert_eq!(
        e,
        vk::Extent2D {
            width: 800,
            height: 600
        }
    );
}

/// The "you choose" sentinel is `0xFFFFFFFF` in *both* fields. A driver that
/// reported one real size and one sentinel has said it owns the size, and
/// `vkCreateSwapchainKHR` requires that size to be used exactly, so a helper
/// that only looked at one axis would hand back a size the surface never agreed
/// to.
#[test]
fn extent_uses_current_when_only_one_axis_is_real() {
    let caps = vk::SurfaceCapabilitiesKHR {
        min_image_extent: vk::Extent2D {
            width: 16,
            height: 16,
        },
        max_image_extent: vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        current_extent: vk::Extent2D {
            width: u32::MAX,
            height: 600,
        },
        ..Default::default()
    };
    assert_eq!(
        clamp_extent(&caps, 10, 10),
        vk::Extent2D {
            width: u32::MAX,
            height: 600
        }
    );
}

/// A driver that reported its minimum extent above its maximum must not take
/// the process down: `u32::clamp` panics on that pair. The bounds are ordered
/// instead, so the window is the pair read the right way round and a request
/// inside it is still honoured — this is a window, not a fallback to one bound.
#[test]
fn extent_survives_an_inverted_window() {
    let caps = vk::SurfaceCapabilitiesKHR {
        min_image_extent: vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        max_image_extent: vk::Extent2D {
            width: 16,
            height: 16,
        },
        current_extent: vk::Extent2D {
            width: u32::MAX,
            height: u32::MAX,
        },
        ..Default::default()
    };
    let inside = clamp_extent(&caps, 800, 600);
    assert_eq!(inside.width, 800);
    assert_eq!(inside.height, 600);
    let outside = clamp_extent(&caps, 4096, 4096);
    assert_eq!(outside.width, 2048);
    assert_eq!(outside.height, 2048);
}

#[test]
fn image_count_plus_one_capped() {
    let caps = vk::SurfaceCapabilitiesKHR {
        min_image_count: 2,
        max_image_count: 3,
        ..Default::default()
    };
    assert_eq!(choose_image_count(&caps), 3);
    let open = vk::SurfaceCapabilitiesKHR {
        min_image_count: 2,
        max_image_count: 0,
        ..Default::default()
    };
    assert_eq!(choose_image_count(&open), 3);
}

/// A minimum of `u32::MAX` has no room for the `+ 1`; wrapping would ask for
/// zero images, which can never present a frame.
#[test]
fn image_count_does_not_overflow_at_the_top_of_the_range() {
    let caps = vk::SurfaceCapabilitiesKHR {
        min_image_count: u32::MAX,
        max_image_count: 0,
        ..Default::default()
    };
    assert_eq!(choose_image_count(&caps), u32::MAX);
    let capped = vk::SurfaceCapabilitiesKHR {
        min_image_count: u32::MAX,
        max_image_count: 4,
        ..Default::default()
    };
    assert_eq!(choose_image_count(&capped), 4);
}

#[test]
fn vk_error_from_vk_result_is_descriptive() {
    let e: VkError = vk::Result::ERROR_DEVICE_LOST.into();
    assert!(format!("{e}").contains("ERROR_DEVICE_LOST"));
}
