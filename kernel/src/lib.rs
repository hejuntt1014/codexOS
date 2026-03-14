#![no_std]

mod desktop;
mod serial;

use bootinfo::BootInfo;

pub fn run(boot_info: &BootInfo) -> ! {
    serial::init();
    serial_println!("codexOS kernel entered");
    serial_println!(
        "framebuffer: {}x{} stride={} bpp={} bytes={} format={:?}",
        boot_info.framebuffer.width,
        boot_info.framebuffer.height,
        boot_info.framebuffer.stride,
        boot_info.framebuffer.bytes_per_pixel,
        boot_info.framebuffer.size,
        boot_info.framebuffer.pixel_format
    );

    desktop::render(boot_info);

    loop {
        halt();
    }
}

#[cfg(target_arch = "x86_64")]
fn halt() {
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn halt() {
    core::hint::spin_loop();
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        $crate::serial::print(core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::serial_print!("\r\n")
    };
    ($fmt:expr) => {
        $crate::serial_print!(concat!($fmt, "\r\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::serial_print!(concat!($fmt, "\r\n"), $($arg)*)
    };
}
