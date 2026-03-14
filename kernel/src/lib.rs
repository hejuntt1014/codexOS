#![no_std]

extern crate alloc;

mod allocator;

pub use boot_runtime::{boot, memory, serial, vm};
pub use desktop_runtime::{DesktopApp, DesktopInput, PointerSample};

use bootinfo::BootInfo;

#[global_allocator]
static ALLOCATOR: allocator::BumpAllocator = allocator::BumpAllocator;

pub fn init(boot_info: &BootInfo) {
    boot_runtime::init(boot_info);
}

pub fn activate_post_ebs_vm(boot_info: &BootInfo) {
    boot_runtime::activate_post_ebs_vm(boot_info);
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
