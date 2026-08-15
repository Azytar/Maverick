// maverick-vk/src/lib.rs
//
// Public surface of the crate: [`SurfaceTarget`] (a raw xcb handle bundle the
// caller owns) and [`Vulkan`], the minimal bootstrap + clear/present loop. There
// is deliberately no shader, no pipeline and no render pass: this phase only
// proves the backend can bring up Vulkan on X11 and present cleared frames using
// `vkCmdClearColorImage`. Window/compositor integration is a later phase; this
// crate is not referenced by `maverick` or `maverick-gl`.

mod device;
mod error;
mod instance;
mod surface;
mod swapchain;

pub use device::DeviceReport;
pub use error::VkError;
pub use swapchain::{
    choose_image_count, choose_present_mode, choose_surface_format, clamp_extent,
};

use std::os::raw::c_void;

use ash::vk;

/// Everything `Vulkan` needs to anchor a surface to a window, owned and kept
/// alive by the caller. The `xcb_connection` pointer must be a live
/// `xcb_connection_t*` that outlives `Vulkan`; `window` must be a real X window.
pub struct SurfaceTarget {
    pub xcb_connection: *mut c_void,
    pub window: u32,
    pub width: u32,
    pub height: u32,
}

/// Minimal Vulkan/X11 backend: instance → surface → device → swapchain plus the
/// one-shot command buffer and synchronization objects used to clear and present
/// a single frame.
// Field order is the drop order: semaphores/fence/pool are freed explicitly in
// `Drop` before the fields run, and then `swapchain → device → surface →
// instance` must unwind in that order, so `instance` is declared LAST.
pub struct Vulkan {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    in_flight: vk::Fence,
    current_image: u32,
    swapchain: swapchain::Swapchain,
    device: device::Device,
    surface: surface::Surface,
    // Never read after construction except by `Drop`; kept alive for ordering.
    #[allow(dead_code)]
    instance: instance::Instance,
}

impl Vulkan {
    /// Bring up the whole backend for `target`.
    ///
    /// Validation layers are enabled only when `MAVERICK_VK_VALIDATION=1` *and*
    /// the Khronos validation layer is installed.
    pub fn new(target: SurfaceTarget) -> Result<Self, VkError> {
        let validation = std::env::var("MAVERICK_VK_VALIDATION").as_deref() == Ok("1");

        let instance = instance::Instance::new(validation)?;
        let surface = surface::Surface::new(
            instance.entry(),
            instance.handle(),
            target.xcb_connection,
            target.window,
        )?;
        let device = device::Device::new(instance.handle(), &surface)?;
        let swapchain =
            swapchain::Swapchain::new(&device, &surface, target.width, target.height, None)?;

        // Command pool: transient (used once per frame) and resettable.
        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(device.graphics_family)
            .flags(
                vk::CommandPoolCreateFlags::TRANSIENT
                    | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            );
        let command_pool =
            unsafe { device.handle.create_command_pool(&pool_ci, None) }?;

        let alloc_ci = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer =
            unsafe { device.handle.allocate_command_buffers(&alloc_ci) }?[0];

        let sem_ci = vk::SemaphoreCreateInfo::default();
        let image_available =
            unsafe { device.handle.create_semaphore(&sem_ci, None) }?;
        let render_finished =
            unsafe { device.handle.create_semaphore(&sem_ci, None) }?;

        // FENCE_CREATE_SIGNALED_BIT: the first `wait_for_fences` must not block
        // forever waiting on a fence that was never signaled.
        let fence_ci = vk::FenceCreateInfo::default()
            .flags(vk::FenceCreateFlags::SIGNALED);
        let in_flight = unsafe { device.handle.create_fence(&fence_ci, None) }?;

        Ok(Self {
            instance,
            surface,
            device,
            swapchain,
            command_pool,
            command_buffer,
            image_available,
            render_finished,
            in_flight,
            current_image: 0,
        })
    }

    /// Acquire the next swapchain image, clear it to `clear`, and present it.
    pub fn acquire_and_present(&mut self, clear: [f32; 4]) -> Result<(), VkError> {
        let dev = &self.device.handle;

        unsafe {
            dev.wait_for_fences(&[self.in_flight], true, u64::MAX)
                .map_err(|e| VkError::Acquire(e.to_string()))?;
            dev.reset_fences(&[self.in_flight])
                .map_err(|e| VkError::Acquire(e.to_string()))?;

            let (idx, _) = self
                .device
                .swapchain_loader
                .acquire_next_image(
                    self.swapchain.handle,
                    u64::MAX,
                    self.image_available,
                    vk::Fence::null(),
                )
                .map_err(|e| VkError::Acquire(e.to_string()))?;
            self.current_image = idx;

            let image = self.swapchain.images[idx as usize];

            dev.begin_command_buffer(
                self.command_buffer,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(|e| VkError::Acquire(e.to_string()))?;

            // UNDEFINED -> TRANSFER_DST_OPTIMAL so we can clear.
            transition_image_layout(
                dev,
                self.command_buffer,
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::AccessFlags::empty(),
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::TRANSFER_WRITE,
            );

            let color = vk::ClearColorValue { float32: clear };
            dev.cmd_clear_color_image(
                self.command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &color,
                &[color_subresource_range()],
            );

            // TRANSFER_DST_OPTIMAL -> PRESENT_SRC_KHR for the present engine.
            transition_image_layout(
                dev,
                self.command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::AccessFlags::MEMORY_READ,
            );

            dev.end_command_buffer(self.command_buffer)
                .map_err(|e| VkError::Acquire(e.to_string()))?;

            let wait_sems = [self.image_available];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let signal_sems = [self.render_finished];
            let cmd_bufs = [self.command_buffer];
            let submit = [vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmd_bufs)
                .signal_semaphores(&signal_sems)];
            dev.queue_submit(self.device.queue, &submit, self.in_flight)
                .map_err(|e| VkError::Acquire(e.to_string()))?;

            let swapchains = [self.swapchain.handle];
            let indices = [idx];
            let present_info = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal_sems)
                .swapchains(&swapchains)
                .image_indices(&indices);
            self.device
                .swapchain_loader
                .queue_present(self.device.present_queue, &present_info)
                .map_err(|e| VkError::Present(e.to_string()))?;
        }

        Ok(())
    }

    /// Recreate the swapchain (and image views) for a new size. The previous
    /// swapchain handle is destroyed as part of this call.
    pub fn recreate_swapchain(&mut self, w: u32, h: u32) -> Result<(), VkError> {
        // Wait for the in-flight frame so we don't pull the swapchain out from
        // under a submission that still references it.
        unsafe {
            self.device
                .handle
                .wait_for_fences(&[self.in_flight], true, u64::MAX)
                .map_err(|e| VkError::Swapchain(e.to_string()))?;
        }

        let old = self.swapchain.handle;
        let new = swapchain::Swapchain::new(&self.device, &self.surface, w, h, Some(old))?;
        // `new` already replaced `old` inside the create info; destroy old now.
        unsafe {
            self.swapchain.loader.destroy_swapchain(old, None);
        }
        self.swapchain = new;
        self.current_image = 0;
        Ok(())
    }

    /// Diagnostic snapshot of the chosen GPU and the swapchain format.
    pub fn report(&self) -> &DeviceReport {
        &self.device.report
    }

    /// The swapchain's current pixel extent.
    pub fn extent(&self) -> vk::Extent2D {
        self.swapchain.extent
    }

    /// The swapchain's current image format.
    pub fn format(&self) -> vk::Format {
        self.swapchain.format
    }
}

fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn transition_image_layout(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags,
    src_access: vk::AccessFlags,
    dst_stage: vk::PipelineStageFlags,
    dst_access: vk::AccessFlags,
) {
    let barrier = vk::ImageMemoryBarrier {
        src_access_mask: src_access,
        dst_access_mask: dst_access,
        old_layout,
        new_layout,
        src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        image,
        subresource_range: color_subresource_range(),
        ..Default::default()
    };
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            src_stage,
            dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

impl Drop for Vulkan {
    fn drop(&mut self) {
        let dev = &self.device.handle;
        unsafe {
            // Order matters: semaphores → fence → command pool → swapchain →
            // surface → device → (instance drops debug messenger + instance).
            dev.destroy_semaphore(self.image_available, None);
            dev.destroy_semaphore(self.render_finished, None);
            dev.destroy_fence(self.in_flight, None);
            dev.destroy_command_pool(self.command_pool, None);
        }
        // `self.swapchain`, `self.surface`, `self.device` and `self.instance`
        // drop (in that field order) here, each cleaning up its own Vulkan
        // object in the correct order.
    }
}
