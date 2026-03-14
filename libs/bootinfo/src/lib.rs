#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    Rgb = 0,
    Bgr = 1,
    Unknown = 2,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FrameBufferInfo {
    pub base: *mut u8,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub pixel_format: PixelFormat,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootInfo {
    pub framebuffer: FrameBufferInfo,
}
