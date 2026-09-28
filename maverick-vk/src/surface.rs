//! The window-system surface: a `VK_KHR_xcb_surface` anchored to a raw
//! `xcb_connection_t*` and an X window.
//!
//! The connection arrives as a bare `*mut c_void` on purpose, so this crate
//! stays free of any X11 or x11rb dependency and does not have to agree with
//! the rest of the workspace on a connection wrapper type. The contract is
//! simply that the pointer is a live `xcb_connection_t*` and must outlive the
//! `Vulkan` value that owns the surface.
//!
//! # The borrow, and why it is the caller's to keep
//!
//! A `VK_KHR_xcb_surface` does not own or reference-count the `xcb_connection_t*`
//! it was created from: the driver reads from and writes to that connection
//! directly, and `vkDestroySurfaceKHR` does not close it. So the connection has
//! to outlive the surface, and Rust cannot enforce that here because the pointer
//! arrives as a bare `*mut c_void` with no lifetime attached to it — the borrow
//! is stated in [`Surface::new`]'s contract, checked for null there, and
//! re-checked by the caller (`Vulkan::new` takes the pointer in a
//! `SurfaceTarget` it does not keep). Wrapping the connection in a lifetime
//! parameter would be the alternative, and it is deliberately not done: this
//! crate takes one already-borrowed pointer from a caller that already owns a
//! connection wrapper, and inventing a second one would put this crate's idea of
//! an X connection in competition with the rest of the workspace's. What the
//! contract buys is that the unsafe here is one call with one stated
//! precondition, rather than a wrapper type whose invariant would be just as
//! uncheckable and spread over the whole surface lifetime.
//!
//! The consequence to be aware of: closing the X connection before the surface
//! is destroyed leaves the driver's presentation requests going to a connection
//! that no longer exists. The teardown waits for the device to be idle before
//! destroying the surface, so the requests have been made by then, but the
//! connection must still be open at that point. A caller therefore has to drop
//! the `Vulkan` value before it closes the connection it supplied — which
//! `tests/smoke.rs` does by holding the connection for the whole test.

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
    /// lifetime of `Vulkan` — see the module docs for why that cannot be
    /// checked here. A null pointer is rejected below; a non-null pointer that is
    /// not a connection, or a `window` that is not a live X window on that
    /// connection, is rejected by the driver at creation rather than by this
    /// crate, because neither is something safe Rust can tell from the value.
    /// `window` is an XID, not a pointer, so no dereference is implied by
    /// passing one.
    pub fn new(
        entry: &ash::Entry,
        instance: &ash::Instance,
        xcb_connection: *mut c_void,
        window: u32,
    ) -> Result<Self, VkError> {
        if xcb_connection.is_null() {
            return Err(VkError::Surface("xcb_connection is null".into()));
        }
        // SAFETY: the null check above rules out the one value that cannot be a
        // connection, and the rest of the contract is the caller's as stated
        // above. `xcb_surface` is one of the two instance extensions this crate
        // enables unconditionally, so the loader's entry points exist, and both
        // `entry` and `instance` are the live pair this surface is a child of —
        // the instance outlives it, being declared after it in `Vulkan`. The
        // create info holds only the two copied values and the driver reads
        // through the connection for the rest of the surface's life, which is
        // why the connection has to outlive it.
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
        // SAFETY: `self.handle` is the surface this struct created and destroys
        // nowhere else, and `self.loader` holds the entry points of the instance
        // it was created from, which is still alive — the surface is a field
        // declared before the instance in `Vulkan`. By the time this runs,
        // `Vulkan::drop` has already waited for the device to be idle, so no
        // presentation request is still using the surface, and the swapchain
        // built on it — a child of the *device*, declared before it — has
        // already gone. A NULL `pAllocator` matches the one the surface was
        // created with. The X connection is deliberately not closed here: this
        // crate borrowed it and does not own it.
        unsafe {
            self.loader.destroy_surface(self.handle, None);
        }
    }
}
