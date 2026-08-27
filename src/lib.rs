//! A slit-scan camera field: the television shows the live camera, but each
//! displayed frame replaces only one line of it. The line walks across the
//! screen and wraps, so what is on the glass is one line of now beside a
//! minute of the recent past.

pub mod app;
pub mod args;
pub mod camera;
pub mod field;
pub mod sweep;

/// Vulkan, Metal, DX12 and WebGPU. Deliberately not `Backends::all()`, which
/// also brings up a GL context per instance purely to enumerate adapters.
pub const BACKENDS: wgpu::Backends = wgpu::Backends::PRIMARY;

/// Bytes in one tightly packed RGBA8 frame.
pub fn frame_bytes((width, height): (u32, u32)) -> usize {
    width as usize * height as usize * 4
}
