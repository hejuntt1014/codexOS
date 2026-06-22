#![no_std]
#![no_main]

use bootinfo::BootInfo;
use core::panic::PanicInfo;
use kernel::input::Ps2Keyboard;
use kernel::{DesktopApp, interrupts};

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
    abi_version: 2,
    reserved: 0,
    entry_hint: 0,
};

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_BANNER: [u8; 23] = *b"codexOS kernel image v2";

#[unsafe(no_mangle)]
/// Enters the resident kernel after the loader has exited UEFI boot services.
///
/// # Safety
///
/// `boot_info` must point to an initialized `BootInfo` that remains valid for
/// the lifetime of the kernel. The active page tables must contain the kernel
/// image mappings and the higher-half aliases described by that structure.
pub unsafe extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    kernel::serial::init();
    let Some(boot_info) = (unsafe { boot_info.as_ref() }) else {
        kernel::serial_println!("standalone boot info missing");
        interrupts::halt();
    };
    kernel::serial_println!("standalone boot info present");

    let root = match kernel::init_standalone(boot_info) {
        Ok(root) => root,
        Err(error) => {
            kernel::serial_println!("standalone initialization failed: {:?}", error);
            interrupts::halt();
        }
    };
    kernel::serial_println!("standalone runtime root=0x{:016x}", root);

    let mut desktop = DesktopApp::new(boot_info);
    desktop.note_handoff_complete();
    desktop.render(boot_info);
    kernel::serial_println!("standalone desktop rendered");

    if interrupts::activate_hardware() {
        kernel::serial_println!("standalone timer interrupts active");
    } else {
        kernel::serial_println!("standalone timer interrupt activation failed");
        interrupts::halt();
    }

    let mut keyboard = Ps2Keyboard::new();
    kernel::serial_println!("standalone keyboard polling active");
    loop {
        let mut received_input = false;
        while let Some(input) = keyboard.poll_input() {
            desktop.handle_input(input);
            received_input = true;
        }

        if desktop.needs_redraw() {
            desktop.render(boot_info);
        }
        if desktop.should_exit() {
            kernel::serial_println!("standalone desktop requested shutdown");
            interrupts::halt();
        }

        if !received_input {
            interrupts::wait_for_interrupt();
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    kernel::serial::init();
    kernel::serial_println!("[PANIC] {}", info);
    interrupts::halt()
}
