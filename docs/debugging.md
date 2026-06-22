# Debugging

## Environment

- Rust target: `x86_64-unknown-uefi`
- Firmware: `edk2-x86_64-code.fd`
- Emulator: `qemu-system-x86_64`
- Serial output: COM1 redirected to QEMU stdio

## Useful commands

```powershell
cargo xtask env
cargo xtask run
cargo xtask handoff
cargo xtask chainload
cargo xtask debug
cargo xtask smoke
cargo xtask smoke-handoff
cargo xtask smoke-chainload
```

`cargo xtask debug` starts QEMU paused and exposes a gdb stub on TCP port `1234`.
`cargo xtask smoke` runs QEMU headlessly for a few seconds and verifies the serial log automatically.
`cargo xtask smoke-handoff` verifies the post-`ExitBootServices` path and checks for `boot mode: post-exit-boot-services`.
`cargo xtask smoke-chainload` verifies the resident kernel handoff, page-table adoption, reserved heap initialization, desktop rendering, and PIT interrupt activation.

## Current debug signals

- Serial line `codexOS kernel entered` confirms the handoff into the kernel crate.
- A visible desktop scene confirms that GOP framebuffer access works.
- Serial line `memory map: ...` confirms the loader collected and handed off memory regions.
- Serial line `boot mode: ...` confirms whether the kernel is still under UEFI boot services or already firmware-detached.
- Serial line `reserved memory: ...` confirms the loader marked the image, framebuffer, and memory-map buffer as in-use.
- Serial line `kernel image: ...` confirms the loader found `KERNEL.ELF`, parsed the ELF header, allocated pages for its loadable segments, and computed a staged entry address.
- Serial line `codexOS standalone kernel entered` confirms the chainload mode jumped into the resident kernel image after the loader-built boot VM was activated.
- Serial lines for page-table adoption, standalone heap capacity, and active timer interrupts confirm that the resident kernel took ownership of memory and CPU idle scheduling.
- Serial line `vm: root table at ... synced` confirms the kernel wrote the current page-table image into physical pages.
- Serial line `vm boot map: ...` confirms the handoff path built an identity-mapped boot address space plus a boot-time higher-half direct-map window.
- The `stack=...` portion of `vm boot map: ...` shows the explicit identity-mapped stack window kept alive after handoff.
- Serial line `vm switched to kernel page tables at ...` confirms the handoff path successfully loaded the new `CR3`.
- Serial line `vm hhdm probe: ...` confirms the kernel can read the active root page table through the higher-half direct map after switching.
- Serial line `vm framebuffer hhdm: ...` confirms the framebuffer has a usable higher-half direct-map address after switching.
- Serial line `vm reserved hhdm: ...` confirms the loader image and memory-map buffer both have higher-half aliases after switching.
- Serial line `page allocator: ...` confirms the kernel turned usable regions into page-allocation state.
- Serial line `vm: root table at ...` confirms the kernel initialized the virtual-memory manager.
- Serial line `pointer input: available` confirms mouse support was discovered.

## Interaction smoke

The current interactive desktop supports:

- arrow-key window movement
- `Tab` focus cycling
- terminal command entry with `Enter` and `Backspace`
- mouse title-bar dragging when a pointer device is exposed by UEFI

## Shell commands

The built-in terminal currently supports:

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

## Recommended next debug upgrades

- Add a frame counter and panic screen.
- Write logs to both serial and an in-memory ring buffer.
- Add `-d int,cpu_reset` QEMU modes for low-level bring-up.
- Add screenshot-based regression tests for the desktop scene.
