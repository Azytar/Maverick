//! The Vulkan instance: loader, instance extensions/layers, and an optional
//! debug-utils messenger.
//!
//! No GPU is needed to build an instance, so this step succeeds on any machine
//! with `libvulkan.so.1`. Every failure it can report (`VkError::Loader`,
//! `VkError::Instance`) happens before a device exists, which is what makes the
//! error layering in `VkError` worth having.

use std::ffi::CStr;
use std::os::raw::c_void;

use ash::vk;

use crate::error::VkError;

/// Engine/app info reported to the Vulkan implementation. Mirrors the workspace
/// version so driver logs line up with the rest of Maverick.
pub(crate) const ENGINE_NAME: &CStr = c"maverick";
pub(crate) const APP_NAME: &CStr = c"maverick-vk";
pub(crate) const ENGINE_VERSION: u32 = 0x001204; // 0.18.4
pub(crate) const APP_VERSION: u32 = 0x001204;

/// Khronos validation layer name.
pub(crate) const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Required instance extensions for an X11 surface.
pub(crate) const REQUIRED_EXTENSIONS: &[&CStr] = &[vk::KHR_SURFACE_NAME, vk::KHR_XCB_SURFACE_NAME];

pub struct Instance {
    pub entry: ash::Entry,
    pub handle: ash::Instance,
    // Loader plus handle together, so `Drop` destroys the messenger before the
    // instance it was created from.
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

// Invoked by the loader from whichever thread reports the error, so it may
// touch nothing that belongs to the instance. Returning `vk::FALSE` keeps a
// validation error from aborting the process.
//
// # Safety
//
// This is the C ABI of `PFN_vkDebugUtilsMessengerCallbackEXT`, and the loader —
// not this crate — guarantees the four arguments: `p_callback_data` is a valid
// pointer to a `VkDebugUtilsMessengerCallbackDataEXT` that stays valid for the
// duration of the call, and `data.p_message` is a NUL-terminated UTF-8 string
// that does the same. Nothing is derived from `user_data`, which is null here,
// and nothing is retained: the `&CStr` is read and dropped before returning.
// The body therefore only has to trust the message pointer, which it does by
// treating a non-UTF-8 message as text to discard rather than as a reason to
// trust the rest of the struct. It also runs on a driver thread, so it holds no
// lock and allocates only the `&str` views it prints.
unsafe extern "system" fn debug_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user_data: *mut c_void,
) -> vk::Bool32 {
    if !p_callback_data.is_null() {
        let data = &*p_callback_data;
        let message = CStr::from_ptr(data.p_message)
            .to_str()
            .unwrap_or("<invalid utf-8 debug message>");
        let severity = if message_severity.intersects(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
        {
            "ERROR"
        } else if message_severity.intersects(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
            "WARN"
        } else {
            "INFO"
        };
        let _ = message_types;
        eprintln!("[vulkan-validation {severity}] {message}");
    }
    vk::FALSE
}

impl Instance {
    /// Build the instance. Validation is enabled only when `enable_validation`
    /// is true (turned on by the `MAVERICK_VK_VALIDATION=1` env var) **and** the
    /// Khronos validation layer is actually present; if the layer is missing we
    /// proceed without it rather than failing.
    pub fn new(enable_validation: bool) -> Result<Self, VkError> {
        // SAFETY: `Entry::load` only `dlopen`s the Vulkan loader and looks up
        // `vkGetInstanceProcAddr`; no Vulkan object exists yet, so the only thing
        // it needs from the host is a loader that reports a version Maverick can
        // speak. Its failure is reported as `VkError::Loader`, which is why a
        // machine with no Vulkan driver gets an error rather than a crash.
        let entry = unsafe { ash::Entry::load()? };

        let app_info = vk::ApplicationInfo::default()
            .application_name(APP_NAME)
            .application_version(APP_VERSION)
            .engine_name(ENGINE_NAME)
            .engine_version(ENGINE_VERSION)
            .api_version(vk::API_VERSION_1_2);

        // `debug_utils` is requested only when validation is on, so a normal
        // run does not depend on the extension being present.
        let use_validation = enable_validation && has_validation_layer(&entry)?;
        let mut ext_names: Vec<&CStr> = REQUIRED_EXTENSIONS.to_vec();
        if use_validation {
            ext_names.push(vk::EXT_DEBUG_UTILS_NAME);
        }
        let ext_ptrs: Vec<*const std::os::raw::c_char> =
            ext_names.iter().map(|n| n.as_ptr()).collect();

        let layer_ptrs: Vec<*const std::os::raw::c_char> = if use_validation {
            vec![VALIDATION_LAYER.as_ptr()]
        } else {
            vec![]
        };

        let create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&ext_ptrs)
            .enabled_layer_names(&layer_ptrs);

        // SAFETY: `entry` is the live loader, and the two pointer arrays are
        // built from the two `Vec`s directly above and live until the end of
        // this scope, so the `*const c_char` pointers the create info holds stay
        // valid for the call. `ext_names` is `REQUIRED_EXTENSIONS` — surface and
        // xcb-surface, both of which the loader must support for an X11 window
        // and both of which it is asked to check — plus debug-utils only when the
        // Khronos layer is confirmed present, so a machine without it still
        // boots. The layer list is empty in the same case. `app_info` requests
        // Vulkan 1.2, which the loader's own version negotiation has already
        // established, and a null `pAllocator` is always compatible.
        let handle = unsafe { entry.create_instance(&create_info, None) }
            .map_err(|r| VkError::Instance(r.to_string()))?;

        // The messenger can only be created from an existing instance, hence
        // after the `create_instance` above.
        let debug = if use_validation {
            let loader = ash::ext::debug_utils::Instance::new(&entry, &handle);
            let ci = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(debug_callback));
            // SAFETY: `debug_utils` is one of the extensions the instance was
            // created with above, and the create info's only pointer is
            // `debug_callback`, a plain `fn` item that needs no capture and
            // outlives the call. The returned messenger is kept in the same
            // struct as the loader that owns its entry points, and is destroyed
            // before the instance in `Drop`; no `user_data` is passed, so
            // `debug_callback` has nothing to keep alive.
            match unsafe { loader.create_debug_utils_messenger(&ci, None) } {
                Ok(messenger) => Some((loader, messenger)),
                // Debug output is optional: a driver that refuses the
                // messenger must not take the compositor down with it.
                Err(_) => None,
            }
        } else {
            None
        };

        Ok(Self {
            entry,
            handle,
            debug,
        })
    }

    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    pub fn handle(&self) -> &ash::Instance {
        &self.handle
    }
}

fn has_validation_layer(entry: &ash::Entry) -> Result<bool, VkError> {
    // SAFETY: `entry` is the live loader; the returned slice is the driver
    // listing its layers into an array `ash` owns for this call and the list is
    // dropped at the end of it, so nothing is borrowed across a driver call.
    let props = unsafe { entry.enumerate_instance_layer_properties() }?;
    Ok(props
        .iter()
        // `layer_name_as_c_str` is `ash`'s bounded reader rather than a raw
        // `CStr::from_ptr`: a layer that failed to NUL-terminate its own name
        // yields `Err` and simply does not match, where treating the array as a C
        // string would have read past its end.
        .any(|p| p.layer_name_as_c_str() == Ok(VALIDATION_LAYER)))
}

impl Drop for Instance {
    fn drop(&mut self) {
        // The messenger is an instance extension object: destroying the
        // instance first would leave `destroy_debug_utils_messenger` calling
        // into freed loader state.
        if let Some((loader, messenger)) = self.debug.take() {
            // SAFETY: both handles come from the same `create_debug_utils_messenger`
            // call, the loader holds the entry points of the instance still alive
            // below, and no callback into this crate is in flight — validation
            // reports are not a queue operation, so the destroy waits for nothing.
            // A NULL `pAllocator` matches the one the messenger was created with.
            unsafe {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
        }
        // SAFETY: `self.handle` is the instance this struct created and destroys
        // nowhere else. Every child object has already gone: the messenger above,
        // and in `Vulkan` the surface, the device and the swapchain, whose field
        // order places them all before this one. A NULL `pAllocator` matches the
        // one the instance was created with.
        unsafe {
            self.handle.destroy_instance(None);
        }
    }
}
