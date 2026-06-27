# Processes and scheduling

The standalone kernel runs Ring 3 tasks in independent four-level page-table roots. Each process owns its root table, user code/data pages, RW/NX stack page, intermediate user page tables, and a dedicated 64 KiB Ring 0 interrupt stack. Kernel mappings are copied into every root without the user bit, executable user pages are RX, writable user pages are NX, and the user stack is RW/NX.

The PIT runs at 100 Hz. A timer interrupt arriving from Ring 3 saves all general-purpose registers plus RIP, RFLAGS, user RSP, and user SS. The scheduler chooses the next runnable process in round-robin order, changes CR3, installs that process's Ring 0 stack in the TSS, restores its saved context, acknowledges the PIC, and returns with `iretq`.

Syscall ABI version 2 uses `int 0x80`:

| Number | Operation | Arguments | Result |
| ---: | --- | --- | --- |
| 1 | log | `rdi` buffer, `rsi` length | bytes accepted or negative errno |
| 2 | exit | `rdi` status | does not return |
| 3 | yield | none | zero |
| 4 | get process ID | none | process ID |
| 5 | get timer ticks | none | monotonic PIT tick count |
| 6 | sleep | `rdi` ticks | zero after wakeup |

When every live process is sleeping, the scheduler switches to the kernel root and executes an interrupt-enabled `hlt`. A PIT interrupt advances time; sleepers whose deadline has arrived become runnable and execution resumes in Ring 3.

Process exit and user exceptions remove the process from the runnable set. Page faults capture CR2 and the hardware error code. Once the workload finishes, the kernel clears and returns every user-owned physical page to a coalescing free-extent allocator before dropping the process kernel stacks.

User programs can be loaded from ELF64 `ET_EXEC` images. The loader accepts x86-64 little-endian files with checked `PT_LOAD` segments, rejects writable-executable segments, validates segment bounds, requires the entry point to land inside executable file-backed bytes, maps each segment with page permissions derived from ELF flags, and zero-fills segment memory beyond the file payload. The chainload path provisions `/system/bin/scheduler-gate.elf` into CodexFS when the file is absent; later boots read the file back, verify its SHA-256 digest against the kernel-expected image, parse the ELF, and execute it as PID 2001.

The chainload smoke gate runs three real user programs. Two sleep, consume timer time, yield, and exit with their process IDs. One sleeps and then attempts a write to kernel text, producing a user/write/present page fault. The gate requires at least two PIT-driven cross-process preemptions, at least two dispatches per process, at least one hardware idle halt, correct exit and fault outcomes, and exact physical-page accounting after reclamation.

The same smoke run then executes the CodexFS-backed ELF program through the shared scheduler path. That gate requires a matching SHA-256 digest, a valid ELF segment table, exit status 2001 from Ring 3, at least one hardware idle halt during timed sleep, and exact address-space reclamation.
