//! The window-system surface: a `VK_KHR_xcb_surface` anchored to a raw
//! `xcb_connection_t*` and an X window.
//!
//! The connection arrives as a bare `*mut c_void` on purpose, so this crate
//! stays free of any X11 or x11rb dependency and does not have to agree with
//! the rest of the workspace on a connection wrapper type. The contract is
//! simply that the pointer is a live `xcb_connection_t*` and must outlive the
//! `Vulkan` value that owns the surface.

use ash::vk;
use std::os::raw::c_void;

use crate::error::VkError;

pub struct Surface {
    /// Generic KHR surface loader. Used for capability/format/present-mode and
    /// present-support queries during device selection and presentation.
    pub loader: ash::khr::surface::Instance,
    pub handle: vk::SurfaceKHR,
}

impl Surface {
    /// Create an XCB-backed surface.
    ///
    /// # Safety contract
    /// `xcb_connection` must be a valid, live `xcb_connection_t*` for the whole
    /// lifetime of `Vulkan`. `window` must be a real X window on that
    /// connection.
    pub fn new(
        entry: &ash::Entry,
        instance: &ash::Instance,
        xcb_connection: *mut c_void,
        window: u32,
    ) -> Result<Self, VkError> {
        if xcb_connection.is_null() {
            return Err(VkError::Surface("xcb_connection is null".into()));
        }
        let xcb = ash::khr::xcb_surface::Instance::new(entry, instance);
        let info = vk::XcbSurfaceCreateInfoKHR::default()
            .connection(xcb_connection)
            .window(window);
        let handle = unsafe { xcb.create_xcb_surface(&info, None) }?;

        let loader = ash::khr::surface::Instance::new(entry, instance);
        Ok(Self { loader, handle })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            self.loader.destroy_surface(self.handle, None);
        }
    }
}
