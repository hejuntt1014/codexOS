# codexOS

`codexOS` is a bootable Rust GUI OS baseline that runs as a UEFI application in QEMU.
This first milestone focuses on a real development loop:

- build with `cargo`
- package a bootable FAT image
- launch in `QEMU + edk2`
- render a framebuffer desktop scene
- emit serial logs for debugging

## Current status

The repository currently implements:

- a `uefi-loader` boot image for `x86_64-unknown-uefi`
- a separate `kernel-image` ELF built for `x86_64-unknown-none` and packed into the boot disk as `KERNEL.ELF`
- an alternate `handoff` loader mode that exits UEFI boot services before rendering the desktop
- a resident `chainload` mode that stages the standalone kernel, switches to kernel page tables, transfers ownership of a reserved 64 MiB kernel heap, and jumps into the standalone entry point
- a shared `boot-runtime` layer that now owns early serial logging, physical-page discovery, boot-state tracking, and boot-VM activation for both the loader and the resident kernel path
- a shared `desktop-runtime` layer that now owns the GUI desktop loop, so `uefi-loader` no longer links the resident `kernel` crate just to render the desktop
- a shared reclaiming heap allocator with aligned allocation, deallocation, free-block coalescing, exhaustion accounting, and host-side tests
- a 512 KiB loader bootstrap heap that switches to a UEFI-allocated 64 MiB runtime heap without inflating the EFI executable
- a `kernel` crate with a reclaiming global heap, physical page allocator, and stateful desktop runtime
- a loader-side ELF64 loader that stages `PT_LOAD` segments, applies checked `R_X86_64_RELATIVE` relocations, rejects unsupported relocations, and records the staged entry point and relocation count in boot info
- an early virtual-memory manager that activates handoff page tables via `CR3`, verifies a boot-time higher-half direct map, maps HHDM memory non-executable, applies per-page ELF permissions, rejects writable-executable kernel pages, and trims identity mappings to the reserved boot footprint
- a shared `bootinfo` crate for framebuffer, boot-state, reserved-memory, and compact memory-map handoff
- a `gfx` crate for primitive 2D drawing plus bitmap text rendering
- an `xtask` runner for build/image/run/debug workflows
- a live desktop loop with keyboard focus movement and mouse dragging
- a tested PS/2 Set-1 keyboard decoder for the firmware-detached kernel, including modifiers, printable keys, controls, and extended arrows
- a 100 Hz PIT timer and interrupt-driven `hlt` idle path in the resident kernel
- kernel-owned GDT and TSS state with a dedicated 64 KiB IST stack for double faults, independent of firmware descriptor tables
- correct stubs for all 32 architectural exception vectors, page-fault access diagnostics, and a boot-time breakpoint/`iretq` self-test across both UEFI and SysV calling conventions
- a terminal window with command input, history, and built-in shell commands
- a loader-collected UEFI memory map surfaced in the kernel, inspector, and shell

## Commands

```powershell
cargo xtask env
cargo xtask build
cargo xtask build-handoff
cargo xtask build-chainload
cargo xtask image
cargo xtask image-handoff
cargo xtask image-chainload
cargo xtask run
cargo xtask handoff
cargo xtask chainload
cargo xtask debug
cargo xtask smoke
cargo xtask smoke-handoff
cargo xtask smoke-chainload
```

## Current controls

- Arrow keys: move the focused window
- `Tab`: switch focus between windows
- Type in the terminal window to enter commands
- `Enter`: run the current command
- `Backspace`: delete one character
- Mouse left button: drag a window by its title bar
- Mouse right button: switch focus
- `Esc`: exit back to firmware in interactive UEFI mode; halt safely after firmware services have ended

## Shell commands

- `help`
- `status`
- `boot`
- `kernel`
- `mem`
- `reserved`
- `regions`
- `region <index>`
- `alloc-page`
- `alloc <count>`
- `vm`
- `vm-sync`
- `map-test`
- `map <count>`
- `translate <address>`
- `walk <address>`
- `hhdm <physical-address>`
- `fb`
- `aliases`
- `phases`
- `pt-entry <table-phys> <index>`
- `theme`
- `clear`
- `focus`
- `move-left`
- `move-right`
- `move-up`
- `move-down`
- `about`
- `exit`

## Commercial-readiness boundary

This repository is a real bootable system, but it is not yet suitable for production deployment. Its kernel image has enforced W^X page permissions and the release images pass real UEFI/QEMU smoke boots, but it still lacks process isolation and a syscall ABI, persistent storage and a filesystem driver, networking, a complete hardware abstraction layer, an installer and recovery environment, signed updates, and a security maintenance process. The standalone chainload path has PS/2 keyboard polling, while general HID and post-firmware pointer input are not implemented.

See `docs/debugging.md` for the debugging workflow.
