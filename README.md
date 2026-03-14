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
- a `chainload` validation mode that stages the standalone kernel at its linked address, switches to the boot VM, and jumps into the standalone entry point
- a `kernel` crate with a simple bump allocator, physical page allocator, and stateful desktop runtime
- a loader-side ELF inspection and staging step that parses the standalone kernel image, allocates pages for its `PT_LOAD` segments, copies them into memory, and surfaces the staged entry point in boot info, logs, and the desktop shell
- an early virtual-memory manager that allocates page-table pages, tracks demo mappings, syncs them into physical memory, activates a handoff page table via `CR3`, verifies a boot-time higher-half direct-map window, and trims the post-handoff identity map to a small boot footprint
- a shared `bootinfo` crate for framebuffer, boot-state, reserved-memory, and compact memory-map handoff
- a `gfx` crate for primitive 2D drawing plus bitmap text rendering
- an `xtask` runner for build/image/run/debug workflows
- a live desktop loop with keyboard focus movement and mouse dragging
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
- `Esc`: exit back to firmware

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

## Next milestones

1. Replace the bump allocator with a real heap manager and make VM mappings back real page tables.
2. Replace the chainload validation path with a full standalone-kernel handoff that does not rely on the current kernel crate staying resident.
3. Add PS/2 or HID input drivers so interaction survives after leaving UEFI services.
4. Replace the mirrored page-table model with a richer mapped-kernel address space.
5. Add a higher-half kernel layout instead of relying on temporary identity mappings.
6. Add a task model, syscall ABI, and user-space apps.

See `docs/debugging.md` for the debugging workflow.
