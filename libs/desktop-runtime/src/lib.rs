#![no_std]

extern crate alloc;

pub use boot_runtime::{boot, memory, vm};

#[path = "../../../kernel/src/desktop.rs"]
mod desktop;

pub use desktop::{DesktopApp, DesktopInput, PointerSample};
