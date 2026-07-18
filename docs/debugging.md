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
cargo xtask smoke-security
cargo xtask smoke-trust-rotation
cargo xtask smoke-network-listener
cargo xtask smoke-pointer-input
cargo xtask smoke-gpt-esp
cargo xtask smoke-recovery
cargo xtask smoke-bootstate-recovery
cargo xtask smoke-install
cargo xtask smoke-gpt-install
```

`cargo xtask debug` starts QEMU paused and exposes a gdb stub on TCP port `1234`.
`cargo xtask smoke` runs QEMU headlessly for a few seconds and verifies the serial log automatically.
`cargo xtask smoke-handoff` verifies the post-`ExitBootServices` path and checks for `boot mode: post-exit-boot-services`.
`cargo xtask smoke-chainload` verifies the resident kernel handoff, page-table ownership, reserved heap initialization, Ring 3 syscalls and isolation faults, PIT-preemptive multi-process scheduling, timed sleep/wakeup, low-power idle entry, address-space reclamation, SHA-256 verified ELF64 execution from CodexFS, directory metadata migration, dynamic multi-sector CodexFS file commits, desktop rendering, two-boot persistence continuity, DHCP configuration, ARP gateway resolution, DNS A-record resolution, TCP three-way handshake plus HTTP response, a TCP/HTTP listener being armed, and an ICMP echo exchange.
`cargo xtask smoke-network-listener` starts QEMU with a temporary host TCP port forwarded to guest port `8080`, completes four host TCP handshakes before sending any request, requires all four connections to receive HTTP 200 responses, and requires the kernel to report both four served connections and four completed client close handshakes.
`cargo xtask smoke-pointer-input` starts QEMU with a TCP monitor, waits for the resident PS/2 pointer driver to enable streaming, injects a mouse movement, and requires the kernel serial event to report nonzero motion.
`cargo xtask smoke-gpt-esp` builds a protective-MBR/GPT disk, validates primary and backup GPT CRCs plus the EFI System Partition contents, then boots it through OVMF.
`cargo xtask smoke-recovery` corrupts active slot A, requires the first boot to verify slot B and persist generation 3 selecting B, then cold-boots again and requires a direct normal boot from B without retrying A.
`cargo xtask smoke-bootstate-recovery` damages both boot-state records, requires the loader to scan signed system slots and rebuild both records, then cold-boots again and requires a normal generation-2 boot.
`cargo xtask smoke-gpt-install` installs a GPT/ESP image, applies a signed offline update through the installed ESP, boots the new slot, corrupts that slot, and proves signed fallback from the other slot.

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
- Serial line `vm boot map: ...` confirms the handoff path built a restricted identity map plus an NX higher-half direct map for reclaimable RAM.
- Serial line `vm ownership: ...` confirms active page-table allocations were recorded in `BootInfo` before resident allocator reinitialization.
- The `stack=...` portion of `vm boot map: ...` shows the explicit identity-mapped stack window kept alive after handoff.
- Serial line `vm switched to kernel page tables at ...` confirms the handoff path successfully loaded the new `CR3`.
- Serial line `vm hhdm probe: ...` confirms the kernel can read the active root page table through the higher-half direct map after switching.
- Serial line `vm framebuffer hhdm: ...` confirms the framebuffer has a usable higher-half direct-map address after switching.
- Serial line `vm reserved hhdm: ...` confirms the loader image and memory-map buffer both have higher-half aliases after switching.
- Serial line `page allocator: ...` confirms the kernel turned usable regions into page-allocation state.
- Serial lines `process[1]: user log: ...`, `kernel write denied ... err=0x7`, and `process isolation verified: ...` prove a Ring 3 syscall completed, a user write to kernel text faulted with present/write/user bits, and the kernel contained the process fault and resumed.
- `scheduler verified: processes=3 ... timer-preemptions=... min-dispatches=... idle-halts=... reclaimed-pages=18 ...` proves three independent address spaces were fairly dispatched by PIT, the all-sleeping state entered hardware idle, the faulting process was contained, and every owned user page was returned.
- Serial line `persistent executable verified: path=/system/bin/scheduler-gate.elf ... exit=2001 ... sha256-prefix=...` proves the kernel read an ELF64 user executable from CodexFS, matched it against the expected SHA-256 digest, loaded it into a Ring 3 address space, observed timer sleep/wakeup, and reclaimed every owned user page.
- Serial line `hardware inventory: pci-devices=... overflow=false ... virtio-blk=... virtio-net=...` proves the resident kernel enumerated PCI functions through its hardware inventory layer and found the required boot storage and network devices.
- Serial lines `hardware driver binding: device=virtio-blk ... inventory-match=true` and `hardware driver binding: device=virtio-net ... inventory-match=true` prove the block and network drivers bound to the same PCI functions reported by the inventory.
- Serial line `virtio-blk: pci=...` confirms PCI discovery, a live legacy virtqueue, disk capacity, and negotiated flush support.
- Serial line `codexfs: mounted state=...` reports whether CodexFS formatted an empty disk or recovered an existing valid generation.
- Serial line `filesystem large-file verified: path=/system/large-proof.bin bytes=98304 ... record-sectors=...` proves CodexFS committed and reread a file larger than the original fixed 64-sector record size; the smoke gate requires `record-sectors` to exceed 64 and the checksum to match the kernel's generated proof bytes.
- Serial line `filesystem persistence: previous=... current=... generation=... directories=... verified=true` confirms the record and alternating superblock were flushed and reread successfully, and that the required persistent namespace directories exist.
- Output `persistence reboot proof passed: A -> B -> C` means the smoke runner cold-booted QEMU twice against the same data image and observed a continuous durable counter.
- Serial line `virtio-net: pci=...` confirms the resident kernel owns a live legacy virtio network device and its negotiated MAC address.
- Serial line `network configured: ipv4=...` reports the validated DHCP lease, subnet, gateway, DHCP server, DNS server, and bidirectional frame counts.
- Serial line `dns resolved: name=example.com ... answer=... query-id=0x4344` proves a checksummed UDP DNS A-record query completed through the configured resolver.
- Serial line `tcp http verified: host=example.com ... status=... bytes=... source-port=49153` proves a TCP active open, HTTP request, checksummed response, and final ACK completed through QEMU networking.
- Serial line `network tcp listener ready: port=8080 protocol=http capacity=8 idle-timeout-ticks=3000 ...` proves the bounded runtime TCP listener is armed with PIT-clocked idle reclamation after DHCP, DNS, ICMP, and active HTTP verification.
- Serial line `tcp listener served: port=8080 remote=... request-bytes=... response-bytes=... connections=...` proves the kernel accepted a host-forwarded TCP connection and returned an HTTP response.
- Serial line `tcp listener closed: count=... total=...` proves the kernel validated the client's final FIN, acknowledged it, and released that peer's connection slot.
- Serial line `tcp listener expired: count=... idle-timeout-ticks=3000` records bounded reclamation of peers that did not complete their handshake or shutdown.
- Serial line `standalone pointer polling active: device=ps2 enabled=true ...` proves the resident kernel initialized the PS/2 auxiliary mouse device after firmware services ended.
- Serial line `pointer input event: device=ps2 dx=... dy=... left=... right=...` proves a PS/2 mouse packet reached the kernel, was decoded into screen-space motion, and was delivered to the desktop pointer path.
- Serial lines `arp gateway verified: ...` and `icmp echo verified: ...` prove layer-2 resolution and a checksummed IPv4/ICMP round trip; the smoke gate requires network checks on each cold boot.
- Serial line `kernel signature: verified=true ...` identifies the accepted release, A/B slot, boot-state generation, recovery status, signing-key ID, and kernel hash prefix.
- Serial line `kernel trust root: source=... activation-version=... signer-key-id=...` identifies whether the loader used its embedded bootstrap root or a persisted UEFI trust root.
- Serial line `trust root update: activated version=... previous-source=... next-key-id=...` proves a signed release carried and persisted a replacement release-verification key.
- Serial line `system slot X rejected: ...` records why a slot was denied before execution; `system recovery fallback: ...` records the independently verified slot selected instead.
- Serial line `system recovery state repair: selected=B version=... generation=3 verified=true` proves the verified fallback selection was persisted and reread before execution.
- Serial line `system boot-state repair: selected=... version=... generation=2 copies=2 verified=true` proves the loader rebuilt and reread both damaged boot-state records.
- Serial line `system boot-state recovery: selected=... version=... source=signed-slot-scan repaired=true generation=2` proves the repaired state came from a verified signed system slot.
- `smoke-security` requires altered kernels, altered signatures, and signed rollback attempts to stop before `codexOS kernel entered`.
- `smoke-trust-rotation` requires a transition release to persist the replacement trust root, a later release signed by that replacement to boot, and a higher old-root release to stop before `codexOS standalone kernel entered`.
- `smoke-network-listener` requires QEMU host forwarding to hold four peer-isolated TCP connections open concurrently, deliver a request through each connection to guest port `8080`, receive four HTTP 200 responses, report `connections=4`, and complete client shutdown through `total=4`.
- `smoke-pointer-input` requires QEMU monitor input to generate a resident PS/2 pointer packet with nonzero movement in the kernel serial log.
- `smoke-gpt-esp` requires the disk image to contain a valid protective MBR, primary and backup GPT headers, a CRC-verified EFI System Partition entry, signed A/B kernel files inside that ESP, and a successful OVMF chainload boot.
- `smoke-recovery` requires the first cold boot to reject corrupted slot A, persist slot B as generation 3, and the second cold boot to report `slot=B state-gen=3 recovery=false` without another fallback.
- `smoke-bootstate-recovery` requires both boot-state records to be damaged, rebuilt as generations 1 and 2, and reused on the next cold boot with `state-gen=2 recovery=false`.
- `smoke-install` requires a full image readback hash, an inactive-slot version switch, and recovery from corruption of the newly selected slot.
- `smoke-gpt-install` requires the same install/update/recovery proof through a GPT disk and its EFI System Partition.
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
