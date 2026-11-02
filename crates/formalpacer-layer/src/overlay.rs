//! The overlay in the Vulkan layer. The picture is copied into the top-left of each presented
//! swapchain image with `vkCmdCopyBufferToImage`, so it needs no shaders, render passes or
//! blending. Every image gets its own staging buffer, command buffer, fence and semaphore: the
//! copy waits on the app's semaphores and the present waits on the copy's.
//!
//! Anything that fails leaves that swapchain without the overlay. The app never sees an error.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::Mutex;

use ash::vk::{self, Handle};
use formalpacer_hook::{overlay, pacer};

use crate::loader::SetDeviceLoaderData;

/// The next layer's entry point for `$name`, as `$ty`.
macro_rules! next_fn {
    ($proc:expr, $handle:expr, $name:expr, $ty:ty) => {
        // SAFETY: the next layer hands back the entry point for the name asked for, of that type.
        unsafe { $proc($handle, $name.as_ptr()) }
            .map(|f| unsafe { std::mem::transmute::<unsafe extern "system" fn(), $ty>(f) })
    };
}

/// What the layer keeps per device to draw the overlay.
pub struct Device {
    fns: ash::Device,
    swapchain_fns: ash::khr::swapchain::DeviceFn,
    get_queue2: Option<vk::PFN_vkGetDeviceQueue2>,
    set_loader_data: Option<SetDeviceLoaderData>,
    physical: vk::PhysicalDevice,
    surface_caps: Option<vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR>,
    memory: vk::PhysicalDeviceMemoryProperties,
    families: Vec<vk::QueueFlags>,
    queues: Mutex<HashMap<vk::Queue, u32>>,
    swapchains: Mutex<HashMap<vk::SwapchainKHR, Swapchain>>,
}

struct Swapchain {
    format: vk::Format,
    extent: vk::Extent2D,
    /// Whether the overlay can draw into this swapchain's images at all.
    drawable: bool,
    images: Vec<vk::Image>,
    /// One per image, made on its first overlay frame. Empty while a present has it out.
    slots: Vec<Option<Slot>>,
}

/// What it takes to draw into one swapchain image.
struct Slot {
    family: u32,
    size: u64,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Whether `fence` has a submission behind it; an unsubmitted fence would never signal.
    pending: bool,
    semaphore: vk::Semaphore,
    /// The overlay picture the buffer and command buffer hold, 0 for none.
    generation: u64,
}

// SAFETY: `mapped` points into memory the slot owns; a slot is only used by one thread at a time,
// moved in and out of its swapchain under the device's lock.
unsafe impl Send for Slot {}

/// The image a present shows, and its slot while the overlay is drawn into it.
struct Target {
    image: vk::Image,
    format: vk::Format,
    extent: vk::Extent2D,
    slot: Option<Slot>,
}

/// `rgba` as one texel of `format`, for the formats the overlay draws into. These are all four
/// bytes; the values are sRGB already, so UNORM and SRGB formats take the same bytes.
fn texel(format: vk::Format, [r, g, b, a]: [u8; 4]) -> Option<[u8; 4]> {
    let ten = |v: u8| (u32::from(v) * 1023 + 127) / 255;
    let two = (u32::from(a) * 3 + 127) / 255;
    match format {
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => Some([b, g, r, a]),
        vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => Some([r, g, b, a]),
        vk::Format::A2R10G10B10_UNORM_PACK32 => Some((two << 30 | ten(r) << 20 | ten(g) << 10 | ten(b)).to_ne_bytes()),
        vk::Format::A2B10G10R10_UNORM_PACK32 => Some((two << 30 | ten(b) << 20 | ten(g) << 10 | ten(r)).to_ne_bytes()),
        _ => None,
    }
}

impl Device {
    /// # Safety
    /// `device` was just created from `physical`, an instance of `instance`; `gipa` and `gdpa`
    /// are the next layer's.
    pub unsafe fn new(
        instance: vk::Instance,
        gipa: vk::PFN_vkGetInstanceProcAddr,
        physical: vk::PhysicalDevice,
        device: vk::Device,
        gdpa: vk::PFN_vkGetDeviceProcAddr,
        set_loader_data: Option<SetDeviceLoaderData>,
    ) -> Self {
        let load = |name: &CStr| {
            // SAFETY: forwarding to the next layer.
            unsafe { gdpa(device, name.as_ptr()) }.map_or(std::ptr::null(), |f| f as *const c_void)
        };
        let none = |_: &CStr| std::ptr::null();
        // Only core 1.0 is loaded; ash stubs the rest, which the overlay never calls.
        let fns = ash::Device::from_parts_1_3(
            device,
            ash::DeviceFnV1_0::load(load),
            ash::DeviceFnV1_1::load(none),
            ash::DeviceFnV1_2::load(none),
            ash::DeviceFnV1_3::load(none),
        );
        let mut memory = vk::PhysicalDeviceMemoryProperties::default();
        if let Some(f) = next_fn!(
            gipa,
            instance,
            c"vkGetPhysicalDeviceMemoryProperties",
            vk::PFN_vkGetPhysicalDeviceMemoryProperties
        ) {
            // SAFETY: `physical` belongs to `instance`.
            unsafe { f(physical, &mut memory) };
        }
        let mut families = Vec::new();
        if let Some(f) = next_fn!(
            gipa,
            instance,
            c"vkGetPhysicalDeviceQueueFamilyProperties",
            vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties
        ) {
            let mut count = 0;
            // SAFETY: the usual two calls, the second with room for `count` entries.
            unsafe {
                f(physical, &mut count, std::ptr::null_mut());
                let mut props = vec![vk::QueueFamilyProperties::default(); count as usize];
                f(physical, &mut count, props.as_mut_ptr());
                props.truncate(count as usize);
                families = props.iter().map(|p| p.queue_flags).collect();
            }
        }
        Self {
            fns,
            swapchain_fns: ash::khr::swapchain::DeviceFn::load(load),
            get_queue2: next_fn!(gdpa, device, c"vkGetDeviceQueue2", vk::PFN_vkGetDeviceQueue2),
            set_loader_data,
            physical,
            surface_caps: next_fn!(
                gipa,
                instance,
                c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
                vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR
            ),
            memory,
            families,
            queues: Mutex::default(),
            swapchains: Mutex::default(),
        }
    }

    fn note_queue(&self, queue: vk::Queue, family: u32) {
        if let Ok(mut queues) = self.queues.lock() {
            queues.insert(queue, family);
        }
    }

    /// # Safety
    /// The app's `vkGetDeviceQueue` arguments.
    pub unsafe fn get_queue(&self, family: u32, index: u32, out: *mut vk::Queue) {
        // SAFETY: forwarding the app's own call to the next layer.
        unsafe {
            (self.fns.fp_v1_0().get_device_queue)(self.fns.handle(), family, index, out);
            self.note_queue(*out, family);
        }
    }

    /// # Safety
    /// The app's `vkGetDeviceQueue2` arguments.
    pub unsafe fn get_queue2(&self, info: *const vk::DeviceQueueInfo2<'_>, out: *mut vk::Queue) {
        let Some(next) = self.get_queue2 else { return };
        // SAFETY: forwarding the app's own call to the next layer.
        unsafe {
            next(self.fns.handle(), info, out);
            self.note_queue(*out, (*info).queue_family_index);
        }
    }

    /// Creates the swapchain with TRANSFER_DST added to its usage when the overlay can draw into
    /// it.
    ///
    /// # Safety
    /// The app's `vkCreateSwapchainKHR` arguments.
    pub unsafe fn create_swapchain(
        &self,
        info: *const vk::SwapchainCreateInfoKHR<'_>,
        alloc: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::SwapchainKHR,
    ) -> vk::Result {
        // SAFETY: the app passes a valid create info.
        let mut ours = unsafe { *info };
        let mut caps = vk::SurfaceCapabilitiesKHR::default();
        let drawable = texel(ours.image_format, [0; 4]).is_some()
            && ours.image_color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            && !ours.flags.contains(vk::SwapchainCreateFlagsKHR::PROTECTED)
            // SAFETY: the surface is the app's, on this device's physical device.
            && self.surface_caps.is_some_and(|f| unsafe { f(self.physical, ours.surface, &mut caps) } == vk::Result::SUCCESS)
            && caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST);
        if drawable {
            ours.image_usage |= vk::ImageUsageFlags::TRANSFER_DST;
        }
        let device = self.fns.handle();
        // SAFETY: the app's call with its own create info copied, pNext chain included.
        let result = unsafe { (self.swapchain_fns.create_swapchain_khr)(device, &ours, alloc, out) };
        if result != vk::Result::SUCCESS {
            return result;
        }
        // SAFETY: the call succeeded, so `out` holds the new swapchain.
        let swapchain = unsafe { *out };
        let mut count = 0;
        let get_images = self.swapchain_fns.get_swapchain_images_khr;
        // SAFETY: the usual two calls, the second with room for `count` images.
        let images = unsafe {
            let mut images = Vec::new();
            if get_images(device, swapchain, &mut count, std::ptr::null_mut()) == vk::Result::SUCCESS {
                images.resize(count as usize, vk::Image::null());
                if get_images(device, swapchain, &mut count, images.as_mut_ptr()) != vk::Result::SUCCESS {
                    images.clear();
                }
                images.truncate(count as usize);
            }
            images
        };
        pacer::note_extent(ours.image_extent.width, ours.image_extent.height);
        if let Ok(mut swapchains) = self.swapchains.lock() {
            swapchains.insert(
                swapchain,
                Swapchain {
                    format: ours.image_format,
                    extent: ours.image_extent,
                    drawable: drawable && !images.is_empty(),
                    slots: images.iter().map(|_| None).collect(),
                    images,
                },
            );
        }
        result
    }

    /// # Safety
    /// The app's `vkDestroySwapchainKHR` arguments.
    pub unsafe fn destroy_swapchain(&self, swapchain: vk::SwapchainKHR, alloc: *const vk::AllocationCallbacks<'_>) {
        let removed = self.swapchains.lock().ok().and_then(|mut m| m.remove(&swapchain));
        for slot in removed.into_iter().flat_map(|s| s.slots).flatten() {
            self.free(slot);
        }
        // SAFETY: forwarding the app's own call to the next layer.
        unsafe { (self.swapchain_fns.destroy_swapchain_khr)(self.fns.handle(), swapchain, alloc) };
    }

    /// Frees what is left before the device goes: slots of swapchains the app never destroyed.
    pub fn destroy(&self) {
        let left = self.swapchains.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default();
        for slot in left.into_values().flat_map(|s| s.slots).flatten() {
            self.free(slot);
        }
    }

    /// Draws the overlay into every image of the present it can. Returns the semaphores the
    /// present must wait on in place of the app's, which the copies have already waited on;
    /// empty when nothing was drawn and the present goes ahead as the app asked.
    ///
    /// # Safety
    /// The app's `vkQueuePresentKHR` arguments.
    pub unsafe fn draw(&self, queue: vk::Queue, info: &vk::PresentInfoKHR<'_>) -> Vec<vk::Semaphore> {
        let mut waits = Vec::new();
        let Some(family) = self.queues.lock().ok().and_then(|m| m.get(&queue).copied()) else {
            return waits;
        };
        let copies = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER;
        if !self.families.get(family as usize).is_some_and(|f| f.intersects(copies)) {
            return waits;
        }
        let count = info.swapchain_count as usize;
        // SAFETY: the app's arrays hold `swapchainCount` and `waitSemaphoreCount` entries.
        let (swapchains, indices, app_waits) = unsafe {
            (
                std::slice::from_raw_parts(info.p_swapchains, count),
                std::slice::from_raw_parts(info.p_image_indices, count),
                if info.wait_semaphore_count == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(info.p_wait_semaphores, info.wait_semaphore_count as usize)
                },
            )
        };
        for (&swapchain, &index) in swapchains.iter().zip(indices) {
            let Some(mut target) = self.take(swapchain, index) else { continue };
            // The first copy to go through consumes the app's semaphores, and with them the
            // wait for every image in this present.
            let app = if waits.is_empty() { app_waits } else { &[] };
            if let Some(semaphore) = self.draw_into(queue, family, &mut target, app) {
                waits.push(semaphore);
            }
            self.put_back(swapchain, index, target.slot);
        }
        waits
    }

    fn take(&self, swapchain: vk::SwapchainKHR, index: u32) -> Option<Target> {
        let mut swapchains = self.swapchains.lock().ok()?;
        let s = swapchains.get_mut(&swapchain).filter(|s| s.drawable)?;
        let image = *s.images.get(index as usize)?;
        let slot = s.slots.get_mut(index as usize)?.take();
        Some(Target { image, format: s.format, extent: s.extent, slot })
    }

    fn put_back(&self, swapchain: vk::SwapchainKHR, index: u32, slot: Option<Slot>) {
        let slot = match self.swapchains.lock() {
            Ok(mut swapchains) => match swapchains.get_mut(&swapchain).and_then(|s| s.slots.get_mut(index as usize)) {
                Some(entry) => {
                    *entry = slot;
                    return;
                }
                None => slot,
            },
            Err(_) => slot,
        };
        if let Some(slot) = slot {
            self.free(slot);
        }
    }

    /// Records and submits the copy for one image, returning the semaphore it signals.
    fn draw_into(
        &self,
        queue: vk::Queue,
        family: u32,
        target: &mut Target,
        app_waits: &[vk::Semaphore],
    ) -> Option<vk::Semaphore> {
        let frame = pacer::overlay(target.extent.height)?;
        let bitmap = &frame.bitmap;
        let origin = overlay::MARGIN * bitmap.scale;
        if origin >= target.extent.width || origin >= target.extent.height {
            return None;
        }
        let size = u64::from(bitmap.width) * u64::from(bitmap.height) * 4;
        if let Some(slot) = target.slot.take_if(|s| s.family != family || s.size < size) {
            self.free(slot);
        }
        if target.slot.is_none() {
            // Room to spare, so a wider line of text does not mean a new buffer.
            target.slot = self.create_slot(family, size.next_power_of_two());
        }
        let slot = target.slot.as_mut()?;
        let d = &self.fns;
        if slot.pending {
            // SAFETY: the fence is this slot's and has a submission behind it.
            unsafe {
                d.wait_for_fences(&[slot.fence], true, u64::MAX).ok()?;
                d.reset_fences(&[slot.fence]).ok()?;
            }
            slot.pending = false;
        }
        if slot.generation != frame.generation {
            slot.generation = 0;
            let palette = overlay::COLORS.map(|c| texel(target.format, c).unwrap_or_default());
            // SAFETY: the mapping holds `slot.size` bytes, at least the bitmap's four per pixel,
            // and the GPU is done reading it since the fence signalled.
            let out = unsafe { std::slice::from_raw_parts_mut(slot.mapped, size as usize) };
            for (texel, &value) in out.chunks_exact_mut(4).zip(&bitmap.pixels) {
                texel.copy_from_slice(&palette[usize::from(value).min(palette.len() - 1)]);
            }
            let region = vk::Extent3D {
                width: bitmap.width.min(target.extent.width - origin),
                height: bitmap.height.min(target.extent.height - origin),
                depth: 1,
            };
            // SAFETY: the command buffer is not pending, its pool can reset it, and every
            // handle recorded belongs to this device.
            unsafe { self.record(slot, target.image, origin, region, bitmap.width, bitmap.height) }.ok()?;
            slot.generation = frame.generation;
        }
        let stages = vec![vk::PipelineStageFlags::TRANSFER; app_waits.len()];
        let cmds = [slot.cmd];
        let signal = [slot.semaphore];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(app_waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signal);
        // SAFETY: the app holds the queue for the length of its present. The semaphore was last
        // waited on by this image's previous present, which is done since the image was
        // acquired again.
        unsafe { d.queue_submit(queue, &[submit], slot.fence) }.ok()?;
        slot.pending = true;
        Some(slot.semaphore)
    }

    /// # Safety
    /// See [`Self::draw_into`].
    unsafe fn record(
        &self,
        slot: &Slot,
        image: vk::Image,
        origin: u32,
        region: vk::Extent3D,
        row: u32,
        rows: u32,
    ) -> ash::prelude::VkResult<()> {
        let d = &self.fns;
        let range =
            vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        let barrier = |old, new, src, dst| {
            vk::ImageMemoryBarrier::default()
                .src_access_mask(src)
                .dst_access_mask(dst)
                .old_layout(old)
                .new_layout(new)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
        };
        let copy = vk::BufferImageCopy::default()
            .buffer_row_length(row)
            .buffer_image_height(rows)
            .image_subresource(
                vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1),
            )
            .image_offset(vk::Offset3D { x: origin as i32, y: origin as i32, z: 0 })
            .image_extent(region);
        // SAFETY: as the caller promises.
        unsafe {
            d.begin_command_buffer(slot.cmd, &vk::CommandBufferBeginInfo::default())?;
            // ALL_COMMANDS also orders the copy after the app's rendering when it presents
            // without semaphores and relies on queue order.
            d.cmd_pipeline_barrier(
                slot.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::MEMORY_WRITE,
                    vk::AccessFlags::TRANSFER_WRITE,
                )],
            );
            d.cmd_copy_buffer_to_image(slot.cmd, slot.buffer, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy]);
            d.cmd_pipeline_barrier(
                slot.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::empty(),
                )],
            );
            d.end_command_buffer(slot.cmd)
        }
    }

    fn create_slot(&self, family: u32, size: u64) -> Option<Slot> {
        let mut slot = Slot {
            family,
            size,
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            mapped: std::ptr::null_mut(),
            pool: vk::CommandPool::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            pending: false,
            semaphore: vk::Semaphore::null(),
            generation: 0,
        };
        if self.fill_slot(&mut slot).is_none() {
            self.free(slot);
            return None;
        }
        Some(slot)
    }

    fn fill_slot(&self, slot: &mut Slot) -> Option<()> {
        let d = &self.fns;
        let set_loader_data = self.set_loader_data?;
        // SAFETY: plain object creation on this device; each handle lands in the slot as soon as
        // it exists, so `free` cleans up after a failure part way.
        unsafe {
            slot.buffer = d
                .create_buffer(
                    &vk::BufferCreateInfo::default().size(slot.size).usage(vk::BufferUsageFlags::TRANSFER_SRC),
                    None,
                )
                .ok()?;
            let needs = d.get_buffer_memory_requirements(slot.buffer);
            let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let types = &self.memory.memory_types[..self.memory.memory_type_count as usize];
            let kind = (0..)
                .zip(types)
                .find(|(i, t)| needs.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(host))?
                .0;
            slot.memory = d
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default().allocation_size(needs.size).memory_type_index(kind),
                    None,
                )
                .ok()?;
            d.bind_buffer_memory(slot.buffer, slot.memory, 0).ok()?;
            slot.mapped = d.map_memory(slot.memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()).ok()?.cast();
            slot.pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(slot.family),
                    None,
                )
                .ok()?;
            slot.cmd = *d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(slot.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .ok()?
                .first()?;
            // The loader's trampolines dispatch through the first word of the handle, which only
            // the loader can fill in for objects a layer creates.
            if set_loader_data(d.handle(), slot.cmd.as_raw() as usize as *mut c_void) != vk::Result::SUCCESS {
                return None;
            }
            slot.fence = d.create_fence(&vk::FenceCreateInfo::default(), None).ok()?;
            slot.semaphore = d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).ok()?;
        }
        Some(())
    }

    fn free(&self, slot: Slot) {
        let d = &self.fns;
        // SAFETY: the slot's own handles, null ones skipped by Vulkan, and nothing of it in use
        // on the GPU once its fence has signalled.
        unsafe {
            if slot.pending {
                let _ = d.wait_for_fences(&[slot.fence], true, u64::MAX);
            }
            d.destroy_semaphore(slot.semaphore, None);
            d.destroy_fence(slot.fence, None);
            d.destroy_command_pool(slot.pool, None);
            d.destroy_buffer(slot.buffer, None);
            d.free_memory(slot.memory, None);
        }
    }
}
