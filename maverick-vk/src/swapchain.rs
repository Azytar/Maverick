// maverick-vk/src/swapchain.rs
//
// Swapchain creation plus the *pure* selection helpers (format / present-mode /
// extent / image-count). The helpers are free of any Vulkan handle so they can
// be unit-tested in `tests/unit.rs` without a GPU or instance.


use ash::vk;

use crate::device::Device;
use crate::error::VkError;
use crate::surface::Surface;

/// Preferred surface format: sRGB BGRA8 when available.
pub(crate) const PREFERRED_FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;
pub(crate) const PREFERRED_COLOR_SPACE: vk::ColorSpaceKHR = vk::ColorSpaceKHR::SRGB_NONLINEAR;

/// Pick a surface format.
///
/// If the surface reports exactly one format with `FORMAT_UNDEFINED`, the
/// implementation lets us choose any format — return the preferred sRGB BGRA8.
/// Otherwise prefer `B8G8R8A8_SRGB`, falling back to the first reported format.
pub fn choose_surface_format(formats: &[vk::SurfaceFormatKHR]) -> vk::SurfaceFormatKHR {
    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        return vk::SurfaceFormatKHR {
            format: PREFERRED_FORMAT,
            color_space: PREFERRED_COLOR_SPACE,
        };
    }
    *formats
        .iter()
        .find(|f| f.format == PREFERRED_FORMAT && f.color_space == PREFERRED_COLOR_SPACE)
        .or_else(|| formats.iter().find(|f| f.format == PREFERRED_FORMAT))
        .unwrap_or(&formats[0])
}

/// Pick a present mode. Prefer `MAILBOX` (lowest latency, no tearing), but it is
/// never guaranteed; always fall back to `FIFO` (mandatory on every driver).
pub fn choose_present_mode(modes: &[vk::PresentModeKHR]) -> vk::PresentModeKHR {
    if modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    }
}

/// Clamp the requested extent to the surface's min/max. If the surface reports
/// a *fixed* current extent (`u32::MAX` means "use the window size"), honour it.
pub fn clamp_extent(
    caps: &vk::SurfaceCapabilitiesKHR,
    width: u32,
    height: u32,
) -> vk::Extent2D {
    if caps.current_extent.width != u32::MAX {
        return caps.current_extent;
    }
    let w = width.clamp(caps.min_image_extent.width, caps.max_image_extent.width);
    let h = height.clamp(caps.min_image_extent.height, caps.max_image_extent.height);
    vk::Extent2D { width: w, height: h }
}

/// Choose the swapchain image count: `min_image_count + 1`, capped at
/// `max_image_count` (when `max != 0`).
pub fn choose_image_count(caps: &vk::SurfaceCapabilitiesKHR) -> u32 {
    let mut count = caps.min_image_count + 1;
    if caps.max_image_count != 0 && count > caps.max_image_count {
        count = caps.max_image_count;
    }
    count
}

pub struct Swapchain {
    pub loader: ash::khr::swapchain::Device,
    /// Core device handle, retained only to destroy image views in `Drop`.
    device: ash::Device,
    pub handle: vk::SwapchainKHR,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
}

impl Swapchain {
    /// Create (or, when `old` is `Some`, recreate) the swapchain and its image
    /// views for the given target size.
    pub fn new(
        device: &Device,
        surface: &Surface,
        width: u32,
        height: u32,
        old: Option<vk::SwapchainKHR>,
    ) -> Result<Self, VkError> {
        let caps = unsafe {
            surface
                .loader
                .get_physical_device_surface_capabilities(device.physical, surface.handle)
        }?;
        let formats = unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(device.physical, surface.handle)
        }?;
        let modes = unsafe {
            surface
                .loader
                .get_physical_device_surface_present_modes(device.physical, surface.handle)
        }?;

        let fmt = choose_surface_format(&formats);
        let present_mode = choose_present_mode(&modes);
        let extent = clamp_extent(&caps, width, height);
        let image_count = choose_image_count(&caps);

        let sharing = if device.graphics_family == device.present_family {
            vk::SharingMode::EXCLUSIVE
        } else {
            vk::SharingMode::CONCURRENT
        };
        let family_indices = [device.graphics_family, device.present_family];
        let queue_family_indices: &[u32] = if sharing == vk::SharingMode::CONCURRENT {
            &family_indices
        } else {
            &[]
        };

        // Prefer opaque compositing; fall back to the first supported flag.
        let composite_alpha = if caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            vk::CompositeAlphaFlagsKHR::OPAQUE
        } else {
            [
                vk::CompositeAlphaFlagsKHR::OPAQUE,
                vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
                vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
                vk::CompositeAlphaFlagsKHR::INHERIT,
            ]
            .into_iter()
            .find(|f| caps.supported_composite_alpha.contains(*f))
            .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE)
        };

        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface.handle)
            .min_image_count(image_count)
            .image_format(fmt.format)
            .image_color_space(fmt.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .image_sharing_mode(sharing)
            .queue_family_indices(queue_family_indices)
            .pre_transform(caps.current_transform)
            .composite_alpha(composite_alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old.unwrap_or(vk::SwapchainKHR::null()));

        let loader = device.swapchain_loader.clone();
        let handle = unsafe { loader.create_swapchain(&create_info, None) }?;

        let device_handle = device.handle.clone();
        let images = unsafe { loader.get_swapchain_images(handle) }?;
        let views = images
            .iter()
            .map(|&image| {
                let view_ci = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(fmt.format)
                    .components(vk::ComponentMapping::default())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                unsafe { device_handle.create_image_view(&view_ci, None) }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            loader,
            device: device_handle,
            handle,
            images,
            views,
            format: fmt.format,
            extent,
        })
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        unsafe {
            for &view in &self.views {
                self.device.destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}
