// maverick-vk/tests/unit.rs
//
// No-GPU unit tests: the pure selection helpers and error mapping. These run on
// any machine (`cargo test --workspace` stays green without a Vulkan driver).

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
        choose_surface_format(&formats).format,
        vk::Format::B8G8R8A8_SRGB
    );
}

#[test]
fn format_undefined_single_allows_any() {
    let formats = vec![fmt(
        vk::Format::UNDEFINED,
        vk::ColorSpaceKHR::SRGB_NONLINEAR,
    )];
    let f = choose_surface_format(&formats);
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
        choose_surface_format(&formats).format,
        vk::Format::R8G8B8_SRGB
    );
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
    // FIFO is always available.
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

#[test]
fn vk_error_from_vk_result_is_descriptive() {
    let e: VkError = vk::Result::ERROR_DEVICE_LOST.into();
    assert!(format!("{e}").contains("ERROR_DEVICE_LOST"));
}
