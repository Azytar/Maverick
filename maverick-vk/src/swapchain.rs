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
/// A `current_extent` of `0xFFFFFFFF` in *both* fields is the surface handing
/// the size back to the application, so the request is honoured within the
/// advertised window. Any other `current_extent` means the window manager (not
/// us) owns the size, so it wins and the request is ignored.
pub fn clamp_extent(caps: &vk::SurfaceCapabilitiesKHR, width: u32, height: u32) -> vk::Extent2D {
    // Both axes, not one: the spec's "you choose" sentinel is the pair of
    // `0xFFFFFFFF` fields, and a driver that reported one real size and one
    // sentinel would have told us it owns that size — which has to be honoured
    // exactly, since `vkCreateSwapchainKHR` requires the image extent to equal
    // the surface's `currentExtent` when the surface has one.
    if caps.current_extent.width != u32::MAX || caps.current_extent.height != u32::MAX {
        return caps.current_extent;
    }
    vk::Extent2D {
        width: clamp_axis(
            width,
            caps.min_image_extent.width,
            caps.max_image_extent.width,
        ),
        height: clamp_axis(
            height,
            caps.min_image_extent.height,
            caps.max_image_extent.height,
        ),
    }
}

/// `want` restricted to the surface's own bounds on one axis.
///
/// The two bounds are ordered first, because a driver that reported them the
/// wrong way round would otherwise take the process down: `u32::clamp` panics
/// when its minimum exceeds its maximum, and a panic here is a crash of a
/// compositor that is otherwise doing nothing but presenting. Ordering the pair
/// is the only reading that is total, and it is what a window manager that does
/// not trust a driver can do; a real driver never inverts the window, so for
/// every conforming one the two orders agree.
fn clamp_axis(want: u32, low: u32, high: u32) -> u32 {
    let (low, high) = if low <= high {
        (low, high)
    } else {
        (high, low)
    };
    want.clamp(low, high)
}

/// Choose the swapchain image count: `min_image_count + 1` so a frame can be
/// recorded while another is being presented, capped at `max_image_count`
/// (which is `0` when the driver sets no upper bound).
///
/// The addition saturates: a driver that reported `min_image_count == u32::MAX`
/// is reporting a minimum no swapchain could satisfy, and the answers are then
/// either its minimum or the cap, both of which are the only counts a driver
/// could actually be asked for. Wrapping to zero instead would ask for a
/// swapchain with no images, which can never present a frame.
pub fn choose_image_count(caps: &vk::SurfaceCapabilitiesKHR) -> u32 {
    let mut count = caps.min_image_count.saturating_add(1);
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
        // SAFETY: all three queries need a live physical device, a live surface
        // handle and a live surface loader, and the surface is a child of the
        // same instance that owns `device.physical` — both outlive the swapchain
        // being created here. Each returned slice is read by the pure helpers
        // below and dropped before the next call, so nothing is borrowed across a
        // driver call.
        let caps = unsafe {
            surface
                .loader
                .get_physical_device_surface_capabilities(device.physical, surface.handle)
        }?;
        // SAFETY: as above.
        let formats = unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(device.physical, surface.handle)
        }?;
        // SAFETY: as above.
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
            // EXCLUSIVE means the images are owned by the one family that both
            // renders and presents them, and the spec requires the family array
            // to be empty in that case.
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
        // SAFETY: `create_info` is built from values that all come from the same
        // surface, physical device and logical device: `surface.handle` is live,
        // `fmt` was chosen from the surface's own format list, `extent` was
        // clamped into the surface's own extent window, and `caps.current_transform`
        // and `composite_alpha` were taken from the surface's own capabilities —
        // which is what the swapchain rules require, since the image is created
        // with them and the driver does not adjust them. `image_count` comes from
        // the same capabilities. `queue_family_indices` is non-empty exactly when
        // the mode is `CONCURRENT`, and then names both families the device was
        // created with, which is what that mode requires; it is empty for
        // `EXCLUSIVE`, as that requires. `image_usage` asks for
        // `TRANSFER_DST` because the frame loop clears the image with
        // `vkCmdClearColorImage`. `old_swapchain`, when set, is the handle this
        // crate is replacing: the caller has already waited for the device to be
        // idle, so the driver may retire those images in place, and the caller
        // destroys the old handle itself exactly once afterwards. No allocator is
        // passed, and the returned handle is moved into `self`.
        let handle = unsafe { loader.create_swapchain(&create_info, None) }?;

        let device_handle = device.handle.clone();
        // SAFETY: `handle` is the swapchain just created on `loader`'s device and
        // has not been destroyed, which is what `vkGetSwapchainImagesKHR` needs;
        // the slice it returns is moved into `self` and lives as long as the
        // swapchain does.
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
                // SAFETY: `image` came from the swapchain above, so it is a live
                // image of this swapchain on this device, and the view describes
                // it truthfully — the same format the swapchain was created with,
                // a whole 2D colour image of its single mip and array layer. The
                // handle is moved into `self.views`, which destroys exactly these
                // views and then the swapchain. If one view in this loop fails,
                // the handles collected so far are dropped as `vk` newtypes and
                // leak their Vulkan objects; that is a driver-side exhaustion
                // case, and it leaves the swapchain this crate still owns
                // installed and usable, which is the state the caller recovers
                // to.
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
        // SAFETY: every handle destroyed here was created in `Swapchain::new` on
        // `self.device`, which is a clone of the `ash::Device` the swapchain
        // loader belongs to and is still alive — the swapchain is a field of
        // `Vulkan` declared before `device`, so this runs first. The views are
        // destroyed before the swapchain, which is the order the spec asks for,
        // and each exactly once because `destroy` takes `self` by value and
        // `mem::forget`s the remainder, so a replaced swapchain is never
        // destroyed twice. Both destroy calls need the object's work to have
        // completed: `Vulkan::drop` waits for the device to be idle before any
        // field runs, and `Vulkan::recreate_swapchain` waits the same way before
        // it destroys the swapchain it replaces, which is what
        // `vkDestroySwapchainKHR` requires of the images acquired from it. The
        // device is alive throughout — the swapchain is a child of it and goes
        // first. NULL allocators match the ones the objects were created with.
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
    ///
    /// The caller must have waited for the device to be idle first, for the same
    /// reason [`Drop`] may destroy a swapchain at all: the images acquired from
    /// it must have no outstanding operations
    /// (VUID-vkDestroySwapchainKHR-swapchain-01282). `recreate_swapchain` is
    /// that caller.
    pub(crate) fn destroy(mut self) {
        let views = std::mem::take(&mut self.views);
        let _images = std::mem::take(&mut self.images);
        // SAFETY: the same handles `Drop` would destroy, destroyed once each, in
        // the same order, with the same preconditions already established there.
        unsafe {
            for &view in &views {
                self.device.destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
        std::mem::forget(self);
    }
}
