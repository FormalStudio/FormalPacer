//! The loader's layer interface from `vk_layer.h`, which ash does not carry.

use std::ffi::c_void;

use ash::vk::{self, Handle};

/// `VK_LAYER_LINK_INFO`: the chain entry that carries the next layer's entry points.
const LINK_INFO: i32 = 0;

#[repr(C)]
pub struct LayerInstanceLink {
    pub next: *mut LayerInstanceLink,
    pub gipa: vk::PFN_vkGetInstanceProcAddr,
    pub gpdpa: *const c_void,
}

#[repr(C)]
pub struct LayerInstanceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: i32,
    pub layer_info: *mut LayerInstanceLink,
}

#[repr(C)]
pub struct LayerDeviceLink {
    pub next: *mut LayerDeviceLink,
    pub gipa: vk::PFN_vkGetInstanceProcAddr,
    pub gdpa: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
pub struct LayerDeviceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: i32,
    pub layer_info: *mut LayerDeviceLink,
}

#[repr(C)]
pub struct NegotiateLayerInterface {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub version: u32,
    pub gipa: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub gdpa: Option<vk::PFN_vkGetDeviceProcAddr>,
    pub gpdpa: *const c_void,
}

macro_rules! find_link {
    ($ty:ty, $s_type:expr) => {
        impl $ty {
            /// The link entry in a create info's `pNext` chain. The loader expects the layer to
            /// advance it in place before calling down.
            ///
            /// # Safety
            /// `p_next` must be a valid Vulkan structure chain.
            pub unsafe fn find(mut p_next: *const c_void) -> Option<*mut Self> {
                while !p_next.is_null() {
                    let entry = p_next as *mut Self;
                    // SAFETY: every chain entry starts with sType and pNext.
                    unsafe {
                        if (*entry).s_type == $s_type && (*entry).function == LINK_INFO {
                            return Some(entry);
                        }
                        p_next = (*entry).p_next;
                    }
                }
                None
            }
        }
    };
}

find_link!(LayerInstanceCreateInfo, vk::StructureType::LOADER_INSTANCE_CREATE_INFO);
find_link!(LayerDeviceCreateInfo, vk::StructureType::LOADER_DEVICE_CREATE_INFO);

/// The loader's dispatch table pointer, stored in the first word of every dispatchable handle.
/// A device and its queues share one, as do an instance and its physical devices.
pub fn dispatch_key<H: Handle>(handle: H) -> usize {
    let raw = handle.as_raw() as usize as *const usize;
    if raw.is_null() {
        return 0;
    }
    // SAFETY: dispatchable handles point at loader objects whose first word is the table.
    unsafe { *raw }
}
