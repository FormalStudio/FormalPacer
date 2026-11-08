//! `IDXGISwapChain::Present` and `IDXGISwapChain1::Present1`. Their addresses come from a swap
//! chain made for the purpose on a hidden window of our own; every swap chain in the process runs
//! the same code, so one inline hook on each covers the game's whichever way it made its own.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use formalpacer_hook::overlay::{BACKGROUND, COLORS, MARGIN};
use formalpacer_hook::pacer::{self, Frame};
use minhook::MinHook;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDeviceAndSwapChain,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_MODE_DESC, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT_TEST, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGISwapChain, IDXGISwapChain1,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HRESULT, Interface, w};

type Present = unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT;
type Present1 = unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> HRESULT;

static PRESENT: OnceLock<Present> = OnceLock::new();
static PRESENT1: OnceLock<Present1> = OnceLock::new();
/// Set once the game has swapped through OpenGL; DXGI presents under it are the driver's.
pub static UNDER_OPENGL: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the default handling for a window that does nothing.
    unsafe { DefWindowProcW(window, message, w, l) }
}

/// A swap chain on a hidden window, on the software rasteriser so no GPU work starts for it.
fn probe() -> Option<IDXGISwapChain> {
    // SAFETY: a window class and window of our own, destroyed with the swap chain still alive,
    // which keeps working for reading its function table.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("FormalPacerProbe"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("FormalPacerProbe"),
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            64,
            64,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 64,
                Height: 64,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: window,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let mut swapchain = None;
        let made = D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut swapchain),
            None,
            None,
            None,
        );
        let _ = DestroyWindow(window);
        made.ok()?;
        swapchain
    }
}

pub fn install() {
    let Some(swapchain) = probe() else { return };
    let present = swapchain.vtable().Present as *mut c_void;
    let present1 = swapchain.cast::<IDXGISwapChain1>().ok().map(|s| s.vtable().Present1 as *mut c_void);
    // SAFETY: MinHook patches the functions' first instructions and hands back trampolines to the
    // originals, kept before the hooks go live.
    unsafe {
        if let Ok(original) = MinHook::create_hook(present, hooked_present as *mut c_void) {
            let _ = PRESENT.set(std::mem::transmute::<*mut c_void, Present>(original));
        }
        if let Some(target) = present1
            && let Ok(original) = MinHook::create_hook(target, hooked_present1 as *mut c_void)
        {
            let _ = PRESENT1.set(std::mem::transmute::<*mut c_void, Present1>(original));
        }
    }
}

fn draw(this: *mut c_void) {
    // SAFETY: `this` is the swap chain the game is presenting, alive for the call.
    let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&this) }) else { return };
    if crate::d3d11::draw(swapchain).is_none() {
        crate::d3d12::draw(swapchain);
    }
}

/// Back buffer formats whose values the overlay's colours mean as they are. HDR10 swap chains
/// share `R10G10B10A2` with SDR ones and nothing says which a swap chain is, so it is left out.
const FORMATS: [DXGI_FORMAT; 3] =
    [DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT];

/// Notes the swap chain's size for the app, and returns it with the overlay to draw this frame.
pub fn overlay(swapchain: &IDXGISwapChain) -> Option<(DXGI_SWAP_CHAIN_DESC, Frame)> {
    // SAFETY: a live swap chain.
    let desc = unsafe { swapchain.GetDesc() }.ok()?;
    pacer::note_extent(desc.BufferDesc.Width, desc.BufferDesc.Height);
    let frame = pacer::overlay(desc.BufferDesc.Height)?;
    FORMATS.contains(&desc.BufferDesc.Format).then_some((desc, frame))
}

/// The overlay as rectangles to clear, one colour at a time, the box first.
pub fn rects(frame: &Frame) -> Vec<([f32; 4], Vec<RECT>)> {
    let bitmap = &frame.bitmap;
    let cell = bitmap.scale as i32;
    let left = (MARGIN * bitmap.scale) as i32;
    let rect = |x: i32, y: i32, w: i32, h: i32| RECT {
        left: left + x,
        top: left + y,
        right: left + x + w,
        bottom: left + y + h,
    };
    let runs = bitmap.runs();
    (0..COLORS.len() as u8)
        .map(|value| {
            let color = COLORS[usize::from(value)].map(|c| f32::from(c) / 255.0);
            let rects = if value == BACKGROUND {
                vec![rect(0, 0, bitmap.width as i32, bitmap.height as i32)]
            } else {
                runs.iter()
                    .filter(|r| r.3 == value)
                    .map(|&(x, y, w, _)| rect(x as i32, y as i32, w as i32, cell))
                    .collect()
            };
            (color, rects)
        })
        .filter(|(_, rects)| !rects.is_empty())
        .collect()
}

fn skip(flags: u32) -> bool {
    flags & DXGI_PRESENT_TEST.0 != 0 || UNDER_OPENGL.load(Ordering::Relaxed)
}

unsafe extern "system" fn hooked_present(this: *mut c_void, sync: u32, flags: u32) -> HRESULT {
    let Some(&original) = PRESENT.get() else { return HRESULT(0) };
    if skip(flags) {
        // SAFETY: the game's own call, passed on unchanged.
        return unsafe { original(this, sync, flags) };
    }
    // SAFETY: as above.
    crate::paced(|| draw(this), || unsafe { original(this, sync, flags) })
}

unsafe extern "system" fn hooked_present1(this: *mut c_void, sync: u32, flags: u32, params: *const c_void) -> HRESULT {
    let Some(&original) = PRESENT1.get() else { return HRESULT(0) };
    if skip(flags) {
        // SAFETY: the game's own call, passed on unchanged.
        return unsafe { original(this, sync, flags, params) };
    }
    // SAFETY: as above.
    crate::paced(|| draw(this), || unsafe { original(this, sync, flags, params) })
}
