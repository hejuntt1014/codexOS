#![no_std]
#![no_main]

use bootinfo::BootInfo;
use core::hint::spin_loop;
use core::panic::PanicInfo;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KernelImageDescriptor {
    pub magic: [u8; 8],
    pub abi_version: u32,
    pub reserved: u32,
    pub entry_hint: u64,
}

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_DESCRIPTOR: KernelImageDescriptor = KernelImageDescriptor {
    magic: *b"CDXKERN\0",
    abi_version: 1,
    reserved: 0,
    entry_hint: 0,
};

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_BANNER: [u8; 23] = *b"codexOS kernel image v1";

#[unsafe(no_mangle)]
#[used]
pub static mut CODEXOS_EARLY_BSS: [u8; 4096] = [0; 4096];

#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    serial_init();
    serial_write_str("codexOS standalone kernel entered\r\n");

    if let Some(boot_info) = unsafe { boot_info.as_ref() } {
        draw_takeover(boot_info);
        serial_write_str("standalone boot info present\r\n");
        serial_write_str("standalone framebuffer takeover complete\r\n");
    } else {
        serial_write_str("standalone boot info missing\r\n");
    }

    loop {
        spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        spin_loop();
    }
}

const COM1: u16 = 0x3F8;

fn serial_init() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

fn serial_write_str(value: &str) {
    for byte in value.bytes() {
        unsafe {
            while (inb(COM1 + 5) & 0x20) == 0 {}
            outb(COM1, byte);
        }
    }
}

fn draw_takeover(boot_info: &BootInfo) {
    let width = boot_info.framebuffer.width as i32;
    let height = boot_info.framebuffer.height as i32;

    fill_rect(boot_info, 0, 0, width, height, 6, 12, 24);
    fill_rect(boot_info, 0, height / 2, width, height / 2, 9, 87, 122);

    for index in 0..10 {
        let x = 56 + index * 84;
        let y = 52 + (index % 2) * 18;
        fill_rect(boot_info, x, y, 56, 18, 255, 255, 255);
    }

    let panel_x = width / 2 - 240;
    let panel_y = height / 2 - 120;
    fill_rect(boot_info, panel_x, panel_y, 480, 240, 241, 245, 249);
    draw_rect(boot_info, panel_x, panel_y, 480, 240, 15, 23, 42);
    fill_rect(boot_info, panel_x, panel_y, 480, 28, 14, 165, 233);

    fill_rect(boot_info, panel_x + 24, panel_y + 56, 432, 36, 15, 23, 42);
    fill_rect(boot_info, panel_x + 24, panel_y + 104, 432, 36, 34, 197, 94);
    fill_rect(boot_info, panel_x + 24, panel_y + 152, 432, 36, 251, 191, 36);
    fill_rect(boot_info, panel_x + 24, panel_y + 200, 432, 16, 59, 130, 246);
}

fn fill_rect(
    boot_info: &BootInfo,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    r: u8,
    g: u8,
    b: u8,
) {
    let framebuffer = runtime_framebuffer_base(boot_info);
    let stride = boot_info.framebuffer.stride as i32;
    let bpp = boot_info.framebuffer.bytes_per_pixel as i32;
    let screen_width = boot_info.framebuffer.width as i32;
    let screen_height = boot_info.framebuffer.height as i32;

    let x0 = x.clamp(0, screen_width);
    let y0 = y.clamp(0, screen_height);
    let x1 = (x + width).clamp(0, screen_width);
    let y1 = (y + height).clamp(0, screen_height);

    if x0 >= x1 || y0 >= y1 {
        return;
    }

    for py in y0..y1 {
        for px in x0..x1 {
            let offset = (py * stride + px) * bpp;
            unsafe {
                let pixel = framebuffer.add(offset as usize);
                core::ptr::write_volatile(pixel, b);
                core::ptr::write_volatile(pixel.add(1), g);
                core::ptr::write_volatile(pixel.add(2), r);
                if bpp > 3 {
                    core::ptr::write_volatile(pixel.add(3), 0);
                }
            }
        }
    }
}

fn draw_rect(
    boot_info: &BootInfo,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    r: u8,
    g: u8,
    b: u8,
) {
    fill_rect(boot_info, x, y, width, 1, r, g, b);
    fill_rect(boot_info, x, y + height - 1, width, 1, r, g, b);
    fill_rect(boot_info, x, y, 1, height, r, g, b);
    fill_rect(boot_info, x + width - 1, y, 1, height, r, g, b);
}

fn runtime_framebuffer_base(boot_info: &BootInfo) -> *mut u8 {
    if boot_info.runtime_hhdm_base != 0 {
        boot_info
            .runtime_hhdm_base
            .saturating_add(boot_info.framebuffer.base as u64) as *mut u8
    } else {
        boot_info.framebuffer.base
    }
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
