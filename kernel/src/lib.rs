#![no_std]

extern crate alloc;

mod allocator;
pub mod input;

pub use boot_runtime::{boot, interrupts, memory, serial, vm};
pub use desktop_runtime::{DesktopApp, DesktopInput, PointerSample};

use bootinfo::{BootInfo, ReservedMemoryKind};
use heap_allocator::HeapInitError;

#[global_allocator]
static ALLOCATOR: allocator::KernelHeap = allocator::KernelHeap::new();

pub fn init(boot_info: &BootInfo) {
    boot_runtime::init(boot_info);
}

pub fn activate_post_ebs_vm(boot_info: &BootInfo) -> Result<u64, vm::VmError> {
    boot_runtime::activate_post_ebs_vm(boot_info)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandaloneInitError {
    VirtualMemory(vm::VmError),
    KernelHeapMissing,
    KernelHeapAddressOverflow,
    KernelHeapInitialization(HeapInitError),
    ExceptionPathUnavailable,
}

impl From<vm::VmError> for StandaloneInitError {
    fn from(error: vm::VmError) -> Self {
        Self::VirtualMemory(error)
    }
}

pub fn init_standalone(boot_info: &BootInfo) -> Result<u64, StandaloneInitError> {
    serial::init();
    log_line(format_args!("codexOS standalone kernel entered"));
    let root = boot_runtime::adopt_current_post_ebs_vm(boot_info)?;
    if !interrupts::verify_exception_path() {
        return Err(StandaloneInitError::ExceptionPathUnavailable);
    }
    log_line(format_args!("standalone exception path verified"));
    let interrupt_status = interrupts::status();
    log_line(format_args!(
        "standalone descriptor tables: gdt={} tr=0x{:04x} idt={}",
        interrupt_status.gdt_loaded, interrupt_status.task_register, interrupt_status.idt_loaded
    ));

    let heap = boot_info
        .reserved_memory()
        .iter()
        .find(|range| range.kind == ReservedMemoryKind::KernelHeap)
        .ok_or(StandaloneInitError::KernelHeapMissing)?;
    let heap_base =
        usize::try_from(heap.start).map_err(|_| StandaloneInitError::KernelHeapAddressOverflow)?;
    let heap_size =
        usize::try_from(heap.length).map_err(|_| StandaloneInitError::KernelHeapAddressOverflow)?;
    ALLOCATOR
        .initialize_external(heap_base as *mut u8, heap_size)
        .map_err(StandaloneInitError::KernelHeapInitialization)?;

    let stats = ALLOCATOR.stats();
    log_line(format_args!(
        "standalone heap: capacity={} MiB free={} MiB bootstrap-used={} KiB",
        stats.capacity_bytes / (1024 * 1024),
        stats.free_bytes / (1024 * 1024),
        stats.used_bytes / 1024
    ));
    Ok(root)
}

pub fn log_line(args: core::fmt::Arguments<'_>) {
    boot_runtime::log_line(args);
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
