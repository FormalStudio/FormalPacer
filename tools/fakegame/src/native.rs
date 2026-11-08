//! Direct3D 11, Direct3D 12 and OpenGL presenters on Windows, for the hook FormalPacer loads into
//! games that are not Vulkan. Each clears the window to a colour and presents without vsync.

use std::num::NonZeroIsize;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC,
    D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_STATE_PRESENT,
    D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, D3D12CreateDevice,
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence,
    ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGISwapChain1, IDXGISwapChain3,
};
use windows::Win32::Graphics::Gdi::{GetDC, HDC};
use windows::Win32::Graphics::OpenGL::{
    ChoosePixelFormat, GL_COLOR_BUFFER_BIT, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA,
    PIXELFORMATDESCRIPTOR, SetPixelFormat, SwapBuffers, glClear, glClearColor, glViewport, wglCreateContext,
    wglGetProcAddress, wglMakeCurrent,
};
use windows::core::Interface;

#[derive(Clone, Copy)]
pub enum Api {
    D3d11,
    D3d12,
    Gl,
}

const BUFFERS: u32 = 2;

pub enum Native {
    D3d11 { device: ID3D11Device, context: ID3D11DeviceContext, swapchain: IDXGISwapChain1, size: (u32, u32) },
    D3d12(Box<D3d12>),
    Gl { dc: HDC },
}

pub struct D3d12 {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    swapchain: IDXGISwapChain3,
    heap: ID3D12DescriptorHeap,
    rtv_size: usize,
    allocators: Vec<ID3D12CommandAllocator>,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    used: Vec<u64>,
    signalled: u64,
    size: (u32, u32),
}

fn swapchain_desc(width: u32, height: u32) -> DXGI_SWAP_CHAIN_DESC1 {
    DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFERS,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..Default::default()
    }
}

impl Native {
    pub fn new(api: Api, hwnd: NonZeroIsize, width: u32, height: u32) -> Result<Self> {
        let hwnd = HWND(hwnd.get() as *mut _);
        // SAFETY: a live window owned by this thread; every object made here is kept in `Self`.
        unsafe {
            match api {
                Api::D3d11 => {
                    let (mut device, mut context) = (None, None);
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        Default::default(),
                        D3D11_CREATE_DEVICE_FLAG(0),
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )?;
                    let device: ID3D11Device = device.context("no device")?;
                    let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
                    let swapchain =
                        factory.CreateSwapChainForHwnd(&device, hwnd, &swapchain_desc(width, height), None, None)?;
                    Ok(Self::D3d11 {
                        device,
                        context: context.context("no context")?,
                        swapchain,
                        size: (width, height),
                    })
                }
                Api::D3d12 => {
                    let mut device: Option<ID3D12Device> = None;
                    D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device)?;
                    let device = device.context("no device")?;
                    let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })?;
                    let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
                    let swapchain = factory
                        .CreateSwapChainForHwnd(&queue, hwnd, &swapchain_desc(width, height), None, None)?
                        .cast::<IDXGISwapChain3>()?;
                    let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                        NumDescriptors: BUFFERS,
                        ..Default::default()
                    })?;
                    let allocators = (0..BUFFERS)
                        .map(|_| {
                            device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
                        })
                        .collect::<windows::core::Result<Vec<_>>>()?;
                    let list: ID3D12GraphicsCommandList =
                        device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocators[0], None)?;
                    list.Close()?;
                    let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
                    Ok(Self::D3d12(Box::new(D3d12 {
                        rtv_size: device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize,
                        device,
                        queue,
                        swapchain,
                        heap,
                        used: vec![0; allocators.len()],
                        allocators,
                        list,
                        fence,
                        signalled: 0,
                        size: (width, height),
                    })))
                }
                Api::Gl => {
                    let dc = GetDC(Some(hwnd));
                    let pfd = PIXELFORMATDESCRIPTOR {
                        nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16,
                        nVersion: 1,
                        dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
                        iPixelType: PFD_TYPE_RGBA,
                        cColorBits: 32,
                        ..Default::default()
                    };
                    let format = ChoosePixelFormat(dc, &pfd);
                    SetPixelFormat(dc, format, &pfd)?;
                    let rc = wglCreateContext(dc)?;
                    wglMakeCurrent(dc, rc)?;
                    // Drivers default to vsync on OpenGL; this runs uncapped like the others.
                    if let Some(f) = wglGetProcAddress(windows::core::s!("wglSwapIntervalEXT")) {
                        let swap_interval: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
                        swap_interval(0);
                    }
                    Ok(Self::Gl { dc })
                }
            }
        }
    }

    pub fn present(&mut self, width: u32, height: u32, [r, g, b, a]: [f32; 4]) -> Result<()> {
        // SAFETY: objects made in `new`, used from the thread that made them.
        unsafe {
            match self {
                Self::D3d11 { device, context, swapchain, size } => {
                    if *size != (width, height) {
                        context.ClearState();
                        swapchain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
                        *size = (width, height);
                    }
                    let buffer: ID3D11Texture2D = swapchain.GetBuffer(0)?;
                    let mut view = None;
                    device.CreateRenderTargetView(&buffer, None, Some(&mut view))?;
                    context.ClearRenderTargetView(&view.context("no view")?, &[r, g, b, a]);
                    swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
                }
                Self::D3d12(d) => d.present(width, height, [r, g, b, a])?,
                Self::Gl { dc } => {
                    glViewport(0, 0, width as i32, height as i32);
                    glClearColor(r, g, b, a);
                    glClear(GL_COLOR_BUFFER_BIT);
                    SwapBuffers(*dc)?;
                }
            }
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
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // SAFETY: a borrowed copy of the pointer, never released.
                pResource: unsafe { std::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
        ..Default::default()
    }
}

impl D3d12 {
    unsafe fn wait(&self, value: u64) {
        // SAFETY: a live fence; a null event blocks until it reaches `value`.
        unsafe {
            if self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, HANDLE::default()).ok();
            }
        }
    }

    fn present(&mut self, width: u32, height: u32, color: [f32; 4]) -> Result<()> {
        // SAFETY: objects made in `Native::new`; each allocator is reset only once the GPU is past it.
        unsafe {
            if self.size != (width, height) {
                self.wait(self.signalled);
                self.swapchain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
                self.size = (width, height);
            }
            let index = self.swapchain.GetCurrentBackBufferIndex() as usize;
            self.wait(self.used[index]);
            let buffer: ID3D12Resource = self.swapchain.GetBuffer(index as u32)?;
            self.allocators[index].Reset()?;
            self.list.Reset(&self.allocators[index], None)?;
            let view = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: self.heap.GetCPUDescriptorHandleForHeapStart().ptr + index * self.rtv_size,
            };
            self.device.CreateRenderTargetView(&buffer, None, view);
            self.list.ResourceBarrier(&[transition(
                &buffer,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);
            self.list.ClearRenderTargetView(view, &color, None);
            self.list.ResourceBarrier(&[transition(
                &buffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
            self.signalled += 1;
            self.queue.Signal(&self.fence, self.signalled)?;
            self.used[index] = self.signalled;
        }
        Ok(())
    }
}

impl Drop for D3d12 {
    fn drop(&mut self) {
        // SAFETY: lets the GPU finish before the objects are freed.
        unsafe { self.wait(self.signalled) };
    }
}
