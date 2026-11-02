//! The Vulkan side of FormalPacer: an implicit layer that wraps `vkQueuePresentKHR`. Before each
//! present it sleeps until the next deadline of the interval the app asked for, then records how
//! long the frame took and how much of that was its own sleep. DXVK and VKD3D-Proton sit on
//! Vulkan, so this covers Proton games as well as native ones.
//!
//! In a game it also draws the overlay into each presented image (see `overlay`).
//!
//! The layer stays out of processes that are not games: there it hands back the next layer's
//! `vkQueuePresentKHR` and costs nothing per frame.

mod loader;
mod overlay;

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use ash::vk;
use formalpacer_hook::{game, pacer};
use loader::{
    LayerDeviceCreateInfo, LayerDeviceLoaderData, LayerInstanceCreateInfo, NegotiateLayerInterface, dispatch_key,
};

struct InstanceData {
    instance: vk::Instance,
    gipa: vk::PFN_vkGetInstanceProcAddr,
    destroy: vk::PFN_vkDestroyInstance,
}

#[derive(Clone)]
struct DeviceData {
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    destroy: vk::PFN_vkDestroyDevice,
    present: Option<vk::PFN_vkQueuePresentKHR>,
    /// Only in games.
    overlay: Option<Arc<overlay::Device>>,
}

static INSTANCES: LazyLock<Mutex<HashMap<usize, InstanceData>>> = LazyLock::new(Default::default);
static DEVICES: LazyLock<Mutex<HashMap<usize, DeviceData>>> = LazyLock::new(Default::default);
static ACTIVE: OnceLock<Option<game::Game>> = OnceLock::new();

fn game() -> Option<&'static game::Game> {
    ACTIVE.get_or_init(game::detect).as_ref()
}

/// # Safety
/// Called by the Vulkan loader with a valid negotiation struct.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    negotiate: *mut NegotiateLayerInterface,
) -> vk::Result {
    // SAFETY: the loader passes a valid struct.
    let Some(negotiate) = (unsafe { negotiate.as_mut() }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    if negotiate.version < 2 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    negotiate.version = 2;
    negotiate.gipa = Some(get_instance_proc_addr);
    negotiate.gdpa = Some(get_device_proc_addr);
    negotiate.gpdpa = std::ptr::null();
    vk::Result::SUCCESS
}

unsafe extern "system" fn create_instance(
    info: *const vk::InstanceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    // SAFETY: the loader's chain info is valid for the duration of the call.
    unsafe {
        let Some(chain) = LayerInstanceCreateInfo::find((*info).p_next) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*chain).layer_info;
        let gipa = (*link).gipa;
        (*chain).layer_info = (*link).next;
        let Some(create) = gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let create = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateInstance>(create);
        let result = create(info, alloc, out);
        if result != vk::Result::SUCCESS {
            return result;
        }
        let Some(destroy) = gipa(*out, c"vkDestroyInstance".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let destroy = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroyInstance>(destroy);
        if let Ok(mut map) = INSTANCES.lock() {
            map.insert(dispatch_key(*out), InstanceData { instance: *out, gipa, destroy });
        }
        result
    }
}

unsafe extern "system" fn destroy_instance(instance: vk::Instance, alloc: *const vk::AllocationCallbacks<'_>) {
    let data = INSTANCES.lock().ok().and_then(|mut m| m.remove(&dispatch_key(instance)));
    if let Some(data) = data {
        // SAFETY: forwarding the app's own call to the next layer.
        unsafe { (data.destroy)(instance, alloc) };
    }
}

unsafe extern "system" fn create_device(
    physical: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    // SAFETY: the loader's chain info is valid for the duration of the call.
    unsafe {
        let Some(chain) = LayerDeviceCreateInfo::find((*info).p_next) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*chain).layer_info;
        let gipa = (*link).gipa;
        let gdpa = (*link).gdpa;
        (*chain).layer_info = (*link).next;
        let Some(create) = gipa(vk::Instance::null(), c"vkCreateDevice".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let create = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateDevice>(create);
        let result = create(physical, info, alloc, out);
        if result != vk::Result::SUCCESS {
            return result;
        }
        let Some(destroy) = gdpa(*out, c"vkDestroyDevice".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let destroy = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroyDevice>(destroy);
        let present = gdpa(*out, c"vkQueuePresentKHR".as_ptr())
            .map(|f| std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkQueuePresentKHR>(f));
        let instance = game().and_then(|_| INSTANCES.lock().ok()?.get(&dispatch_key(physical)).map(|d| d.instance));
        let overlay = instance.map(|instance| {
            let set_loader_data = LayerDeviceLoaderData::find((*info).p_next).map(|c| (*c).set_device_loader_data);
            Arc::new(overlay::Device::new(instance, gipa, physical, *out, gdpa, set_loader_data))
        });
        if let Ok(mut map) = DEVICES.lock() {
            map.insert(dispatch_key(*out), DeviceData { gdpa, destroy, present, overlay });
        }
        result
    }
}

unsafe extern "system" fn destroy_device(device: vk::Device, alloc: *const vk::AllocationCallbacks<'_>) {
    let data = DEVICES.lock().ok().and_then(|mut m| m.remove(&dispatch_key(device)));
    if let Some(data) = data {
        if let Some(overlay) = &data.overlay {
            overlay.destroy();
        }
        // SAFETY: forwarding the app's own call to the next layer.
        unsafe { (data.destroy)(device, alloc) };
    }
}

/// The overlay state of the device `handle` (a device or one of its queues) belongs to.
fn overlay_device<H: ash::vk::Handle>(handle: H) -> Option<Arc<overlay::Device>> {
    DEVICES.lock().ok()?.get(&dispatch_key(handle))?.overlay.clone()
}

unsafe extern "system" fn queue_present(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    // Queues carry their device's dispatch pointer, so the device's key finds them.
    let next = DEVICES.lock().ok().and_then(|m| m.get(&dispatch_key(queue)).map(|d| (d.present, d.overlay.clone())));
    let Some((Some(next), overlay)) = next else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    if let Some(game) = game() {
        pacer::before_present(game);
    }
    // SAFETY: the app's present info is valid for the duration of the call.
    let waits = overlay.map(|o| unsafe { o.draw(queue, &*info) }).unwrap_or_default();
    if waits.is_empty() {
        // SAFETY: forwarding the app's own call to the next layer.
        return unsafe { next(queue, info) };
    }
    // SAFETY: the app's present info, copied with its pNext chain, now waiting on the overlay's
    // copies, which waited on the app's semaphores.
    unsafe {
        let mut ours = *info;
        ours.wait_semaphore_count = waits.len() as u32;
        ours.p_wait_semaphores = waits.as_ptr();
        next(queue, &ours)
    }
}

unsafe extern "system" fn get_device_queue(device: vk::Device, family: u32, index: u32, out: *mut vk::Queue) {
    if let Some(overlay) = overlay_device(device) {
        // SAFETY: the app's own call.
        unsafe { overlay.get_queue(family, index, out) };
    }
}

unsafe extern "system" fn get_device_queue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2<'_>,
    out: *mut vk::Queue,
) {
    if let Some(overlay) = overlay_device(device) {
        // SAFETY: the app's own call.
        unsafe { overlay.get_queue2(info, out) };
    }
}

unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    let Some(overlay) = overlay_device(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    // SAFETY: the app's own call.
    unsafe { overlay.create_swapchain(info, alloc, out) }
}

unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    if let Some(overlay) = overlay_device(device) {
        // SAFETY: the app's own call.
        unsafe { overlay.destroy_swapchain(swapchain, alloc) };
    }
}

/// The next layer's entry point for `name`.
fn next_instance_fn(instance: vk::Instance, name: &CStr) -> vk::PFN_vkVoidFunction {
    let gipa = INSTANCES.lock().ok().and_then(|m| m.get(&dispatch_key(instance)).map(|d| d.gipa))?;
    // SAFETY: forwarding to the next layer.
    unsafe { gipa(instance, name.as_ptr()) }
}

unsafe extern "system" fn create_xcb_surface(
    instance: vk::Instance,
    info: *const vk::XcbSurfaceCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SurfaceKHR,
) -> vk::Result {
    let Some(next) = next_instance_fn(instance, c"vkCreateXcbSurfaceKHR") else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    // SAFETY: forwarding the app's own call, with the type of the name asked for.
    unsafe {
        let next = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateXcbSurfaceKHR>(next);
        let result = next(instance, info, alloc, out);
        if result == vk::Result::SUCCESS {
            pacer::note_x11_window((*info).window);
        }
        result
    }
}

unsafe extern "system" fn create_xlib_surface(
    instance: vk::Instance,
    info: *const vk::XlibSurfaceCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SurfaceKHR,
) -> vk::Result {
    let Some(next) = next_instance_fn(instance, c"vkCreateXlibSurfaceKHR") else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    // SAFETY: forwarding the app's own call, with the type of the name asked for.
    unsafe {
        let next = std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateXlibSurfaceKHR>(next);
        let result = next(instance, info, alloc, out);
        if result == vk::Result::SUCCESS {
            // X11 window ids are 29 bits.
            #[allow(clippy::unnecessary_cast, reason = "Window is a c_ulong: u64, or u32 on 32-bit targets")]
            pacer::note_x11_window((*info).window as u32);
        }
        result
    }
}

macro_rules! entry {
    ($f:expr, $ty:ty) => {
        // SAFETY: the loader casts the generic pointer back to the type of the name it asked for.
        Some(unsafe { std::mem::transmute::<$ty, unsafe extern "system" fn()>($f) })
    };
}

fn ours(name: &CStr, device_level: bool) -> vk::PFN_vkVoidFunction {
    match name.to_bytes() {
        b"vkGetDeviceProcAddr" => entry!(get_device_proc_addr, vk::PFN_vkGetDeviceProcAddr),
        b"vkDestroyDevice" => entry!(destroy_device, vk::PFN_vkDestroyDevice),
        b"vkQueuePresentKHR" if game().is_some() => entry!(queue_present, vk::PFN_vkQueuePresentKHR),
        b"vkGetDeviceQueue" if game().is_some() => entry!(get_device_queue, vk::PFN_vkGetDeviceQueue),
        b"vkGetDeviceQueue2" if game().is_some() => entry!(get_device_queue2, vk::PFN_vkGetDeviceQueue2),
        b"vkCreateSwapchainKHR" if game().is_some() => entry!(create_swapchain, vk::PFN_vkCreateSwapchainKHR),
        b"vkDestroySwapchainKHR" if game().is_some() => entry!(destroy_swapchain, vk::PFN_vkDestroySwapchainKHR),
        _ if device_level => None,
        b"vkGetInstanceProcAddr" => entry!(get_instance_proc_addr, vk::PFN_vkGetInstanceProcAddr),
        b"vkCreateInstance" => entry!(create_instance, vk::PFN_vkCreateInstance),
        b"vkDestroyInstance" => entry!(destroy_instance, vk::PFN_vkDestroyInstance),
        b"vkCreateDevice" => entry!(create_device, vk::PFN_vkCreateDevice),
        b"vkCreateXcbSurfaceKHR" if game().is_some() => entry!(create_xcb_surface, vk::PFN_vkCreateXcbSurfaceKHR),
        b"vkCreateXlibSurfaceKHR" if game().is_some() => entry!(create_xlib_surface, vk::PFN_vkCreateXlibSurfaceKHR),
        _ => None,
    }
}

unsafe extern "system" fn get_instance_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: the loader passes a NUL-terminated name.
    let name = unsafe { CStr::from_ptr(name) };
    if let Some(f) = ours(name, false) {
        return Some(f);
    }
    if instance == vk::Instance::null() {
        return None;
    }
    next_instance_fn(instance, name)
}

unsafe extern "system" fn get_device_proc_addr(device: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
    // SAFETY: the loader passes a NUL-terminated name.
    let name = unsafe { CStr::from_ptr(name) };
    let Some(gdpa) = DEVICES.lock().ok().and_then(|m| m.get(&dispatch_key(device)).map(|d| d.gdpa)) else {
        return ours(name, true);
    };
    // SAFETY: forwarding to the next layer.
    let next = unsafe { gdpa(device, name.as_ptr()) };
    // A hook only for what the device has, so an app probing for an extension still sees it missing.
    next.map(|next| ours(name, true).unwrap_or(next))
}
