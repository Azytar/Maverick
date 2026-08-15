// maverick-vk/src/error.rs
//
// The crate's single error type. Every fallible step in the Vulkan bootstrap
// maps to one of these variants so callers (today only the smoke test) get a
// specific, human-readable reason instead of an opaque `vk::Result`.

use std::error::Error;
use std::fmt;

use ash::vk;

/// Where in the Vulkan bootstrap a failure occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VkError {
    /// `libvulkan.so.1` could not be loaded (dlopen failed). No Vulkan driver.
    Loader(String),
    /// The Vulkan instance could not be created (layers/extensions/alloc).
    Instance(String),
    /// The window-system surface could not be created.
    Surface(String),
    /// No physical device satisfied the swapchain/graphics/present requirements.
    NoPhysicalDevice,
    /// The logical device could not be created (queue or extension setup).
    Device(String),
    /// Swapchain creation or recreation failed.
    Swapchain(String),
    /// `acquire_next_image` returned a non-success status.
    Acquire(String),
    /// `queue_present` returned a non-success status.
    Present(String),
    /// A requested feature/format/present-mode is not supported.
    Unsupported(String),
    /// Two parts of the setup contradict each other (e.g. families mismatch).
    Incompatible(String),
}

impl fmt::Display for VkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VkError::Loader(s) => write!(f, "vulkan loader: {s}"),
            VkError::Instance(s) => write!(f, "vulkan instance: {s}"),
            VkError::Surface(s) => write!(f, "vulkan surface: {s}"),
            VkError::NoPhysicalDevice => {
                write!(f, "no suitable Vulkan physical device (graphics + present + swapchain)")
            }
            VkError::Device(s) => write!(f, "vulkan device: {s}"),
            VkError::Swapchain(s) => write!(f, "vulkan swapchain: {s}"),
            VkError::Acquire(s) => write!(f, "vulkan acquire: {s}"),
            VkError::Present(s) => write!(f, "vulkan present: {s}"),
            VkError::Unsupported(s) => write!(f, "vulkan unsupported: {s}"),
            VkError::Incompatible(s) => write!(f, "vulkan incompatible: {s}"),
        }
    }
}

impl Error for VkError {}

impl From<ash::LoadingError> for VkError {
    fn from(e: ash::LoadingError) -> Self {
        VkError::Loader(e.to_string())
    }
}

impl From<vk::Result> for VkError {
    fn from(r: vk::Result) -> Self {
        VkError::Unsupported(format!("vulkan returned {r:?}"))
    }
}
