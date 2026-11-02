//! Minimal Vulkan swapchain that clears each image and presents it.

use anyhow::{Context, Result, anyhow};
use ash::khr::{surface, swapchain};
use ash::{Device, Entry, Instance, vk};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

const FRAMES_IN_FLIGHT: usize = 2;

#[derive(Clone, Copy)]
pub enum Present {
    Immediate,
    Mailbox,
    Fifo,
}

pub struct Frame {
    slot: usize,
    image: u32,
}

pub struct Gfx {
    _entry: Entry,
    instance: Instance,
    surface_fn: surface::Instance,
    surface: vk::SurfaceKHR,
    phys: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    swapchain_fn: swapchain::Device,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    extent: vk::Extent2D,
    format: vk::SurfaceFormatKHR,
    mode: vk::PresentModeKHR,
    want: Present,
    pool: vk::CommandPool,
    cmds: Vec<vk::CommandBuffer>,
    acquired: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    // One per swapchain image: the presentation engine may still hold the
    // semaphore of an image after a later frame slot reuses its own.
    rendered: Vec<vk::Semaphore>,
    slot: usize,
}

impl Gfx {
    pub fn new(window: &Window, want: Present, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let entry = Entry::load().context("load libvulkan")?;
            let display = window.display_handle()?.as_raw();
            let handle = window.window_handle()?.as_raw();
            let exts = ash_window::enumerate_required_extensions(display)?;
            let app = vk::ApplicationInfo::default().application_name(c"fakegame").api_version(vk::API_VERSION_1_1);
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(exts),
                None,
            )?;
            let surface = ash_window::create_surface(&entry, &instance, display, handle, None)?;
            let surface_fn = surface::Instance::new(&entry, &instance);

            let (phys, family) = instance
                .enumerate_physical_devices()?
                .into_iter()
                .filter_map(|p| {
                    let family = instance.get_physical_device_queue_family_properties(p).iter().enumerate().position(
                        |(i, q)| {
                            q.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                                && surface_fn.get_physical_device_surface_support(p, i as u32, surface).unwrap_or(false)
                        },
                    )?;
                    Some((p, family as u32))
                })
                .max_by_key(|(p, _)| {
                    instance.get_physical_device_properties(*p).device_type == vk::PhysicalDeviceType::DISCRETE_GPU
                })
                .ok_or_else(|| anyhow!("no device can present to this surface"))?;

            let prio = [1.0];
            let queue_info = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let dev_exts = [swapchain::NAME.as_ptr()];
            let device = instance.create_device(
                phys,
                &vk::DeviceCreateInfo::default().queue_create_infos(&queue_info).enabled_extension_names(&dev_exts),
                None,
            )?;
            let queue = device.get_device_queue(family, 0);
            let swapchain_fn = swapchain::Device::new(&instance, &device);

            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let cmds = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .command_buffer_count(FRAMES_IN_FLIGHT as u32),
            )?;
            let mut acquired = Vec::new();
            let mut fences = Vec::new();
            for _ in 0..FRAMES_IN_FLIGHT {
                acquired.push(device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
                fences.push(
                    device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)?,
                );
            }

            let mut gfx = Self {
                _entry: entry,
                instance,
                surface_fn,
                surface,
                phys,
                device,
                queue,
                swapchain_fn,
                swapchain: vk::SwapchainKHR::null(),
                images: Vec::new(),
                extent: vk::Extent2D::default(),
                format: vk::SurfaceFormatKHR::default(),
                mode: vk::PresentModeKHR::FIFO,
                want,
                pool,
                cmds,
                acquired,
                fences,
                rendered: Vec::new(),
                slot: 0,
            };
            gfx.recreate(width, height)?;
            Ok(gfx)
        }
    }

    pub fn recreate(&mut self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.device.device_wait_idle()?;
            let caps = self.surface_fn.get_physical_device_surface_capabilities(self.phys, self.surface)?;
            if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST) {
                return Err(anyhow!("surface does not support TRANSFER_DST images"));
            }
            self.extent = if caps.current_extent.width == u32::MAX {
                vk::Extent2D {
                    width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                    height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                }
            } else {
                caps.current_extent
            };
            let formats = self.surface_fn.get_physical_device_surface_formats(self.phys, self.surface)?;
            self.format = formats
                .iter()
                .copied()
                .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
                .or(formats.first().copied())
                .ok_or_else(|| anyhow!("surface reports no formats"))?;
            let modes = self.surface_fn.get_physical_device_surface_present_modes(self.phys, self.surface)?;
            let wanted = match self.want {
                Present::Immediate => vk::PresentModeKHR::IMMEDIATE,
                Present::Mailbox => vk::PresentModeKHR::MAILBOX,
                Present::Fifo => vk::PresentModeKHR::FIFO,
            };
            self.mode = if modes.contains(&wanted) { wanted } else { vk::PresentModeKHR::FIFO };

            let mut count = caps.min_image_count + 1;
            if caps.max_image_count > 0 {
                count = count.min(caps.max_image_count);
            }
            let old = self.swapchain;
            self.swapchain = self.swapchain_fn.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(self.surface)
                    .min_image_count(count)
                    .image_format(self.format.format)
                    .image_color_space(self.format.color_space)
                    .image_extent(self.extent)
                    .image_array_layers(1)
                    .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                    .pre_transform(caps.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(self.mode)
                    .clipped(true)
                    .old_swapchain(old),
                None,
            )?;
            if old != vk::SwapchainKHR::null() {
                self.swapchain_fn.destroy_swapchain(old, None);
            }
            self.images = self.swapchain_fn.get_swapchain_images(self.swapchain)?;
            for s in self.rendered.drain(..) {
                self.device.destroy_semaphore(s, None);
            }
            for _ in &self.images {
                self.rendered.push(self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
            }
        }
        Ok(())
    }

    /// Waits for the slot's previous frame and acquires an image. `None` means
    /// the swapchain was out of date and has been rebuilt; skip this frame.
    pub fn begin_frame(&mut self, width: u32, height: u32) -> Result<Option<Frame>> {
        let slot = self.slot;
        unsafe {
            self.device.wait_for_fences(&[self.fences[slot]], true, u64::MAX)?;
            let image = match self.swapchain_fn.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.acquired[slot],
                vk::Fence::null(),
            ) {
                // SUBOPTIMAL still signals the semaphore; it is handled after present.
                Ok((i, _)) => i,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.recreate(width, height)?;
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            };
            // Reset only once a submit is guaranteed, or the fence never signals again.
            self.device.reset_fences(&[self.fences[slot]])?;
            Ok(Some(Frame { slot, image }))
        }
    }

    /// Records, submits and presents. Returns true when the swapchain needs
    /// rebuilding.
    pub fn end_frame(&mut self, frame: Frame, color: [f32; 4]) -> Result<bool> {
        let Frame { slot, image } = frame;
        let img = self.images[image as usize];
        let cmd = self.cmds[slot];
        let range =
            vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        let barrier = |old, new, src, dst| {
            vk::ImageMemoryBarrier::default()
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(img)
                .subresource_range(range)
        };
        unsafe {
            self.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::empty(),
                    vk::AccessFlags::TRANSFER_WRITE,
                )],
            );
            self.device.cmd_clear_color_image(
                cmd,
                img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue { float32: color },
                &[range],
            );
            self.device.cmd_pipeline_barrier(
                cmd,
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
            self.device.end_command_buffer(cmd)?;

            let wait = [self.acquired[slot]];
            let signal = [self.rendered[image as usize]];
            let cmds = [cmd];
            // The acquire semaphore must gate the first use of the image, which
            // is the layout transition in the TRANSFER stage.
            let stages = [vk::PipelineStageFlags::TRANSFER];
            self.device.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds)
                    .signal_semaphores(&signal)],
                self.fences[slot],
            )?;
            self.slot = (slot + 1) % FRAMES_IN_FLIGHT;

            let swapchains = [self.swapchain];
            let indices = [image];
            match self.swapchain_fn.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default().wait_semaphores(&signal).swapchains(&swapchains).image_indices(&indices),
            ) {
                Ok(suboptimal) => Ok(suboptimal),
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(true),
                Err(e) => Err(e.into()),
            }
        }
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        unsafe {
            // Best effort: nothing useful to do if the device is lost on exit.
            let _ = self.device.device_wait_idle();
            for s in self.rendered.drain(..).chain(self.acquired.drain(..)) {
                self.device.destroy_semaphore(s, None);
            }
            for f in self.fences.drain(..) {
                self.device.destroy_fence(f, None);
            }
            self.device.destroy_command_pool(self.pool, None);
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.device.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}
