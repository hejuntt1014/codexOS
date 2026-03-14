#![no_std]

extern crate alloc;

mod allocator;
mod boot;
mod desktop;
mod memory;
mod serial;
mod vm;

use bootinfo::{BootInfo, FirmwareMode};
pub use desktop::{DesktopApp, DesktopInput, PointerSample};

#[global_allocator]
static ALLOCATOR: allocator::BumpAllocator = allocator::BumpAllocator;

pub fn init(boot_info: &BootInfo) {
    serial::init();
    memory::init(boot_info);
    boot::record_firmware_entry();
    log_line(format_args!("codexOS kernel entered"));
    log_line(format_args!(
        "framebuffer: {}x{} stride={} bpp={} bytes={} format={:?}",
        boot_info.framebuffer.width,
        boot_info.framebuffer.height,
        boot_info.framebuffer.stride,
        boot_info.framebuffer.bytes_per_pixel,
        boot_info.framebuffer.size,
        boot_info.framebuffer.pixel_format
    ));
    log_line(format_args!(
        "memory map: stored={} total={} usable={} MiB total={} MiB",
        boot_info.memory_region_count,
        boot_info.memory_region_total,
        boot_info.usable_memory_bytes() / (1024 * 1024),
        boot_info.total_memory_bytes() / (1024 * 1024)
    ));
    log_line(format_args!(
        "boot mode: {} reserved={} KiB",
        boot_info.firmware_mode.as_str(),
        boot_info.reserved_memory_bytes() / 1024
    ));
    let stats = memory::stats();
    log_line(format_args!(
        "page allocator: regions={} pages={} remaining={} first-kind={}",
        stats.usable_region_count,
        stats.total_usable_pages,
        stats.remaining_pages,
        memory::first_usable_kind().as_str()
    ));
    match vm::init() {
        Some(root) => match vm::sync() {
            Ok(_) => {
                boot::record_vm_initialized(root);
                log_line(format_args!("vm: root table at 0x{:016x} synced", root))
            }
            Err(err) => log_line(format_args!(
                "vm: root table at 0x{:016x} sync failed: {:?}",
                root, err
            )),
        },
        None => log_line(format_args!("vm: failed to allocate root table")),
    }
}

pub fn activate_post_ebs_vm(boot_info: &BootInfo) {
    if !matches!(boot_info.firmware_mode, FirmwareMode::PostExitBootServices) {
        log_line(format_args!(
            "vm activation skipped: still under boot services"
        ));
        return;
    }

    match vm::prepare_boot_identity_map(boot_info) {
        Ok(report) => log_line(format_args!(
            "vm boot map: ident={} ranges/{} pages stack=0x{:016x}/{} pages hhdm={} ranges/{} pages @ 0x{:016x}",
            report.identity_ranges,
            report.identity_pages,
            report.stack_window_start,
            report.stack_window_pages,
            report.higher_half_ranges,
            report.higher_half_pages,
            report.higher_half_base
        )),
        Err(err) => {
            log_line(format_args!("vm boot map failed: {:?}", err));
            return;
        }
    }

    match vm::activate() {
        Ok(root) => {
            boot::record_post_ebs_active(boot_info, root);
            log_line(format_args!(
                "vm switched to kernel page tables at 0x{:016x}",
                root
            ));
            let snapshot = boot::snapshot();
            match snapshot.hhdm_probe {
                Some(probe) => log_line(format_args!(
                    "vm hhdm probe: root=0x{:016x} entry0=0x{:016x}",
                    probe.virt_addr, probe.root_entry0
                )),
                None => log_line(format_args!("vm hhdm probe unavailable")),
            }
            if let Some(framebuffer) = snapshot.framebuffer_alias {
                log_line(format_args!(
                    "vm framebuffer hhdm: phys=0x{:016x} virt=0x{:016x}",
                    boot_info.framebuffer.base as u64, framebuffer
                ));
            }
            log_line(format_args!(
                "vm reserved hhdm: loader={} memmap={}",
                format_reserved_alias(snapshot.loader_alias),
                format_reserved_alias(snapshot.memory_map_alias)
            ));
        }
        Err(err) => log_line(format_args!("vm activation failed: {:?}", err)),
    }
}

fn format_reserved_alias(alias: Option<boot::ReservedAlias>) -> alloc::string::String {
    use alloc::format;

    match alias {
        Some(alias) => format!(
            "0x{:016x}->0x{:016x} ({} KiB)",
            alias.range.start,
            alias.virt,
            alias.range.length / 1024
        ),
        None => format!("n/a"),
    }
}

pub fn log_line(args: core::fmt::Arguments<'_>) {
    serial::print(args);
    serial::print(format_args!("\r\n"));
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
