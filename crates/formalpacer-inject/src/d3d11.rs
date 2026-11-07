//! The overlay on Direct3D 11 (and 10 on 11) swap chains: `ClearView` of rectangles on a render
//! target view made for the frame and dropped after it, so the game's `ResizeBuffers` never finds
//! a reference of ours on its back buffer. No pipeline state is touched.

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext1, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

/// `None` when the swap chain is not a Direct3D 11 device's.
pub fn draw(swapchain: &IDXGISwapChain) -> Option<()> {
    // SAFETY: a live swap chain; asking for a device of another API fails cleanly.
    let device: ID3D11Device = unsafe { swapchain.GetDevice() }.ok()?;
    let Some((_, frame)) = crate::dxgi::overlay(swapchain) else { return Some(()) };
    // SAFETY: buffer 0 of a flip model swap chain is the one about to be presented, and of a blt
    // model one the only one the game draws to. The view and buffer are released on return.
    unsafe {
        let Ok(context) = device.GetImmediateContext().and_then(|c| c.cast::<ID3D11DeviceContext1>()) else {
            return Some(());
        };
        let Ok(buffer) = swapchain.GetBuffer::<ID3D11Texture2D>(0) else { return Some(()) };
        let mut view = None;
        if device.CreateRenderTargetView(&buffer, None, Some(&mut view)).is_err() {
            return Some(());
        }
        let Some(view) = view else { return Some(()) };
        for (color, rects) in crate::dxgi::rects(&frame) {
            context.ClearView(&view, &color, Some(&rects));
        }
    }
    Some(())
}
