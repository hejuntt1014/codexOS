#![no_std]
#![no_main]

use bootinfo::{BootInfo, FrameBufferInfo};
use core::hint::spin_loop;
use core::panic::PanicInfo;
use gfx::{Canvas, Color};

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
        serial_write_str("standalone boot info present\r\n");
        if render_standalone_desktop(boot_info) {
            serial_write_str("standalone framebuffer takeover complete\r\n");
            serial_write_str("standalone desktop rendered\r\n");
        } else {
            serial_write_str("standalone framebuffer unavailable\r\n");
        }
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

fn render_standalone_desktop(boot_info: &BootInfo) -> bool {
    let Some(framebuffer) = runtime_framebuffer(boot_info) else {
        return false;
    };
    let width = framebuffer.width as i32;
    let height = framebuffer.height as i32;

    let mut canvas = unsafe { Canvas::from_framebuffer(framebuffer) };
    canvas.vertical_gradient(Color::rgb(11, 17, 24), Color::rgb(26, 44, 61));
    canvas.checkerboard(
        48,
        Color::rgb(17, 26, 35),
        Color::rgb(38, 78, 110),
        1,
        6,
    );

    canvas.fill_rect(24, 24, width - 48, height - 48, Color::rgb(243, 246, 248));
    canvas.draw_shadow(24, 24, width - 48, height - 48, 6, Color::rgb(10, 15, 20));
    canvas.fill_rect(24, 24, width - 48, 72, Color::rgb(19, 88, 126));
    canvas.fill_rect(24, height - 92, width - 48, 44, Color::rgb(229, 235, 238));
    canvas.fill_rect(24, height - 92, width - 48, 1, Color::rgb(197, 205, 210));

    draw_header(&mut canvas, boot_info, width);
    draw_overview_card(&mut canvas, boot_info, 56, 128, width / 2 - 72, 292);
    draw_kernel_card(
        &mut canvas,
        boot_info,
        width / 2 + 16,
        128,
        width / 2 - 72,
        292,
    );
    draw_footer(&mut canvas, boot_info, width, height);
    draw_cursor_accent(&mut canvas, width, height);

    true
}

fn draw_header(canvas: &mut Canvas, boot_info: &BootInfo, width: i32) {
    let ink = Color::rgb(245, 248, 250);
    let sub = Color::rgb(205, 223, 231);
    draw_label(canvas, 52, 42, "CODEXOS", ink, 3);
    draw_label(canvas, 52, 68, "STANDALONE", sub, 2);
    draw_label(canvas, 52, 92, "CHAINLOAD DESKTOP", sub, 1);
    let mode_label = if boot_info.firmware_mode.as_str() == "post-exit-boot-services" {
        "MODE POST EBS"
    } else {
        "MODE UEFI"
    };
    draw_label(canvas, width - 286, 48, mode_label, ink, 1);
}

fn draw_overview_card(
    canvas: &mut Canvas,
    boot_info: &BootInfo,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    let title = Color::rgb(24, 35, 44);
    let body = Color::rgb(70, 82, 92);
    let accent = Color::rgb(36, 127, 170);
    let panel = Color::rgb(255, 255, 255);

    canvas.fill_rect(x, y, width, height, panel);
    canvas.draw_rect(x, y, width, height, Color::rgb(211, 218, 223));
    canvas.fill_rect(x, y, width, 40, Color::rgb(233, 239, 242));
    draw_label(canvas, x + 18, y + 14, "BOOT SNAPSHOT", title, 1);

    draw_label(canvas, x + 18, y + 58, "FRAMEBUFFER", body, 1);
    draw_label(canvas, x + 18, y + 82, "MEMORY MAP", body, 1);
    draw_label(canvas, x + 18, y + 106, "USABLE MEMORY", body, 1);

    canvas.fill_rect(x + 18, y + 138, width - 36, 14, Color::rgb(225, 231, 234));
    let usage_width =
        ((width - 36) as u64).saturating_mul(boot_info.usable_memory_bytes()) / boot_info.total_memory_bytes().max(1);
    canvas.fill_rect(
        x + 18,
        y + 138,
        usage_width as i32,
        14,
        accent,
    );

    draw_label(canvas, x + 18, y + 176, "RESERVED OBJECTS", title, 1);
    draw_label(canvas, x + 18, y + 204, "LOW MEMORY", body, 1);
    draw_label(canvas, x + 18, y + 226, "LOADER IMAGE", body, 1);
    draw_label(canvas, x + 18, y + 248, "FRAMEBUFFER", body, 1);

    canvas.fill_rect(x + 18, y + height - 70, width - 36, 44, Color::rgb(241, 246, 248));
    canvas.draw_rect(
        x + 18,
        y + height - 70,
        width - 36,
        44,
        Color::rgb(214, 221, 226),
    );
    draw_label(canvas, x + 32, y + height - 56, "FRAMEBUFFER ACTIVE", accent, 1);
}

fn draw_kernel_card(
    canvas: &mut Canvas,
    boot_info: &BootInfo,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    let title = Color::rgb(24, 35, 44);
    let body = Color::rgb(70, 82, 92);
    let accent = Color::rgb(32, 92, 128);

    canvas.fill_rect(x, y, width, height, Color::rgb(248, 250, 251));
    canvas.draw_rect(x, y, width, height, Color::rgb(211, 218, 223));
    canvas.fill_rect(x, y, width, 40, Color::rgb(232, 238, 241));
    draw_label(canvas, x + 18, y + 14, "KERNEL IMAGE", title, 1);

    draw_label(canvas, x + 18, y + 58, "CHAINLOAD ENTRY", body, 1);
    draw_label(canvas, x + 18, y + 82, "STAGED PAGES", body, 1);
    draw_label(canvas, x + 18, y + 106, "DIRECT FRAMEBUFFER", body, 1);

    draw_label(canvas, x + 18, y + 146, "LOAD SEGMENTS", title, 1);
    let mut row_y = y + 174;
    for (index, segment) in boot_info.kernel_image.segments().iter().take(2).enumerate() {
        canvas.fill_rect(x + 18, row_y - 4, width - 36, 42, Color::rgb(239, 244, 246));
        canvas.draw_rect(
            x + 18,
            row_y - 4,
            width - 36,
            42,
            Color::rgb(217, 223, 227),
        );
        if index == 0 {
            draw_label(canvas, x + 30, row_y + 12, "TEXT SEGMENT", accent, 1);
            let bar_width =
                ((width - 86) as u64).saturating_mul(segment.load_page_count) / boot_info.kernel_image.load_page_count.max(1);
            canvas.fill_rect(x + 30, row_y + 26, width - 86, 6, Color::rgb(217, 223, 227));
            canvas.fill_rect(x + 30, row_y + 26, bar_width as i32, 6, accent);
        } else {
            draw_label(canvas, x + 30, row_y + 12, "DATA SEGMENT", accent, 1);
            let bar_width =
                ((width - 86) as u64).saturating_mul(segment.load_page_count) / boot_info.kernel_image.load_page_count.max(1);
            canvas.fill_rect(x + 30, row_y + 26, width - 86, 6, Color::rgb(217, 223, 227));
            canvas.fill_rect(x + 30, row_y + 26, bar_width as i32, 6, body);
        }
        row_y += 54;
    }

    canvas.fill_rect(x + 18, y + height - 120, width - 36, 84, Color::rgb(32, 92, 128));
    draw_label(
        canvas,
        x + 28,
        y + height - 104,
        "NEXT MILESTONE",
        Color::rgb(238, 245, 248),
        1,
    );
    draw_label(
        canvas,
        x + 28,
        y + height - 82,
        "OWN EVENT LOOP",
        Color::rgb(214, 229, 236),
        1,
    );
    draw_label(
        canvas,
        x + 28,
        y + height - 62,
        "AND SHELL",
        Color::rgb(214, 229, 236),
        1,
    );
}

fn draw_footer(canvas: &mut Canvas, boot_info: &BootInfo, width: i32, height: i32) {
    let _ = boot_info;
    draw_label(canvas, 42, height - 76, "POST EBS HANDOFF ACTIVE", Color::rgb(78, 91, 101), 1);

    draw_label(
        canvas,
        width - 262,
        height - 76,
        "SERIAL DESKTOP RENDERED",
        Color::rgb(32, 92, 128),
        1,
    );
}

fn draw_cursor_accent(canvas: &mut Canvas, width: i32, height: i32) {
    canvas.fill_rect(width - 126, 122, 14, 14, Color::rgb(32, 92, 128));
    canvas.fill_rect(width - 98, 122, 14, 14, Color::rgb(47, 127, 164));
    canvas.fill_rect(width - 70, 122, 14, 14, Color::rgb(83, 166, 201));
    canvas.draw_cursor(width - 112, height - 148);
}

fn runtime_framebuffer(boot_info: &BootInfo) -> Option<FrameBufferInfo> {
    let mut framebuffer = boot_info.framebuffer;
    let mapped_base = boot_info
        .runtime_hhdm_base
        .checked_add(framebuffer.base as u64)?;
    framebuffer.base = mapped_base as *mut u8;
    Some(framebuffer)
}

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

fn draw_label(canvas: &mut Canvas, x: i32, y: i32, text: &str, color: Color, scale: i32) {
    if scale <= 0 {
        return;
    }

    let mut cursor_x = x;
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        if let Some(pattern) = glyph_pattern(ch) {
            for (row, bits) in pattern.iter().enumerate() {
                for col in 0..3 {
                    if (bits >> (2 - col)) & 1 == 1 {
                        canvas.fill_rect(
                            cursor_x + col * scale,
                            y + row as i32 * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
        }
        cursor_x += 4 * scale;
    }
}

fn glyph_pattern(ch: char) -> Option<[u8; 5]> {
    match ch {
        'A' => Some([0b010, 0b101, 0b111, 0b101, 0b101]),
        'B' => Some([0b110, 0b101, 0b110, 0b101, 0b110]),
        'C' => Some([0b011, 0b100, 0b100, 0b100, 0b011]),
        'D' => Some([0b110, 0b101, 0b101, 0b101, 0b110]),
        'E' => Some([0b111, 0b100, 0b110, 0b100, 0b111]),
        'F' => Some([0b111, 0b100, 0b110, 0b100, 0b100]),
        'G' => Some([0b011, 0b100, 0b101, 0b101, 0b011]),
        'H' => Some([0b101, 0b101, 0b111, 0b101, 0b101]),
        'I' => Some([0b111, 0b010, 0b010, 0b010, 0b111]),
        'J' => Some([0b001, 0b001, 0b001, 0b101, 0b010]),
        'K' => Some([0b101, 0b101, 0b110, 0b101, 0b101]),
        'L' => Some([0b100, 0b100, 0b100, 0b100, 0b111]),
        'M' => Some([0b111, 0b111, 0b101, 0b101, 0b101]),
        'N' => Some([0b101, 0b111, 0b111, 0b111, 0b101]),
        'O' => Some([0b010, 0b101, 0b101, 0b101, 0b010]),
        'P' => Some([0b110, 0b101, 0b110, 0b100, 0b100]),
        'Q' => Some([0b010, 0b101, 0b101, 0b010, 0b001]),
        'R' => Some([0b110, 0b101, 0b110, 0b101, 0b101]),
        'S' => Some([0b011, 0b100, 0b010, 0b001, 0b110]),
        'T' => Some([0b111, 0b010, 0b010, 0b010, 0b010]),
        'U' => Some([0b101, 0b101, 0b101, 0b101, 0b111]),
        'V' => Some([0b101, 0b101, 0b101, 0b101, 0b010]),
        'W' => Some([0b101, 0b101, 0b101, 0b111, 0b111]),
        'X' => Some([0b101, 0b101, 0b010, 0b101, 0b101]),
        'Y' => Some([0b101, 0b101, 0b010, 0b010, 0b010]),
        '0' => Some([0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => Some([0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => Some([0b110, 0b001, 0b010, 0b100, 0b111]),
        '3' => Some([0b110, 0b001, 0b010, 0b001, 0b110]),
        '4' => Some([0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => Some([0b111, 0b100, 0b110, 0b001, 0b110]),
        '6' => Some([0b011, 0b100, 0b110, 0b101, 0b010]),
        '7' => Some([0b111, 0b001, 0b010, 0b100, 0b100]),
        '8' => Some([0b010, 0b101, 0b010, 0b101, 0b010]),
        '9' => Some([0b010, 0b101, 0b011, 0b001, 0b110]),
        ' ' => Some([0, 0, 0, 0, 0]),
        '-' => Some([0, 0, 0b111, 0, 0]),
        '/' => Some([0b001, 0b001, 0b010, 0b100, 0b100]),
        _ => None,
    }
}
