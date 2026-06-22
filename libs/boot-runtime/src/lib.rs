#![no_std]

extern crate alloc;

#[path = "../../../kernel/src/serial.rs"]
pub mod serial;

#[path = "../../../kernel/src/memory.rs"]
pub mod memory;

#[path = "../../../kernel/src/vm.rs"]
pub mod vm;

#[path = "../../../kernel/src/interrupts.rs"]
pub mod interrupts;

#[path = "../../../kernel/src/boot.rs"]
pub mod boot;

use alloc::string::String;
use bootinfo::{BootInfo, FirmwareMode};

pub fn init(boot_info: &BootInfo) {
    serial::init();
    memory::init(boot_info);
    let idt_loaded = if matches!(boot_info.firmware_mode, FirmwareMode::PostExitBootServices) {
        interrupts::init_idt()
    } else {
        false
    };
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
    let interrupt_status = interrupts::status();
    log_line(format_args!(
        "interrupts: gdt={} tr=0x{:04x} idt={} hw={} ticks={} hz={}",
        if interrupt_status.gdt_loaded {
            "loaded"
        } else {
            "firmware"
        },
        interrupt_status.task_register,
        if idt_loaded || interrupt_status.idt_loaded {
            "loaded"
        } else {
            "missing"
        },
        if interrupt_status.hardware_enabled {
            "on"
        } else {
            "off"
        },
        interrupt_status.ticks,
        interrupt_status.timer_hz
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

pub fn activate_post_ebs_vm(boot_info: &BootInfo) -> Result<u64, vm::VmError> {
    if !matches!(boot_info.firmware_mode, FirmwareMode::PostExitBootServices) {
        return Err(vm::VmError::FirmwareServicesActive);
    }

    log_line(format_args!("vm activation: preparing boot map"));
    let report = vm::prepare_boot_identity_map(boot_info)?;
    log_line(format_args!(
        "vm boot map: ident={} ranges/{} pages kernel={} ranges/{} pages stack=0x{:016x}/{} pages hhdm={} ranges/{} pages @ 0x{:016x}",
        report.identity_ranges,
        report.identity_pages,
        report.kernel_image_ranges,
        report.kernel_image_pages,
        report.stack_window_start,
        report.stack_window_pages,
        report.higher_half_ranges,
        report.higher_half_pages,
        report.higher_half_base
    ));
    log_line(format_args!(
        "vm kernel permissions: writable={} executable={} wx={}",
        report.kernel_writable_pages, report.kernel_executable_pages, report.kernel_wx_pages
    ));

    let root = vm::activate()?;
    boot::record_post_ebs_active(boot_info, root);
    log_line(format_args!(
        "vm switched to kernel page tables at 0x{:016x}",
        root
    ));
    if !interrupts::verify_exception_path() {
        log_line(format_args!(
            "interrupts: exception path verification failed"
        ));
        interrupts::halt();
    }
    log_line(format_args!("interrupts: exception path verified"));
    let interrupt_status = interrupts::status();
    log_line(format_args!(
        "interrupts: hw=deferred ticks={} hz={}",
        interrupt_status.ticks, interrupt_status.timer_hz
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
    Ok(root)
}

pub fn adopt_current_post_ebs_vm(boot_info: &BootInfo) -> Result<u64, vm::VmError> {
    if !matches!(boot_info.firmware_mode, FirmwareMode::PostExitBootServices) {
        return Err(vm::VmError::FirmwareServicesActive);
    }

    memory::init(boot_info);
    interrupts::init_idt();
    boot::record_firmware_entry();

    let root = vm::adopt_current_root()?;
    boot::record_post_ebs_active(boot_info, root);
    log_line(format_args!(
        "standalone vm adopted current page tables at 0x{:016x}",
        root
    ));
    Ok(root)
}

fn format_reserved_alias(alias: Option<boot::ReservedAlias>) -> String {
    use alloc::format;

    match alias {
        Some(alias) => format!(
            "0x{:016x}->0x{:016x} ({} KiB)",
            alias.range.start,
            alias.virt,
            alias.range.length / 1024
        ),
        None => String::from("n/a"),
    }
}

pub fn log_line(args: core::fmt::Arguments<'_>) {
    serial::print(args);
    serial::print(format_args!("\r\n"));
}
