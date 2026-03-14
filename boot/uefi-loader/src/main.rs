#![no_std]
#![no_main]

use bootinfo::{BootInfo, FrameBufferInfo, PixelFormat};
use kernel::run;
use uefi::boot;
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};

#[entry]
fn main() -> Status {
    let handle = match boot::get_handle_for_protocol::<GraphicsOutput>() {
        Ok(handle) => handle,
        Err(status) => return status.status(),
    };

    let mut gop = match boot::open_protocol_exclusive::<GraphicsOutput>(handle) {
        Ok(gop) => gop,
        Err(status) => return status.status(),
    };

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let mut frame_buffer = gop.frame_buffer();

    let boot_info = BootInfo {
        framebuffer: FrameBufferInfo {
            base: frame_buffer.as_mut_ptr(),
            size: frame_buffer.size(),
            width: width as u32,
            height: height as u32,
            stride: mode.stride(),
            bytes_per_pixel: 4,
            pixel_format: map_pixel_format(mode.pixel_format()),
        },
    };

    run(&boot_info)
}

fn map_pixel_format(format: GopPixelFormat) -> PixelFormat {
    match format {
        GopPixelFormat::Rgb => PixelFormat::Rgb,
        GopPixelFormat::Bgr => PixelFormat::Bgr,
        _ => PixelFormat::Unknown,
    }
}
