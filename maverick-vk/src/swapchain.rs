//! Swapchain creation plus the *pure* selection helpers (format, present mode,
//! extent, image count).
//!
//! The helpers take plain slices and touch no Vulkan handle, so they are
//! unit-tested in `tests/unit.rs` without a loader, a surface or a GPU.

use ash::vk;

use crate::device::Device;
use crate::error::VkError;
use crate::surface::Surface;

/// Preferred surface format: sRGB BGRA8 when available.
pub(crate) const PREFERRED_FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;
pub(crate) const PREFERRED_COLOR_SPACE: vk::ColorSpaceKHR = vk::ColorSpaceKHR::SRGB_NONLINEAR;

/// Pick a surface format, or `None` when the driver reports none.
///
/// A lone `UNDEFINED` format means the surface lets us choose, so the preferred
/// sRGB BGRA8 is returned. Otherwise prefer `B8G8R8A8_SRGB` with the sRGB
/// colour space, then that format with any colour space, then whatever the
/// driver listed first.
pub fn choose_surface_format(formats: &[vk::SurfaceFormatKHR]) -> Option<vk::SurfaceFormatKHR> {
    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        return Some(vk::SurfaceFormatKHR {
            format: PREFERRED_FORMAT,
            color_space: PREFERRED_COLOR_SPACE,
        });
    }
    formats
        .iter()
        .find(|f| f.format == PREFERRED_FORMAT && f.color_space == PREFERRED_COLOR_SPACE)
        .or_else(|| formats.iter().find(|f| f.format == PREFERRED_FORMAT))
        .or_else(|| formats.first())
        .copied()
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

/// Clamp the requested extent to the surface's min/max.
///
/// A `current_extent` other than `u32::MAX` means the window manager (not us)
/// owns the size, so it wins and the request is ignored.
pub fn clamp_extent(caps: &vk::SurfaceCapabilitiesKHR, width: u32, height: u32) -> vk::Extent2D {
    if caps.current_extent.width != u32::MAX {
        return caps.current_extent;
    }
    let w = width.clamp(caps.min_image_extent.width, caps.max_image_extent.width);
    let h = height.clamp(caps.min_image_extent.height, caps.max_image_extent.height);
    vk::Extent2D {
        width: w,
        height: h,
    }
}

/// Choose the swapchain image count: `min_image_count + 1` so a frame can be
/// recorded while another is being presented, capped at `max_image_count`
/// (which is `0` when the driver sets no upper bound).
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
    /// Create the swapchain and one image view per image, for the requested
    /// size. Passing `old` sets `oldSwapchain`, which lets the driver retire
    /// the previous images in place; the caller then destroys the old
    /// swapchain with [`Swapchain::destroy`].
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
        if formats.is_empty() || modes.is_empty() {
            return Err(VkError::Swapchain(
                "surface reports no formats or present modes".into(),
            ));
        }

        let fmt = choose_surface_format(&formats)
            .ok_or_else(|| VkError::Swapchain("surface reports no formats".into()))?;
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

        // OPAQUE avoids the compositor's alpha being blended with the window;
        // the rest are probed in spec order for drivers that refuse it.
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
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
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

impl Swapchain {
    /// Destroy this swapchain's views and handle, bypassing the `Drop` impl
    /// above.
    ///
    /// `recreate_swapchain` uses this for the swapchain it replaces, which must
    /// therefore *not* be dropped as well — that would destroy the views and
    /// the handle twice. Consuming `self` is what prevents that: the two
    /// handle vectors are moved out and dropped normally (a `Vec<vk::Image>`
    /// owns no Vulkan memory, and the `ash` loaders own nothing to release),
    /// and `mem::forget` then skips `Drop` for the rest. Views go first, as
    /// the spec requires.
    pub(crate) fn destroy(mut self) {
        let views = std::mem::take(&mut self.views);
        let _images = std::mem::take(&mut self.images);
        unsafe {
            for &view in &views {
                self.device.destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
        std::mem::forget(self);
    }
}
