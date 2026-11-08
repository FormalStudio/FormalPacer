//! The overlay on Direct3D 12 swap chains: a command list of our own on the queue the swap chain
//! presents from, which takes the back buffer from the present state to a render target, clears
//! rectangles into it and gives it back, ahead of the game's present on the same queue.
//!
//! A swap chain does not say which queue it was made on, so `ExecuteCommandLists` is hooked too
//! and the last direct queue the game submitted to is taken as the one it presents from.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::{Mutex, OnceLock, PoisonError};

use minhook::MinHook;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::core::w;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC,
    D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES,
    D3D12_RESOURCE_TRANSITION_BARRIER, D3D12CreateDevice, ID3D12CommandAllocator, ID3D12CommandList,
    ID3D12CommandQueue, ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::{IDXGISwapChain, IDXGISwapChain3};
use windows::core::{Interface, Result};

struct Renderer {
    queue: ID3D12CommandQueue,
    device: ID3D12Device,
    heap: ID3D12DescriptorHeap,
    rtv_size: usize,
    /// One per back buffer, each reused only once the GPU is past its last use.
    allocators: Vec<ID3D12CommandAllocator>,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    used: Vec<u64>,
    signalled: u64,
}

// SAFETY: Direct3D 12 objects are free-threaded; the mutex keeps one recorder at a time.
unsafe impl Send for Renderer {}

static RENDERER: Mutex<Option<Renderer>> = Mutex::new(None);

type Execute = unsafe extern "system" fn(*mut c_void, u32, *const *mut c_void);
static EXECUTE: OnceLock<Execute> = OnceLock::new();

struct Queue(ID3D12CommandQueue);
// SAFETY: command queues are free-threaded.
unsafe impl Send for Queue {}
static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);

/// Hooks `ExecuteCommandLists`, found on a queue of a device made for the purpose, in a game that
/// has Direct3D 12 loaded.
pub fn install() {
    // SAFETY: looks up a loaded module; the probe device and queue are released on return, and
    // MinHook hands back a trampoline to the original, stored before the hook is enabled.
    unsafe {
        if GetModuleHandleW(w!("d3d12.dll")).is_err() {
            return;
        }
        let mut device: Option<ID3D12Device> = None;
        if D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device).is_err() {
            return;
        }
        let Some(device) = device else { return };
        let desc = D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() };
        let Ok(queue) = device.CreateCommandQueue::<ID3D12CommandQueue>(&desc) else { return };
        let target = queue.vtable().ExecuteCommandLists as *mut c_void;
        if let Ok(original) = MinHook::create_hook(target, hooked_execute as *mut c_void) {
            let _ = EXECUTE.set(std::mem::transmute::<*mut c_void, Execute>(original));
        }
    }
}

unsafe extern "system" fn hooked_execute(this: *mut c_void, count: u32, lists: *const *mut c_void) {
    let Some(&original) = EXECUTE.get() else { return };
    // SAFETY: `this` is the queue the game is submitting to, alive for the call.
    if let Some(queue) = unsafe { ID3D12CommandQueue::from_raw_borrowed(&this) } {
        let mut seen = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        let known = seen.as_ref().is_some_and(|q| q.0.as_raw() == this);
        // SAFETY: as above.
        if !known && unsafe { queue.GetDesc() }.Type == D3D12_COMMAND_LIST_TYPE_DIRECT {
            *seen = Some(Queue(queue.clone()));
        }
    }
    // SAFETY: the game's own call, passed on unchanged.
    unsafe { original(this, count, lists) }
}

impl Renderer {
    unsafe fn new(queue: ID3D12CommandQueue, buffers: u32) -> Result<Self> {
        // SAFETY: the caller's queue, alive; everything made here is ours.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            queue.GetDevice(&mut device)?;
            let device = device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_NOINTERFACE))?;
            let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: buffers,
                ..Default::default()
            })?;
            let rtv_size = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
            let allocators = (0..buffers)
                .map(|_| device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT))
                .collect::<Result<Vec<_>>>()?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocators[0], None)?;
            list.Close()?;
            let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            Ok(Self {
                queue,
                device,
                heap,
                rtv_size,
                used: vec![0; allocators.len()],
                allocators,
                list,
                fence,
                signalled: 0,
            })
        }
    }

    unsafe fn wait(&self, value: u64) {
        // SAFETY: a live fence; a null event makes the call block until it reaches `value`.
        unsafe {
            if self.fence.GetCompletedValue() < value {
                let _ = self.fence.SetEventOnCompletion(value, HANDLE::default());
            }
        }
    }

    unsafe fn draw(&mut self, swapchain: &IDXGISwapChain3, frame: &formalpacer_hook::pacer::Frame) -> Result<()> {
        // SAFETY: the back buffer is in the present state, as the game must leave it to present;
        // the commands here put it back there and run on the queue before the present does.
        unsafe {
            let index = swapchain.GetCurrentBackBufferIndex() as usize % self.allocators.len();
            let buffer: ID3D12Resource = swapchain.GetBuffer(index as u32)?;
            self.wait(self.used[index]);
            let allocator = &self.allocators[index];
            allocator.Reset()?;
            self.list.Reset(allocator, None)?;
            let start = self.heap.GetCPUDescriptorHandleForHeapStart();
            let view = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: start.ptr + index * self.rtv_size };
            self.device.CreateRenderTargetView(&buffer, None, view);
            self.list.ResourceBarrier(&[transition(
                &buffer,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);
            // One rectangle per clear: NVIDIA's driver fails fast on a clear of the overlay's
            // hundred-odd rectangles at once.
            for (color, rects) in crate::dxgi::rects(frame) {
                for rect in rects {
                    self.list.ClearRenderTargetView(view, &color, Some(&[rect]));
                }
            }
            self.list.ResourceBarrier(&[transition(
                &buffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
            self.signalled += 1;
            self.queue.Signal(&self.fence, self.signalled)?;
            self.used[index] = self.signalled;
        }
        Ok(())
    }
}

fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // SAFETY: a borrowed copy of the pointer, never released: the barrier is only
                // read while `resource` is alive.
                pResource: unsafe { std::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

pub fn draw(swapchain: &IDXGISwapChain) {
    let Some(queue) = QUEUE.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|q| q.0.clone()) else {
        return;
    };
    let Ok(swapchain3) = swapchain.cast::<IDXGISwapChain3>() else { return };
    let Some((desc, frame)) = crate::dxgi::overlay(swapchain) else { return };
    let mut renderer = RENDERER.lock().unwrap_or_else(PoisonError::into_inner);
    let stale = renderer.as_ref().is_some_and(|r| r.queue != queue || r.allocators.len() != desc.BufferCount as usize);
    if stale && let Some(old) = renderer.take() {
        // SAFETY: lets the GPU finish with the old command lists before they are freed.
        unsafe { old.wait(old.signalled) };
    }
    if renderer.is_none() {
        // SAFETY: the swap chain's own queue.
        *renderer = unsafe { Renderer::new(queue, desc.BufferCount.max(1)) }.ok();
    }
    if let Some(r) = renderer.as_mut() {
        // SAFETY: see `Renderer::draw`.
        let _ = unsafe { r.draw(&swapchain3, &frame) };
    }
}
