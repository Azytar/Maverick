// maverick-vk/src/device.rs
//
// Physical-device selection and the logical device. The selector is vendor-
// agnostic: it scores discrete > integrated > cpu > other and never names a
// specific vendor, so it works the same on Intel/Mesa, AMD/RADV and NVIDIA/NVK.
// A chosen device must expose `VK_KHR_swapchain`, a graphics queue family, a
// present-capable queue family (same family is fine), and a non-empty
// surface format + present-mode set.

use std::ffi::CStr;
use std::fmt;

use ash::vk;

use crate::error::VkError;
use crate::surface::Surface;

/// Diagnostic snapshot of the chosen GPU, for startup logging.
#[derive(Debug, Clone)]
pub struct DeviceReport {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: vk::PhysicalDeviceType,
    pub driver_version: u32,
}

impl fmt::Display for DeviceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ty = match self.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => "discrete",
            vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated",
            vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual",
            vk::PhysicalDeviceType::CPU => "cpu",
            _ => "other",
        };
        writeln!(f, "Vulkan GPU:")?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Type: {ty}")?;
        writeln!(f, "  Vendor: 0x{:04x}", self.vendor_id)?;
        writeln!(f, "  Device: 0x{:04x}", self.device_id)?;
        writeln!(f, "  Driver: 0x{:08x}", self.driver_version)
    }
}

pub struct Device {
    pub handle: ash::Device,
    pub physical: vk::PhysicalDevice,
    pub swapchain_loader: ash::khr::swapchain::Device,
    /// Queue family used for graphics/transfer work.
    pub graphics_family: u32,
    /// Queue family used for presentation (may equal `graphics_family`).
    pub present_family: u32,
    pub queue: vk::Queue,
    pub present_queue: vk::Queue,
    pub report: DeviceReport,
}

/// Return the score for a physical device type (higher is better).
pub(crate) fn score_device_type(t: vk::PhysicalDeviceType) -> i32 {
    match t {
        vk::PhysicalDeviceType::DISCRETE_GPU => 1000,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 500,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 250,
        vk::PhysicalDeviceType::CPU => 100,
        _ => 0,
    }
}

/// Does `p` expose `VK_KHR_swapchain`?
fn has_swapchain_ext(instance: &ash::Instance, p: vk::PhysicalDevice) -> bool {
    match unsafe { instance.enumerate_device_extension_properties(p) } {
        Ok(props) => props.iter().any(|e| {
            // SAFETY: `extension_name` is a NUL-terminated C string.
            let name = unsafe { CStr::from_ptr(e.extension_name.as_ptr()) };
            name == vk::KHR_SWAPCHAIN_NAME
        }),
        Err(_) => false,
    }
}

impl Device {
    /// Select the best physical device and build the logical device.
    pub fn new(instance: &ash::Instance, surface: &Surface) -> Result<Self, VkError> {
        let physical_devices = unsafe { instance.enumerate_physical_devices() }?;
        if physical_devices.is_empty() {
            return Err(VkError::NoPhysicalDevice);
        }

        let mut best: Option<(vk::PhysicalDevice, u32, u32, i32)> = None;
        for p in physical_devices {
            if let Some((g, pr, score)) = Self::rate(instance, surface, p)? {
                if best.as_ref().map(|b| score > b.3).unwrap_or(true) {
                    best = Some((p, g, pr, score));
                }
            }
        }

        let (p, graphics_family, present_family, _score) = best.ok_or(VkError::NoPhysicalDevice)?;

        // Logical device: enable swapchain. Validation is instance-level, so no
        // device layers are requested here.
        let swapchain_ext = vk::KHR_SWAPCHAIN_NAME.as_ptr();
        let ext_ptrs = [swapchain_ext];

        // One queue create info per *distinct* family.
        let mut qcis = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(graphics_family);
        qcis.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_family)
                .queue_priorities(&[1.0f32]),
        );
        if present_family != graphics_family && seen.insert(present_family) {
            qcis.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(present_family)
                    .queue_priorities(&[1.0f32]),
            );
        }

        let features = vk::PhysicalDeviceFeatures::default();
        let create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&qcis)
            .enabled_extension_names(&ext_ptrs)
            .enabled_features(&features);

        let handle = unsafe { instance.create_device(p, &create_info, None) }
            .map_err(|r| VkError::Device(r.to_string()))?;
        let swapchain_loader = ash::khr::swapchain::Device::new(instance, &handle);

        let queue = unsafe { handle.get_device_queue(graphics_family, 0) };
        let present_queue = if present_family == graphics_family {
            queue
        } else {
            unsafe { handle.get_device_queue(present_family, 0) }
        };

        let props = unsafe { instance.get_physical_device_properties(p) };
        let device_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let report = DeviceReport {
            name: device_name,
            vendor_id: props.vendor_id,
            device_id: props.device_id,
            device_type: props.device_type,
            driver_version: props.driver_version,
        };

        Ok(Self {
            handle,
            physical: p,
            swapchain_loader,
            graphics_family,
            present_family,
            queue,
            present_queue,
            report,
        })
    }

    /// Score a physical device; returns `Some((graphics_family, present_family,
    /// score))` only if it meets every hard requirement.
    fn rate(
        instance: &ash::Instance,
        surface: &Surface,
        p: vk::PhysicalDevice,
    ) -> Result<Option<(u32, u32, i32)>, VkError> {
        if !has_swapchain_ext(instance, p) {
            return Ok(None);
        }

        let queue_families = unsafe { instance.get_physical_device_queue_family_properties(p) };

        // First graphics family, and a present-capable family (prefer the same
        // one if it can also present).
        let mut graphics_family: Option<u32> = None;
        let mut present_family: Option<u32> = None;
        for (idx, qf) in queue_families.iter().enumerate() {
            let idx = idx as u32;
            if qf.queue_flags.contains(vk::QueueFlags::GRAPHICS) && graphics_family.is_none() {
                graphics_family = Some(idx);
            }
            let supports_present = unsafe {
                surface
                    .loader
                    .get_physical_device_surface_support(p, idx, surface.handle)
            }
            .unwrap_or(false);
            if supports_present {
                if present_family.is_none() {
                    present_family = Some(idx);
                }
                // Prefer the graphics family if it also presents.
                if Some(idx) == graphics_family {
                    present_family = Some(idx);
                }
            }
        }

        let (graphics_family, present_family) = match (graphics_family, present_family) {
            (Some(g), Some(pr)) => (g, pr),
            _ => return Ok(None),
        };

        // Swapchain support must be non-empty.
        let formats = unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(p, surface.handle)
        }?;
        let modes = unsafe {
            surface
                .loader
                .get_physical_device_surface_present_modes(p, surface.handle)
        }?;
        if formats.is_empty() || modes.is_empty() {
            return Ok(None);
        }

        let props = unsafe { instance.get_physical_device_properties(p) };
        let score = score_device_type(props.device_type);
        Ok(Some((graphics_family, present_family, score)))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            self.handle.destroy_device(None);
        }
    }
}
