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
cargo xtask debug
cargo xtask smoke
```

`cargo xtask debug` starts QEMU paused and exposes a gdb stub on TCP port `1234`.
`cargo xtask smoke` runs QEMU headlessly for a few seconds and verifies the serial log automatically.

## Current debug signals

- Serial line `codexOS kernel entered` confirms the handoff into the kernel crate.
- A visible desktop scene confirms that GOP framebuffer access works.

## Recommended next debug upgrades

- Add a frame counter and panic screen.
- Write logs to both serial and an in-memory ring buffer.
- Add `-d int,cpu_reset` QEMU modes for low-level bring-up.
- Add screenshot-based regression tests for the desktop scene.
