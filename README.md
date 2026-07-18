# codexOS

`codexOS` is a bootable Rust GUI operating-system project with a resident kernel path under QEMU/UEFI.
The repository has a repeatable development and verification loop:

- build with `cargo`
- package a bootable FAT image
- launch in `QEMU + edk2`
- render a framebuffer desktop scene
- emit serial logs for debugging

## Built with Codex

This project was vibe-coded end-to-end with **OpenAI Codex**. Development started on **GPT-5.4**, then moved forward with each model release through **GPT-5.5** and **GPT-5.6**. Later coding used the latest **GPT-5.6**.

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
- a firmware-detached PS/2 auxiliary mouse driver that initializes the controller, enables streaming packets, decodes signed motion/buttons, feeds the desktop pointer path, and is exercised through QEMU monitor injection
- a 100 Hz PIT timer and interrupt-driven `hlt` idle path in the resident kernel
- kernel-owned GDT and TSS state with a dedicated 64 KiB IST stack for double faults, independent of firmware descriptor tables
- correct stubs for all 32 architectural exception vectors, page-fault access diagnostics, and a boot-time breakpoint/`iretq` self-test across both UEFI and SysV calling conventions
- full NX higher-half mappings for reclaimable RAM plus explicit page-table ownership transfer in `BootInfo`, preventing the resident allocator from reusing active translation structures
- isolated Ring 3 processes with separate CR3 roots and kernel interrupt stacks, supervisor-only kernel mappings, user RX code, guarded RW/NX stacks, a versioned `int 0x80` syscall ABI, checked user buffers, PIT-preemptive round-robin scheduling, timed sleep/wakeup, low-power idle waiting, process exit, user-fault containment, and full address-space reclamation
- a kernel PCI hardware inventory layer that scans multifunction devices, classifies storage/network/display/bridge/USB/input controllers, records legacy virtio boot devices, and verifies that the block and network drivers bind to the same PCI functions reported by the inventory
- a PCI legacy `virtio-blk` driver with contiguous DMA queues, negotiated flush support, bounded request completion, sector-range checks, and synchronous read/write/flush operations
- CodexFS, an append-only snapshot filesystem with validated absolute paths, versioned directory and file metadata, owner permission bits, directory listing/removal checks, dynamic multi-sector records for files beyond the original 32 KiB snapshot size, per-record and whole-record CRC32 integrity, alternating superblocks, ordered data/metadata flushes, corruption refusal, old-record migration, and fallback to the newest valid generation
- a separate 64 MiB persistent data image that is preserved across QEMU runs and protected against implicit resizing
- a boot-time durable counter transaction with post-flush reread verification; `smoke-chainload` performs two cold boots and requires a continuous on-disk counter before passing
- a kernel ELF64 user-program loader that validates x86-64 `PT_LOAD` segments, rejects writable-executable pages, checks entry-point reachability, maps each segment into an isolated Ring 3 address space, and runs a SHA-256 verified executable read from CodexFS
- a resident PCI legacy `virtio-net` driver with independent RX/TX DMA virtqueues, checked device completions, bounded waits, and a continuously posted receive buffer
- checked Ethernet II, ARP, IPv4, UDP, TCP, DHCP, DNS, and ICMP paths: the kernel obtains a dynamic lease, learns the resolver, validates IPv4 and transport checksums, resolves the default gateway, completes a DNS A-record query, proves a TCP three-way handshake plus HTTP response, proves an ICMP echo round trip, then remains online to answer ARP, ICMP, and host-forwarded TCP/HTTP listener requests from its desktop loop
- a canonical signed-release format with SHA-256 payload identity, Ed25519 verification, embedded loader bootstrap trust root, manifest-carried trust-root transition, persisted UEFI trust-root state, and a persistent UEFI anti-rollback floor
- dual A/B kernel slots selected by alternating CRC-protected boot-state records, with automatic signed-slot fallback when the active copy is damaged and signed-slot scanning recovery when both boot-state records are damaged
- readback-verified image installation and an offline updater that commits the inactive slot before switching generations, then verifies the redundant copy across whole-disk FAT and GPT/ESP installed images
- GPT disk images with a protective MBR, primary and backup GPT headers, CRC-verified EFI System Partition entries, a FAT-formatted ESP containing the signed A/B boot set, and QEMU/OVMF smoke coverage for booting through that standard UEFI layout
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
cargo xtask image-gpt-chainload
cargo xtask run
cargo xtask handoff
cargo xtask chainload
cargo xtask debug
cargo xtask smoke
cargo xtask smoke-handoff
cargo xtask smoke-chainload
cargo xtask smoke-security
cargo xtask smoke-trust-rotation
cargo xtask smoke-network-listener
cargo xtask smoke-pointer-input
cargo xtask smoke-gpt-esp
cargo xtask smoke-recovery
cargo xtask smoke-bootstate-recovery
cargo xtask smoke-install
cargo xtask smoke-gpt-install
cargo xtask install <destination-image>
cargo xtask install-gpt <destination-image>
cargo xtask apply-update <installed-image>
cargo xtask release-image <destination-image>
cargo xtask release-gpt-image <destination-image>
cargo xtask derive-public-key <seed> <public-key>
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

This repository is a real bootable system, but it is not yet suitable for production deployment. Its kernel image has enforced W^X permissions, hardware-verified Ring 3 isolation, a preemptive multi-process scheduler, timed blocking and idle wakeup, address-space reclamation, syscall buffer validation, boot-time PCI hardware inventory with driver-binding checks, durable virtio block storage, a corruption-detecting filesystem with directories, owner permissions, and dynamic multi-sector file records, a SHA-256 verified ELF64 user-program path from persistent storage, dynamically configured IPv4 networking with DNS A-record resolution, active TCP/HTTP client exchange, host-forwarded TCP/HTTP listener service, firmware-detached PS/2 keyboard and pointer input, signed A/B updates, trust-root transition through signed manifests, GPT/ESP release images, GPT/ESP-aware offline updates, automatic fallback recovery, and release images that pass real UEFI/QEMU smoke boots. Production readiness remains blocked by general application packaging and signing policy, extent allocation for very large filesystem datasets, richer access-control semantics, a general socket API abstraction with multi-connection service policy and TLS, broader physical-machine storage/network/USB HID driver coverage, direct writes to user-selected physical disks, UEFI Secure Boot enrollment for the loader, hardware-backed signing, authenticated firmware variable policy, and independent security assessment.

See `docs/processes.md` for process architecture and the syscall ABI, and `docs/debugging.md` for the verification workflow.
