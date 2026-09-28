//! Minimal Vulkan/X11 backend — scaffold, not a compositor.
//!
//! Brings Vulkan up on X11 and presents frames cleared with
//! `vkCmdClearColorImage`. There is no shader, pipeline or render pass, and
//! nothing in the workspace links this crate: the `compositor-vulkan` feature
//! is empty, so the backend is exercised only by its own tests.
//!
//! The owned objects are `instance → surface → device → swapchain` plus a
//! one-shot command buffer and the `image_available` / `render_finished`
//! semaphores and `in_flight` fence. `recreate_swapchain` handles resize;
//! `report`, `extent` and `format` are diagnostics and transfer no ownership.
//!
//! What it does not own: [`SurfaceTarget::xcb_connection`] is a borrowed raw
//! `xcb_connection_t*` and `window` a borrowed XID; both must stay valid until
//! the [`Vulkan`] value is dropped. The X connection itself is the caller's.
//!
//! # Ownership
//!
//! Field declaration order is drop order. [`Vulkan::drop`] first waits for the
//! device to become idle, then explicitly destroys semaphores, fence, and
//! command pool before fields unwind, then `swapchain → device → surface →
//! instance` unwind in that order. What that order has to satisfy is the
//! parent/child rule: a swapchain is a child of the device, and both the
//! surface and the device are children of the instance, so `instance` is
//! declared last. The idle wait is part of teardown because
//! [`Vulkan::acquire_and_present`] returns as soon as a frame is submitted, and
//! the destroys above require that work to have completed.
//! [`instance::Instance`] destroys its debug messenger before `VkInstance`. The
//! fence is created signaled so the first `wait_for_fences` does not block, and
//! `pacing::FrameFence` decides whether that wait is owed at all.
//! [`Vulkan::recreate_swapchain`] waits for the whole device before replacing a
//! swapchain handle, because a present is a separate set of queue operations
//! that the submission's fence does not cover.
//!
//! # Invariants
//!
//! Swapchain parameters come from pure helpers ([`choose_surface_format`],
//! [`choose_present_mode`], [`clamp_extent`], [`choose_image_count`]) that
//! touch no Vulkan handle and are unit-tested without a GPU. Validation layers
//! are enabled only when `MAVERICK_VK_VALIDATION=1` *and* the Khronos layer is
//! installed.
//!
//! # Safety
//!
//! Every Vulkan entry point is `unsafe` FFI through `ash`, because the spec's
//! preconditions are about handle validity, object lifetime, queue
//! synchronization and object destruction order — facts about a driver and a
//! GPU that no amount of Rust typing can establish. Each call site states the
//! precondition it relies on; the ones worth knowing before reading them are:
//!
//! * The caller must keep the instance, device and surface alive for every
//!   submission, and must not use a queue after device destruction.
//! * The caller must supply a live `xcb_connection_t*` for the whole lifetime of
//!   the [`Vulkan`] value — dropping the value counts, since teardown waits on
//!   presentation requests that Vulkan sends over that connection.
//! * Exactly one frame is in flight, serialised by the frame-in-flight fence;
//!   see [`Vulkan::acquire_and_present`] and the `pacing` module.
//! * A `SurfaceTarget` naming a window that is not a live X window, or an
//!   `xcb_connection` from a different connection, is a contract violation the
//!   driver reports as an error at surface creation rather than something this
//!   crate can check.

mod device;
mod error;
mod instance;
mod pacing;
mod surface;
mod swapchain;

pub use device::DeviceReport;
pub use error::VkError;
pub use swapchain::{choose_image_count, choose_present_mode, choose_surface_format, clamp_extent};

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
///
/// Dropping waits for outstanding GPU work to complete before destroying the
/// objects submitted commands may still reference, so a caller may drop the
/// backend with a frame in flight and add no synchronization of its own.
// Field order is the drop order: after the device is idle, semaphores/fence/pool
// are freed explicitly in `Drop` before the fields run, and then `swapchain →
// device → surface → instance` must unwind in that order, so `instance` is
// declared LAST.
pub struct Vulkan {
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    in_flight: pacing::FrameFence,
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
        // SAFETY: `device.handle` is the live `VkDevice` `Device::new` returned
        // one line above and nothing has been destroyed since. `queueFamilyIndex`
        // is `device.graphics_family`, the family of the queue every submission
        // in this crate goes to and therefore of the pool's command buffers,
        // which is what `vkCreateCommandPool` requires of that field: a family
        // the device was not created with is what it forbids. The
        // `RESET_COMMAND_BUFFER` flag is what makes the
        // `begin_command_buffer` below legal on a buffer that is still in the
        // executable state from the previous frame. No allocation callbacks are
        // passed, so no Rust-owned allocator crosses the boundary.
        let command_pool = unsafe { device.handle.create_command_pool(&pool_ci, None) }?;

        let alloc_ci = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // Index instead of `[0]`: a driver that hands back fewer buffers than
        // requested then yields an `Err` rather than a panic.
        // SAFETY: the pool named above was created from the same live device and
        // has not been destroyed, and `alloc_ci` asks for one PRIMARY command
        // buffer from it — `vkAllocateCommandBuffers` requires the pool to be in
        // the reset state, which it has never left, because this constructor
        // allocates from it exactly once. `level` is a real level rather than
        // `VK_COMMAND_BUFFER_LEVEL_INVALID`, so `pAllocateCallbacks` must be NULL,
        // and it is.
        let command_buffer = unsafe { device.handle.allocate_command_buffers(&alloc_ci) }?
            .into_iter()
            .next()
            .ok_or_else(|| VkError::Device("no command buffers allocated".into()))?;

        let sem_ci = vk::SemaphoreCreateInfo::default();
        // SAFETY: for each of the two semaphores — the device is the live one
        // from `Device::new`, and both are newly created binary semaphores with
        // no allocator, so neither is in use by a pending operation and the NULL
        // `pAllocator` is compatible with the NULL callbacks the device itself
        // was created with. The two handles stay in distinct fields so the frame
        // loop cannot use one where the other belongs: `image_available` is only
        // ever the acquire's signal and the submit's wait, `render_finished` only
        // ever the submit's signal and the present's wait.
        let image_available = unsafe { device.handle.create_semaphore(&sem_ci, None) }?;
        let render_finished = unsafe { device.handle.create_semaphore(&sem_ci, None) }?;

        // FENCE_CREATE_SIGNALED_BIT: the first `wait_for_fences` must not block
        // forever waiting on a fence that was never signaled.
        let fence_ci = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: the device is the live one from `Device::new` and this is the
        // crate's only fence, freshly created with no allocator, so nothing is
        // using it and no callback is required. The `SIGNALED` flag is
        // what makes the frame-in-flight bookkeeping start out owing no wait;
        // `pacing::FrameFence` owns the handle from here and is the only thing
        // that passes it to a wait, a reset or a submit.
        let in_flight =
            pacing::FrameFence::new(unsafe { device.handle.create_fence(&fence_ci, None) }?);

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
    ///
    /// Returns once the frame is submitted, not once it has completed: the
    /// caller is given no completion signal, and the only wait is the one the
    /// next call or the teardown performs.
    ///
    /// # Frames in flight
    ///
    /// Exactly one. The command buffer, the pair of semaphores and the
    /// frame-in-flight fence are single objects rather than a ring, and the wait
    /// at the top of this function is what serialises one frame against the
    /// next: no frame records, submits or presents until the previous frame's
    /// submission has completed. So `current_image` is an index into this
    /// swapchain's images and is never used to index a resource ring, and
    /// `pacing::FrameFence` is why the wait is issued at all — see that module
    /// for the failure it prevents.
    ///
    /// The one thing a submit-side wait cannot cover is the present, which runs
    /// on the present queue. When that is the same queue — the case on every
    /// device where one family serves both graphics and present, which is what
    /// `device::Device::rate` selects for — the spec's rule that queue
    /// operations complete in issue order puts this frame's present ahead of the
    /// next frame's submit, so `render_finished` is always free by the time it
    /// is signalled again. When the two families differ, that ordering does not
    /// exist, and the extra wait at the end of this function supplies what it
    /// does not.
    pub fn acquire_and_present(&mut self, clear: [f32; 4]) -> Result<(), VkError> {
        let dev = &self.device.handle;
        let fence = self.in_flight.handle();

        // Only wait when a submission is actually outstanding. Resetting the
        // fence before the acquire — which is what this used to do — leaves it
        // unsignalled on every path that returns early, and since the timeout
        // below is `u64::MAX` and a fence cannot be re-signalled by the host,
        // the next frame would block on a wait nothing can satisfy.
        if self.in_flight.take_completion() {
            // SAFETY: the fence is the one this crate created in `Vulkan::new`
            // from the same live device, it has not been destroyed (`Drop` does
            // that, and this method cannot run during a drop), and
            // `take_completion` returned `true` only for a fence handed to a
            // `vkQueueSubmit` that succeeded, so a signal is coming. The `true`
            // is `waitAll`, which is what a one-element list wants. The
            // previous frame's submission is the only work that can hold this
            // fence, and waiting for it is what makes the command buffer below
            // leave the pending state and the acquired image reusable — see the
            // module docs of `pacing` for why a wait with nothing behind it
            // would never return.
            unsafe { dev.wait_for_fences(&[fence], true, u64::MAX) }
                .map_err(|e| VkError::Acquire(e.to_string()))?;
        }

        // SAFETY: the swapchain is the live one from `Swapchain::new` and is
        // replaced only by `recreate_swapchain`, which destroys the old handle
        // itself, so this handle cannot be one that was already destroyed.
        // `image_available` is a live semaphore of the same device that is not
        // in use by any pending operation: the wait above covered the previous
        // frame's submit, which is what consumed the previous acquire's signal,
        // so this acquire is the only outstanding one on it. The fence argument
        // is null, which is allowed because the submit below is what this crate
        // waits on. `u64::MAX` is the "no timeout" setting, so this blocks the
        // host until the presentation engine releases an image — which is the
        // point, and the reason a caller must not be holding a fence wait
        // across it. A surface the window server has withdrawn reports
        // `ERROR_SURFACE_LOST_KHR` or `ERROR_OUT_OF_DATE_KHR` here rather than
        // waiting, so a lost X connection surfaces as an error and not as a
        // block.
        let (idx, _) = unsafe {
            self.device.swapchain_loader.acquire_next_image(
                self.swapchain.handle,
                u64::MAX,
                self.image_available,
                vk::Fence::null(),
            )
        }
        .map_err(|e| VkError::Acquire(e.to_string()))?;
        self.current_image = idx;

        // The driver names any live image; a hostile/buggy driver could
        // hand back an out-of-range index — `get` turns that into an
        // error instead of a panic.
        let image =
            *self.swapchain.images.get(idx as usize).ok_or_else(|| {
                VkError::Acquire(format!("swapchain image index {idx} out of range"))
            })?;

        // SAFETY: `command_buffer` is the buffer allocated from this crate's
        // command pool in `Vulkan::new`, which was created for
        // `device.graphics_family` — the family of the queue it is submitted to
        // (VUID-vkQueueSubmit-pCommandBuffers-00074). The wait above means it is
        // not in the pending state, which is what `vkBeginCommandBuffer` requires
        // of a buffer that is not `VK_COMMAND_BUFFER_LEVEL_INVALID`, and the
        // pool's `RESET_COMMAND_BUFFER` flag is what permits recording into a
        // buffer that is already executable. `ONE_TIME_SUBMIT` matches the
        // single use the frame loop gives it.
        unsafe {
            dev.begin_command_buffer(
                self.command_buffer,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
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

        // SAFETY: `image` is a swapchain image this frame's acquire returned and
        // the barrier above left in `TRANSFER_DST_OPTIMAL`; it is in the command
        // buffer's executable state, so recording is legal. The clear needs a
        // colour aspect, one mip level and one array layer, which is the range
        // the barrier used too, and the swapchain was created with
        // `VK_IMAGE_USAGE_TRANSFER_DST_BIT`, which is the usage bit the command
        // requires of a colour image and the one the swapchain create info asks
        // for. No render pass is in scope, so no attachment bound here needs one.
        unsafe {
            let color = vk::ClearColorValue { float32: clear };
            dev.cmd_clear_color_image(
                self.command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &color,
                &[color_subresource_range()],
            );
        }

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

        // SAFETY: the command buffer is in the recording state from
        // `begin_command_buffer` above, which is the only state
        // `vkEndCommandBuffer` accepts, and no submission has been made for it
        // since, because every path out of the last few lines returns before the
        // submit. Closing it here hands the recorded layout transitions and the
        // clear to the driver; the image is left in `PRESENT_SRC_KHR`, the only
        // layout `vkQueuePresentKHR` accepts.
        unsafe { dev.end_command_buffer(self.command_buffer) }
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

        // The reset goes here rather than at the top of the frame: it is the last
        // thing that can go wrong before the submit, and leaving the fence
        // unsignalled with nothing to signal it is the mistake `pacing` exists to
        // prevent. Resetting is legal because the wait above — or the absence of
        // an outstanding submission — means no queue operation is using the
        // fence (VUID-vkResetFences-pFences-01123).
        //
        // SAFETY: the fence is this crate's, live on the same device, and not in
        // use by any pending operation, as established above.
        unsafe { dev.reset_fences(&[fence]) }.map_err(|e| VkError::Acquire(e.to_string()))?;

        // SAFETY: `dev` is the live device and `self.device.queue` is its
        // graphics-family queue, both from `Device::new`; the fence is the
        // crate's own, just reset and therefore unsignalled as
        // `VUID-vkQueueSubmit-fence-00063` requires, and not associated with any
        // other unfinished queue command as `VUID-vkQueueSubmit-fence-00064`
        // requires — the wait above is what establishes that. The batch waits on
        // `image_available`, the semaphore this frame's own acquire signalled, so
        // the semaphore signal operation it names has been submitted for
        // execution (VUID-vkQueueSubmit-pWaitSemaphores-03238), and it signals
        // `render_finished`, which `Drop` and `recreate_swapchain` have not
        // destroyed. `wait_dst_stage_mask` names the one stage the recorded work
        // touches, so no wait can be left hanging behind a stage this queue does
        // not reach. The arrays outlive the call.
        unsafe { dev.queue_submit(self.device.queue, &submit, fence) }
            .map_err(|e| VkError::Acquire(e.to_string()))?;
        // Only on the success path: this is the record the next frame's wait is
        // authorised by, and a failed submit enqueued nothing to signal the fence.
        self.in_flight.submitted();

        let swapchains = [self.swapchain.handle];
        let indices = [idx];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&signal_sems)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the present queue is the one `Device::rate` chose by asking
        // `vkGetPhysicalDeviceSurfaceSupportKHR` for this physical device and
        // surface, and one queue was requested for its family at device creation,
        // so presentation is supported from it
        // (VUID-vkQueuePresentKHR-pSwapchains-01292). The swapchain handle is
        // the live one, and `idx` is the image index the acquire above returned
        // for it, which is the same image the command buffer just transitioned to
        // `PRESENT_SRC_KHR`; the present releases that image, which is what makes
        // the next acquire of it legal. `render_finished` was signalled by the
        // submit above, which is the signal operation this wait names
        // (VUID-vkQueuePresentKHR-pWaitSemaphores-03268), and it is a binary
        // semaphore as that VUID requires. The arrays outlive the call.
        unsafe {
            self.device
                .swapchain_loader
                .queue_present(self.device.present_queue, &present_info)
        }
        .map_err(|e| VkError::Present(e.to_string()))?;

        // On a device with a separate present family the present is a queue
        // operation on a *different* queue from the submit, so issue order on the
        // submit queue does not order it against the next frame's submit — and
        // the next frame signals `render_finished` again. This is the wait that
        // closes that window, and it is skipped entirely on the single-family
        // devices where issue order already closes it.
        if self.device.present_family != self.device.graphics_family {
            // SAFETY: `present_queue` is the live queue this crate just issued
            // the present on, obtained from the live device. `vkQueueWaitIdle`
            // returns once every operation enqueued on it has completed, which
            // includes the present's semaphore wait, so `render_finished` is
            // back to its unsignaled initial state before the next frame can
            // signal it (VUID-vkQueueSubmit-pSignalSemaphores-00067). The fence
            // is deliberately not used here: it is signalled by the submit, not
            // by the present, and waiting on it would not cover this.
            unsafe { dev.queue_wait_idle(self.device.present_queue) }
                .map_err(|e| VkError::Present(e.to_string()))?;
        }

        Ok(())
    }

    /// Recreate the swapchain (and image views) for a new size.
    ///
    /// The replaced swapchain is destroyed exactly once, views before handle,
    /// by [`swapchain::Swapchain::destroy`]: it must not be dropped afterwards
    /// as well, or the views and the handle would be destroyed twice.
    pub fn recreate_swapchain(&mut self, w: u32, h: u32) -> Result<(), VkError> {
        // Wait for the in-flight frame so we don't pull the swapchain out from
        // under a submission that still references it.
        //
        // SAFETY: as in `acquire_and_present` — this crate's own live fence on a
        // live device, and `take_completion` reports a wait only for a submission
        // that succeeded and therefore signalled it.
        if self.in_flight.take_completion() {
            unsafe {
                self.device
                    .handle
                    .wait_for_fences(&[self.in_flight.handle()], true, u64::MAX)
            }
            .map_err(|e| VkError::Swapchain(e.to_string()))?;
        }

        // And then wait for the *present*, which that fence does not cover: it is
        // signalled by the submit batch alone, while a present is a separate set
        // of queue operations on the present queue. `vkDestroySwapchainKHR`
        // requires that all uses of the swapchain's images have completed
        // execution (VUID-vkDestroySwapchainKHR-swapchain-01282) and the
        // description spells it out as "the application must not destroy a
        // swapchain until after completion of all outstanding operations on
        // images that were acquired from the swapchain" — which is why the
        // teardown in `Drop` waits on the whole device rather than on this
        // fence, and why the fence above is not enough here.
        //
        // SAFETY: the device is live, and `vkDeviceWaitIdle` needs no other
        // argument to be valid; it waits for every queue of the device, so the
        // submit, the present and the acquire all complete. Its result is
        // deliberately not propagated: a device that cannot be waited on is
        // either lost — in which case the spec counts command buffers as not
        // pending and objects as not in use, so the destroys below are legal
        // either way — or hung, where no shorter wait would be legal.
        unsafe {
            let _ = self.device.handle.device_wait_idle();
        }

        let old_handle = self.swapchain.handle;
        // Build the replacement first: on failure the live swapchain stays
        // installed and usable (only its size is stale).
        let new = swapchain::Swapchain::new(&self.device, &self.surface, w, h, Some(old_handle))?;
        let old = std::mem::replace(&mut self.swapchain, new);
        old.destroy();
        self.current_image = 0;
        Ok(())
    }

    /// Startup diagnostics: chosen GPU name, PCI ids, driver version and type.
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
    // SAFETY: recording a barrier needs the command buffer to be in the recording
    // state and the barrier's parameters to describe the image's real state. The
    // caller is the frame loop: `cmd` is the buffer it began recording a moment
    // earlier and has not submitted, and this function is only ever called with
    // the layout the image is actually in — `UNDEFINED` for a freshly acquired
    // image, which the spec lets a driver discard, and `TRANSFER_DST_OPTIMAL`
    // for the one the clear above wrote. `offset` and `size` are left at their
    // defaults of 0, which the spec reads as "the whole image", and the subresource
    // range is the same single colour mip and array layer the clear and the
    // swapchain image view use. The queue family indices are `VK_QUEUE_FAMILY_IGNORED`,
    // which is the required value whenever the barrier is not part of a queue
    // family ownership transfer — and this one is not, because the swapchain is
    // `EXCLUSIVE` on a single family or `CONCURRENT` on both, never transferred.
    // `device` is the live `VkDevice` the command buffer was allocated from, which
    // is what `vkCmdPipelineBarrier` requires, and `barrier` outlives the call.
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
        // SAFETY for every call below: `dev` is the live `VkDevice` created in
        // `Device::new`, and this is the only place the handles it owns are
        // destroyed, after the wait that makes each destroy legal. No
        // allocation callbacks are passed, matching the NULL callbacks every one
        // of these objects was created with.
        unsafe {
            // `acquire_and_present` returns once the frame is submitted, not
            // once it has run, yet every destroy below requires the submitted
            // work referring to the object to have completed execution — the
            // semaphore, fence, command-pool and swapchain preconditions all
            // say so in their own terms.
            //
            // `device_wait_idle`, not `wait_for_fences` on `in_flight`: that
            // fence is signalled by the `queue_submit` batch alone, so it
            // cannot cover `queue_present` on the present queue — whose signal
            // semaphore the presentation engine may still hold — nor
            // `acquire_next_image`, which signals `image_available` and is
            // given no fence at all.
            //
            // The result is ignored because `Drop` cannot report it, and that
            // is sound: the wait succeeds, or the device is lost, and for a
            // lost device the spec counts command buffers as not pending and
            // objects as not in use, so the destroys are legal either way. A
            // hung rather than lost device can stall this call, but no shorter
            // wait makes the teardown legal.
            let _ = dev.device_wait_idle();

            // The wait above is what makes the four destroys legal, and they are
            // done here rather than in a `Drop` of their own so that the fields
            // they release can be plain `vk` handles. Semaphores and a fence must
            // not be in use by a pending operation, and `vkDestroyCommandPool`
            // requires every command buffer allocated from the pool to be back
            // outside the pending state — which it is, whether the frame
            // submitted or not. Semaphores first: both are released by the
            // destroy itself, and neither is referenced after this point.
            dev.destroy_semaphore(self.image_available, None);
            dev.destroy_semaphore(self.render_finished, None);
            dev.destroy_fence(self.in_flight.handle(), None);
            dev.destroy_command_pool(self.command_pool, None);
        }
        // The remaining objects are released by the fields that own them, as
        // those fields unwind. That order is `swapchain → device → surface →
        // instance`, and what has to hold for it is the parent/child rule rather
        // than a fixed sequence: a swapchain is a child of the device, so it is
        // destroyed first; a surface and a device are both children of the
        // instance, so both precede it. (The spec's own shutdown recipe puts the
        // surface before the device, which is a recommendation for closing a
        // window; destroying the device first does not invalidate an
        // instance-level object, and the two are independent children here.)
        // The wait above already covers the images and the presentation requests
        // the swapchain's destruction releases, and `recreate_swapchain` waits
        // the same way before it destroys a swapchain it replaces.
    }
}
