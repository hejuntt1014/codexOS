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
- a tiny `kernel` crate that renders a desktop-like GUI scene
- a shared `bootinfo` crate for framebuffer handoff
- a `gfx` crate for primitive 2D drawing
- an `xtask` runner for build/image/run/debug workflows

## Commands

```powershell
cargo xtask env
cargo xtask build
cargo xtask image
cargo xtask run
cargo xtask debug
cargo xtask smoke
```

## Next milestones

1. Exit UEFI boot services cleanly and own the whole machine lifecycle.
2. Add input handling for keyboard and mouse.
3. Introduce memory management and a heap.
4. Split the loader and kernel into separate binaries.
5. Add a task model, syscall ABI, and user-space apps.

See `docs/debugging.md` for the debugging workflow.
