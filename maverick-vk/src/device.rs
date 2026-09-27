//! Physical-device selection and logical-device creation.
//!
//! Selection is vendor-agnostic: device types are ranked discrete > virtual >
//! integrated > cpu > other, so the same code works on Intel/Mesa, AMD/RADV and
//! NVIDIA/NVK. A candidate is eligible only if it exposes `VK_KHR_swapchain`,
//! a graphics queue family, a present-capable queue family (one family may
//! serve both), and a non-empty surface format and present-mode set.
//!
//! # Why the present family is preferred over the graphics one
//!
//! When a single family does both, it is chosen for both roles and the swapchain
//! is created `EXCLUSIVE` on it, so the submit queue and the present queue are
//! one queue. That is not a preference: the spec runs a queue's operations in
//! issue order, so on one queue this frame's present is ordered before the next
//! frame's submit and the render-finished semaphore is always free to be signalled
//! again. Split the two across families and the swapchain has to be `CONCURRENT`,
//! the present becomes a separate set of queue operations on a different queue,
//! and that ordering guarantee is gone — which is why
//! `Vulkan::acquire_and_present` pays for an extra wait in that case. Every
//! family index used anywhere in this crate comes from the single `rate` call
//! below, so a queue can never be fetched for a family the device was not
//! created with.

use std::ffi::CStr;
use std::fmt;

use ash::vk;

use crate::error::VkError;
use crate::surface::Surface;

/// Stand-in for a device name the driver left unterminated. Vulkan requires
/// `deviceName` to be a NUL-terminated string inside its fixed-size array, so
/// this only appears for a driver that broke that rule — and reading the array
/// as a C string to find out would be the undefined behaviour the bounded
/// accessor avoids.
const UNKNOWN_DEVICE_NAME: &CStr = c"<unnamed device>";

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
    // SAFETY: `instance` is the live `VkInstance` that owns `p` — `p` came from
    // `vkEnumeratePhysicalDevices` on this very instance, and a physical device
    // is only released with its instance, which outlives every call here.
    // `extension_name_as_c_str` is `ash`'s own bounded reader: it scans for a
    // NUL inside the fixed-size array rather than trusting the driver to have
    // terminated it, so a driver that did not cannot make this read past the
    // array. Such an entry simply does not match.
    match unsafe { instance.enumerate_device_extension_properties(p) } {
        Ok(props) => props
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(vk::KHR_SWAPCHAIN_NAME)),
        Err(_) => false,
    }
}

impl Device {
    /// Select the best physical device and build the logical device.
    pub fn new(instance: &ash::Instance, surface: &Surface) -> Result<Self, VkError> {
        // SAFETY: `instance` is the live `VkInstance` the caller built and has
        // not destroyed — it owns the device and surface this function takes
        // part in creating, and the caller keeps it alive for the whole
        // constructor. An empty list is a machine with no Vulkan device at all,
        // which is reported below rather than as a panic.
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

        // One queue create info per *distinct* family, and exactly one queue
        // requested from each. The priority slices must be named bindings, not
        // `&[1.0f32]` temporaries inside the `push` calls: such a temporary dies
        // at the end of its statement and leaves the `DeviceQueueCreateInfo`
        // holding a dangling pointer that `create_device` dereferences below.
        let gfx_priorities = [1.0f32];
        let present_priorities = [1.0f32];
        let mut qcis = Vec::new();
        qcis.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_family)
                .queue_priorities(&gfx_priorities),
        );
        // A second entry, and only when the families differ: a family listed
        // twice is a spec violation, and the one entry above already covers both
        // roles when they are the same family.
        if present_family != graphics_family {
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

        // SAFETY: `p` is one of the physical devices the live instance just
        // enumerated, and `VK_KHR_swapchain` is one of the extensions it
        // reported, so enabling it is legal. Every queue create info names a
        // family that was found by inspecting *this* physical device's queue
        // family properties, which is the requirement, and `qcis` holds at most
        // one entry per family — the same family twice would be rejected. The
        // priority slices are named locals rather than temporaries, so the
        // pointers the create info holds stay valid for the duration of the
        // call. `features` is all-zero, so no optional feature is requested that
        // `p` might not support. No allocator is passed, and the returned
        // `VkDevice` becomes this struct's `handle`, which is what makes the
        // destroys below and in `Drop` legal.
        let handle = unsafe { instance.create_device(p, &create_info, None) }
            .map_err(|r| VkError::Device(r.to_string()))?;
        let swapchain_loader = ash::khr::swapchain::Device::new(instance, &handle);

        // SAFETY: each family index is one a `DeviceQueueCreateInfo` above asked
        // a queue to be created for, and the queue index is 0 — the only index
        // requested, since each create info has exactly one priority. Both
        // queues therefore come from this very `VkDevice`, and `Drop` destroys
        // it only after every other field that holds a queue has gone. The
        // single-family case reuses the one queue rather than asking twice,
        // which the spec permits: a queue handle is a name for a family, so both
        // names refer to the same queue.
        let queue = unsafe { handle.get_device_queue(graphics_family, 0) };
        let present_queue = if present_family == graphics_family {
            queue
        } else {
            unsafe { handle.get_device_queue(present_family, 0) }
        };

        // SAFETY: `p` is a physical device of the live `instance`. The returned
        // struct is a copy the driver fills in, and `device_name_as_c_str` reads
        // it back through `ash`'s bounded accessor, so a driver that failed to
        // NUL-terminate its own name produces a placeholder rather than a read
        // past the fixed-size array.
        let props = unsafe { instance.get_physical_device_properties(p) };
        let device_name = props
            .device_name_as_c_str()
            .unwrap_or(UNKNOWN_DEVICE_NAME)
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

        // SAFETY: `p` is a physical device of the live `instance`, and the
        // returned slice is the driver filling in a caller-provided array
        // `ash` owns for the duration of this call. It is read below and not
        // kept, so nothing outlives the borrow.
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
            // SAFETY: `p` and `surface.handle` are both live — the surface was
            // created from the same instance and has not been destroyed, which is
            // the precondition of every `vkGetPhysicalDeviceSurface*` query — and
            // `idx` is an index into the family array just read, so it names a
            // family this device really has. A query error is treated as "cannot
            // present from this family", which only ever disqualifies a family.
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
        //
        // SAFETY: same live `p`, live surface handle and live surface loader as
        // the support query above. The two `?`s are the point of the check: a
        // surface that cannot answer is a surface no swapchain can be built on.
        let formats = unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(p, surface.handle)
        }?;
        // SAFETY: as above.
        let modes = unsafe {
            surface
                .loader
                .get_physical_device_surface_present_modes(p, surface.handle)
        }?;
        if formats.is_empty() || modes.is_empty() {
            return Ok(None);
        }

        // SAFETY: `p` is a physical device of the live `instance`; the returned
        // struct is read immediately and kept only as a `device_type`.
        let props = unsafe { instance.get_physical_device_properties(p) };
        let score = score_device_type(props.device_type);
        Ok(Some((graphics_family, present_family, score)))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is the `VkDevice` this struct created and
        // destroyed nowhere else. Every object that is a child of it has already
        // been released by the time this runs — `Vulkan`'s field order puts
        // `swapchain` (which holds an `ash::Device` for its image views) before
        // `device`, and `Vulkan::drop` has already destroyed the command pool,
        // semaphores and fence that are also its children — which is what
        // `vkDestroyDevice` requires of an object that still has children. The
        // instance that created the device outlives it, being declared after it
        // in `Vulkan`. No allocator is passed, matching the NULL the device was
        // created with. A lost device is still legal to destroy: the spec
        // counts every object on it as not in use.
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
