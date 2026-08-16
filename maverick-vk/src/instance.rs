// maverick-vk/src/instance.rs
//
// The Vulkan instance: loads the loader, picks instance extensions/layers, and
// optionally installs a debug-utils messenger. No GPU is required to build an
// instance, so this step succeeds on any machine that has `libvulkan.so.1`.

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
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

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
        let entry = unsafe { ash::Entry::load()? };

        let app_info = vk::ApplicationInfo::default()
            .application_name(APP_NAME)
            .application_version(APP_VERSION)
            .engine_name(ENGINE_NAME)
            .engine_version(ENGINE_VERSION)
            .api_version(vk::API_VERSION_1_2);

        // Collect the extensions the instance must enable. `debug_utils` is only
        // needed when validation is on (and only if the layer exists).
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

        let handle = unsafe { entry.create_instance(&create_info, None) }
            .map_err(|r| VkError::Instance(r.to_string()))?;

        // Install the messenger *after* the instance exists, and keep both the
        // loader and the handle so Drop can destroy it in the right order.
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
            match unsafe { loader.create_debug_utils_messenger(&ci, None) } {
                Ok(messenger) => Some((loader, messenger)),
                // A failure here must not break the whole backend.
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
    let props = unsafe { entry.enumerate_instance_layer_properties() }?;
    Ok(props
        .iter()
        .any(|p| unsafe { CStr::from_ptr(p.layer_name.as_ptr()) } == VALIDATION_LAYER))
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Destroy the debug messenger *before* the instance.
        if let Some((loader, messenger)) = self.debug.take() {
            unsafe {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
        }
        unsafe {
            self.handle.destroy_instance(None);
        }
    }
}
