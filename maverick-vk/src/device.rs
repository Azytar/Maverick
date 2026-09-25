//! Physical-device selection and logical-device creation.
//!
//! Selection is vendor-agnostic: device types are ranked discrete > virtual >
//! integrated > cpu > other, so the same code works on Intel/Mesa, AMD/RADV and
//! NVIDIA/NVK. A candidate is eligible only if it exposes `VK_KHR_swapchain`,
//! a graphics queue family, a present-capable queue family (one family may
//! serve both), and a non-empty surface format and present-mode set.

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

/// Rank a physical device type; higher wins. Only the order matters, the gaps
/// are arbitrary: `Device::new` keeps the first candidate when scores tie.
pub(crate) fn score_device_type(t: vk::PhysicalDeviceType) -> i32 {
    match t {
        vk::PhysicalDeviceType::DISCRETE_GPU => 1000,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 500,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 250,
        vk::PhysicalDeviceType::CPU => 100,
        _ => 0,
    }
}

/// Whether `p` exposes `VK_KHR_swapchain`. An enumeration failure disqualifies
/// the device rather than aborting the whole selection.
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

        // Only `VK_KHR_swapchain` is enabled on the device: validation is an
        // instance-level layer, so no device layers are requested here.
        let swapchain_ext = vk::KHR_SWAPCHAIN_NAME.as_ptr();
        let ext_ptrs = [swapchain_ext];

        // One queue create info per *distinct* family. The priority slices must
        // be named bindings, not `&[1.0f32]` temporaries inside the `push`
        // calls: such a temporary dies at the end of its statement and leaves
        // the `DeviceQueueCreateInfo` holding a dangling pointer that
        // `create_device` dereferences below.
        let gfx_priorities = [1.0f32];
        let present_priorities = [1.0f32];
        let mut qcis = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(graphics_family);
        qcis.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_family)
                .queue_priorities(&gfx_priorities),
        );
        if present_family != graphics_family && seen.insert(present_family) {
            qcis.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(present_family)
                    .queue_priorities(&present_priorities),
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

    /// Score a physical device, returning `Some((graphics_family,
    /// present_family, score))` only when every hard requirement is met.
    fn rate(
        instance: &ash::Instance,
        surface: &Surface,
        p: vk::PhysicalDevice,
    ) -> Result<Option<(u32, u32, i32)>, VkError> {
        if !has_swapchain_ext(instance, p) {
            return Ok(None);
        }

        let queue_families = unsafe { instance.get_physical_device_queue_family_properties(p) };

        // First graphics family wins. The present family is preferred whenever
        // it is that same family: it keeps the swapchain EXCLUSIVE instead of
        // forcing CONCURRENT access from two queue families.
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
                if Some(idx) == graphics_family {
                    present_family = Some(idx);
                }
            }
        }

        let (graphics_family, present_family) = match (graphics_family, present_family) {
            (Some(g), Some(pr)) => (g, pr),
            _ => return Ok(None),
        };

        // A device whose surface reports no formats or no present modes cannot
        // back a swapchain, so it is not eligible no matter how good it is.
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

// The ranking is the one piece of device selection that is pure, so it is
// exercised here rather than in `tests/`: `score_device_type` is crate-private
// and making it public to reach it from an integration test would commit this
// crate to an API it does not otherwise need. `#[cfg(test)]` keeps the test out
// of the library build entirely, so `proptest` stays a dev-dependency.
#[cfg(test)]
mod tests {
    use super::score_device_type;
    use ash::vk;
    use proptest::prelude::*;

    /// The five types the ranking names. The order of this list is not a
    /// preference, it only makes every named type reachable in one draw.
    static KNOWN: [vk::PhysicalDeviceType; 5] = [
        vk::PhysicalDeviceType::DISCRETE_GPU,
        vk::PhysicalDeviceType::INTEGRATED_GPU,
        vk::PhysicalDeviceType::VIRTUAL_GPU,
        vk::PhysicalDeviceType::CPU,
        vk::PhysicalDeviceType::OTHER,
    ];

    proptest! {
        /// Ranking has to survive every value the type can hold, including the
        /// ones no version of `ash` names: an unknown device type is a driver
        /// Maverick does not recognise, and a `match` that treated it as
        /// impossible would take the whole bootstrap down over a device it would
        /// only have skipped. What has to come back is one of the five classes
        /// the ranking defines — a value of its own would be a preference no
        /// named type could be compared against.
        #[test]
        fn device_type_score_is_total_over_the_whole_type_space(raw in any::<i32>()) {
            let ty = vk::PhysicalDeviceType::from_raw(raw);
            let score = score_device_type(ty);
            let classes: Vec<i32> = KNOWN.iter().map(|t| score_device_type(*t)).collect();

            prop_assert!(
                classes.contains(&score),
                "{ty:?} scored {score}, which is not one of the ranked classes {classes:?}"
            );
            prop_assert_eq!(
                score,
                score_device_type(vk::PhysicalDeviceType::from_raw(raw)),
                "the same type must always score the same"
            );
        }

        /// The selection loop only ever compares scores, so they have to form a
        /// strict order over the named types: no two of them may tie, or which
        /// one wins would come down to the order the loader happened to
        /// enumerate the devices in. A discrete GPU is the best candidate
        /// whatever else is plugged in, and transitivity is what makes "better
        /// than" a single relation rather than a cycle the enumeration order
        /// could break.
        #[test]
        fn known_device_types_form_a_strict_order_with_discrete_first(
            a in prop::sample::select(&KNOWN[..]),
            b in prop::sample::select(&KNOWN[..]),
            c in prop::sample::select(&KNOWN[..]),
        ) {
            let a_score = score_device_type(a);
            let b_score = score_device_type(b);
            let c_score = score_device_type(c);

            // A discrete GPU is the best candidate whatever else is plugged in.
            // Checked over all five named types rather than over the drawn
            // ones, so "best" is a total claim and not a statistical one.
            let discrete = score_device_type(vk::PhysicalDeviceType::DISCRETE_GPU);
            for t in KNOWN {
                if t != vk::PhysicalDeviceType::DISCRETE_GPU {
                    prop_assert!(
                        discrete > score_device_type(t),
                        "a discrete GPU ({discrete}) did not outrank {:?} ({})",
                        t,
                        score_device_type(t)
                    );
                }
            }
            if a != b {
                prop_assert_ne!(
                    a_score,
                    b_score,
                    "{:?} and {:?} tie, so the first one enumerated would win",
                    a,
                    b
                );
            }
            if a_score > b_score && b_score > c_score {
                prop_assert!(
                    a_score > c_score,
                    "{a:?} beats {b:?} beats {c:?}, yet not {a:?} over {c:?}"
                );
            }
        }

        /// A type Maverick does not know must never be preferred over a
        /// discrete GPU. That is not a judgement about the hardware: an
        /// unrecognised value is a driver reporting something outside the spec,
        /// and letting it outrank the best known candidate would hand the
        /// compositor to whatever the driver claimed. Unknown types also share
        /// one class, so they tie with each other and the loop's "first
        /// candidate wins" rule applies to them rather than to the order they
        /// happened to be enumerated in.
        #[test]
        fn unknown_device_type_never_outranks_a_discrete_gpu(
            raw in any::<i32>(),
            other in any::<i32>(),
        ) {
            let ty = vk::PhysicalDeviceType::from_raw(raw);
            let other = vk::PhysicalDeviceType::from_raw(other);
            prop_assume!(!KNOWN.contains(&ty), "{:?} is a type the ranking names", ty);
            prop_assume!(
                !KNOWN.contains(&other),
                "{:?} is a type the ranking names",
                other
            );

            let score = score_device_type(ty);
            prop_assert!(
                score < score_device_type(vk::PhysicalDeviceType::DISCRETE_GPU),
                "an unrecognised type outranked a discrete GPU with {score}"
            );
            prop_assert_eq!(
                score,
                score_device_type(other),
                "two unrecognised types must share one class, or the winner would depend on enumeration order"
            );
        }
    }
}
