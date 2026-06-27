use alloc::{boxed::Box, vec, vec::Vec};
use core::cell::UnsafeCell;

use sha2::{Digest, Sha256};

use crate::{block::BlockDevice, fs, interrupts, memory, process, user_elf, vm};

pub const PERSISTENT_EXECUTABLE_PATH: &str = "/system/bin/scheduler-gate.elf";
const KERNEL_STACK_BYTES: usize = 64 * 1024;
const FIRST_PID: u64 = 1001;
const PERSISTENT_PID: u64 = 2001;
const ISOLATION_FAULT_ADDRESS: u64 = 0x20_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerReport {
    pub process_count: usize,
    pub timer_ticks: u64,
    pub timer_preemptions: u64,
    pub context_switches: u64,
    pub minimum_dispatches: u64,
    pub idle_halts: u64,
    pub reclaimed_pages: u64,
    pub faulted_pid: u64,
    pub fault_address: u64,
    pub fault_error: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentExecutableReport {
    pub path: &'static str,
    pub bytes: usize,
    pub sha256: [u8; 32],
    pub entry: u64,
    pub load_segments: usize,
    pub exit_status: u64,
    pub timer_ticks: u64,
    pub idle_halts: u64,
    pub reclaimed_pages: u64,
    pub generation: u64,
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    VirtualMemory(vm::VmError),
    Filesystem(fs::FsError),
    UserExecutable(user_elf::UserElfError),
    ProgramImageInvalid,
    PersistentProgramMissing,
    PersistentProgramHashMismatch,
    PersistentProgramPermissions,
    TimerInactive,
    PrivilegeStackUnavailable,
    RuntimeRootSwitch,
    ProcessDidNotExit(u64),
    FaultIsolationFailed,
    PreemptionNotObserved,
    IdleNotObserved,
    FairnessViolation(u64),
    PhysicalPageLeak { before: u64, after: u64 },
}

impl From<vm::VmError> for SchedulerError {
    fn from(error: vm::VmError) -> Self {
        Self::VirtualMemory(error)
    }
}

impl From<fs::FsError> for SchedulerError {
    fn from(error: fs::FsError) -> Self {
        Self::Filesystem(error)
    }
}

impl From<user_elf::UserElfError> for SchedulerError {
    fn from(error: user_elf::UserElfError) -> Self {
        Self::UserExecutable(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Runnable,
    Sleeping {
        wake_tick: u64,
    },
    Exited(u64),
    Faulted {
        vector: u8,
        address: u64,
        error_code: u64,
    },
}

#[derive(Clone, Copy)]
struct UserContext {
    frame: interrupts::TrapFrame,
    stack_pointer: u64,
    stack_selector: u64,
}

impl UserContext {
    fn initial(entry: u64, stack_pointer: u64) -> Self {
        Self {
            frame: interrupts::TrapFrame {
                r15: 0,
                r14: 0,
                r13: 0,
                r12: 0,
                r11: 0,
                r10: 0,
                r9: 0,
                r8: 0,
                rbp: 0,
                rdi: 0,
                rsi: 0,
                rdx: 0,
                rcx: 0,
                rbx: 0,
                rax: 0,
                vector: 0,
                error_code: 0,
                rip: entry,
                cs: u64::from(interrupts::USER_CODE_SELECTOR),
                rflags: 0x202,
            },
            stack_pointer,
            stack_selector: u64::from(interrupts::USER_DATA_SELECTOR),
        }
    }

    fn capture(frame: &interrupts::TrapFrame) -> Option<Self> {
        let (stack_pointer, stack_selector) = interrupts::user_stack(frame)?;
        Some(Self {
            frame: *frame,
            stack_pointer,
            stack_selector,
        })
    }

    fn restore(self, frame: &mut interrupts::TrapFrame) -> bool {
        *frame = self.frame;
        interrupts::set_user_stack(frame, self.stack_pointer, self.stack_selector)
    }
}

struct Process {
    pid: u64,
    space: Option<vm::UserAddressSpace>,
    kernel_stack: Box<[u8]>,
    context: UserContext,
    state: ProcessState,
    dispatches: u64,
    syscalls: u64,
}

impl Process {
    fn new(pid: u64, image: &[u8]) -> Result<Self, SchedulerError> {
        let space = vm::create_user_address_space(image)?;
        Self::from_space(pid, space)
    }

    fn from_elf(pid: u64, image: &[u8]) -> Result<Self, SchedulerError> {
        let executable = user_elf::parse(image)?;
        let segments: Vec<vm::UserSegment<'_>> = executable
            .segments
            .iter()
            .map(|segment| vm::UserSegment {
                virtual_address: segment.virtual_address,
                data: segment.data,
                memory_size: segment.memory_size,
                writable: segment.writable,
                executable: segment.executable,
            })
            .collect();
        let space = vm::create_user_address_space_from_segments(executable.entry, &segments)?;
        Self::from_space(pid, space)
    }

    fn from_space(pid: u64, space: vm::UserAddressSpace) -> Result<Self, SchedulerError> {
        if !space.kernel_mapping_supervisor_only
            || !space.code_user_read_execute
            || !space.stack_user_read_write
        {
            vm::destroy_user_address_space(space)?;
            return Err(SchedulerError::ProgramImageInvalid);
        }
        Ok(Self {
            pid,
            context: UserContext::initial(space.entry, space.stack_top),
            space: Some(space),
            kernel_stack: vec![0_u8; KERNEL_STACK_BYTES].into_boxed_slice(),
            state: ProcessState::Runnable,
            dispatches: 0,
            syscalls: 0,
        })
    }

    fn root(&self) -> u64 {
        self.space
            .as_ref()
            .expect("live process owns an address space")
            .root_phys
    }

    fn kernel_stack_top(&self) -> u64 {
        (self.kernel_stack.as_ptr() as u64 + self.kernel_stack.len() as u64) & !0x0f
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFailure {
    MissingUserStack,
    RootSwitch,
    PrivilegeStack,
}

struct SchedulerState {
    active: bool,
    kernel_root: u64,
    current: Option<usize>,
    processes: Vec<Process>,
    timer_preemptions: u64,
    context_switches: u64,
    failure: Option<RuntimeFailure>,
    idle_halts: u64,
}

#[derive(Debug, Clone, Copy)]
struct SchedulerRunMetrics {
    runtime_failure: Option<RuntimeFailure>,
    ticks_before: u64,
    ticks_after: u64,
    timer_preemptions: u64,
    context_switches: u64,
    idle_halts: u64,
    minimum_dispatches: u64,
    pages_before: u64,
    pages_allocated: u64,
}

impl SchedulerState {
    const fn new() -> Self {
        Self {
            active: false,
            kernel_root: 0,
            current: None,
            processes: Vec::new(),
            timer_preemptions: 0,
            context_switches: 0,
            failure: None,
            idle_halts: 0,
        }
    }

    fn wake_sleepers(&mut self) {
        let ticks = interrupts::status().ticks;
        for process in &mut self.processes {
            if matches!(process.state, ProcessState::Sleeping { wake_tick } if ticks >= wake_tick) {
                process.state = ProcessState::Runnable;
            }
        }
    }

    fn has_sleepers(&self) -> bool {
        self.processes
            .iter()
            .any(|process| matches!(process.state, ProcessState::Sleeping { .. }))
    }

    fn next_runnable(&self, current: usize) -> Option<usize> {
        (1..=self.processes.len())
            .map(|offset| (current + offset) % self.processes.len())
            .find(|index| self.processes[*index].state == ProcessState::Runnable)
    }

    fn schedule(&mut self, frame: &mut interrupts::TrapFrame, timer: bool) {
        let Some(current) = self.current else {
            return;
        };
        if matches!(
            self.processes[current].state,
            ProcessState::Runnable | ProcessState::Sleeping { .. }
        ) {
            let Some(context) = UserContext::capture(frame) else {
                self.failure = Some(RuntimeFailure::MissingUserStack);
                self.return_to_kernel(frame);
                return;
            };
            self.processes[current].context = context;
        }

        self.wake_sleepers();
        let mut next = self.next_runnable(current);
        if next.is_none() && self.has_sleepers() {
            if vm::switch_root(self.kernel_root).is_err() {
                self.failure = Some(RuntimeFailure::RootSwitch);
                self.return_to_kernel(frame);
                return;
            }
            while next.is_none() && self.has_sleepers() {
                self.idle_halts = self.idle_halts.saturating_add(1);
                unsafe {
                    core::arch::asm!("sti", "hlt", "cli");
                }
                self.wake_sleepers();
                next = self.next_runnable(current);
            }
        }
        let Some(next) = next else {
            self.return_to_kernel(frame);
            return;
        };
        if next != current {
            self.context_switches = self.context_switches.saturating_add(1);
            if timer {
                self.timer_preemptions = self.timer_preemptions.saturating_add(1);
            }
        }
        self.current = Some(next);
        self.processes[next].dispatches = self.processes[next].dispatches.saturating_add(1);
        let root = self.processes[next].root();
        let stack_top = self.processes[next].kernel_stack_top();
        let context = self.processes[next].context;
        if vm::switch_root(root).is_err() {
            self.failure = Some(RuntimeFailure::RootSwitch);
            self.return_to_kernel(frame);
            return;
        }
        if !interrupts::set_privilege_stack(stack_top) {
            self.failure = Some(RuntimeFailure::PrivilegeStack);
            self.return_to_kernel(frame);
            return;
        }
        if !context.restore(frame) {
            self.failure = Some(RuntimeFailure::MissingUserStack);
            self.return_to_kernel(frame);
        }
    }

    fn return_to_kernel(&mut self, frame: &mut interrupts::TrapFrame) {
        if vm::switch_root(self.kernel_root).is_err() {
            self.failure = Some(RuntimeFailure::RootSwitch);
        }
        self.active = false;
        self.current = None;
        frame.rip = scheduler_kernel_resume as usize as u64;
        frame.cs = u64::from(interrupts::KERNEL_CODE_SELECTOR);
        frame.rflags = (frame.rflags | 0x02) & !0x400;
    }
}

struct SchedulerCell(UnsafeCell<SchedulerState>);

unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(SchedulerState::new()));

unsafe extern "sysv64" {
    fn scheduler_enter_user(root: u64, entry: u64, stack_pointer: u64);
    fn scheduler_kernel_resume();
    static scheduler_normal_start: u8;
    static scheduler_normal_end: u8;
    static scheduler_fault_start: u8;
    static scheduler_fault_end: u8;
}

pub fn run_preemption_gate() -> Result<SchedulerReport, SchedulerError> {
    let pages_before = memory::stats().allocated_pages;
    let normal_image = task_image(
        core::ptr::addr_of!(scheduler_normal_start),
        core::ptr::addr_of!(scheduler_normal_end),
    )?;
    let fault_image = task_image(
        core::ptr::addr_of!(scheduler_fault_start),
        core::ptr::addr_of!(scheduler_fault_end),
    )?;
    let mut processes = Vec::with_capacity(3);
    for (pid, image) in [
        (FIRST_PID, normal_image),
        (FIRST_PID + 1, fault_image),
        (FIRST_PID + 2, normal_image),
    ] {
        match Process::new(pid, image) {
            Ok(process) => processes.push(process),
            Err(error) => {
                reclaim_processes(&mut processes)?;
                return Err(error);
            }
        }
    }

    let (mut processes, metrics) = run_loaded_processes(processes, pages_before)?;
    let validation = validate_outcomes(
        &processes,
        metrics.runtime_failure,
        metrics.ticks_before,
        metrics.ticks_after,
        metrics.timer_preemptions,
        metrics.idle_halts,
    );
    let fault = processes[1].state;
    reclaim_processes(&mut processes)?;
    let pages_after = memory::stats().allocated_pages;
    if pages_after != metrics.pages_before {
        return Err(SchedulerError::PhysicalPageLeak {
            before: metrics.pages_before,
            after: pages_after,
        });
    }
    validation?;

    let ProcessState::Faulted {
        address,
        error_code,
        ..
    } = fault
    else {
        return Err(SchedulerError::FaultIsolationFailed);
    };
    Ok(SchedulerReport {
        process_count: 3,
        timer_ticks: metrics.ticks_after.saturating_sub(metrics.ticks_before),
        timer_preemptions: metrics.timer_preemptions,
        context_switches: metrics.context_switches,
        minimum_dispatches: metrics.minimum_dispatches,
        idle_halts: metrics.idle_halts,
        reclaimed_pages: metrics.pages_allocated.saturating_sub(pages_after),
        faulted_pid: FIRST_PID + 1,
        fault_address: address,
        fault_error: error_code,
    })
}

pub fn run_persistent_executable_gate<D: BlockDevice>(
    filesystem: &mut fs::CodexFs<D>,
) -> Result<PersistentExecutableReport, SchedulerError> {
    let executable = persistent_user_executable()?;
    let expected_hash = sha256(&executable);
    let mut installed = match filesystem.read_file(PERSISTENT_EXECUTABLE_PATH) {
        None => {
            filesystem.write_file_with_permissions(
                PERSISTENT_EXECUTABLE_PATH,
                &executable,
                0o555,
            )?;
            true
        }
        Some(bytes) if sha256(bytes) == expected_hash => false,
        Some(_) => return Err(SchedulerError::PersistentProgramHashMismatch),
    };
    filesystem.verify_committed_state()?;
    let mut metadata = filesystem.metadata(PERSISTENT_EXECUTABLE_PATH)?;
    if metadata.kind == fs::EntryKind::File
        && metadata.permissions & 0o500 != 0o500
        && filesystem
            .read_file(PERSISTENT_EXECUTABLE_PATH)
            .is_some_and(|bytes| sha256(bytes) == expected_hash)
    {
        filesystem.set_permissions(PERSISTENT_EXECUTABLE_PATH, 0o555)?;
        filesystem.verify_committed_state()?;
        metadata = filesystem.metadata(PERSISTENT_EXECUTABLE_PATH)?;
        installed = true;
    }
    if metadata.kind != fs::EntryKind::File || metadata.permissions & 0o500 != 0o500 {
        return Err(SchedulerError::PersistentProgramPermissions);
    }
    let generation = filesystem.info().generation;

    let bytes = filesystem
        .read_file(PERSISTENT_EXECUTABLE_PATH)
        .ok_or(SchedulerError::PersistentProgramMissing)?;
    if sha256(bytes) != expected_hash {
        return Err(SchedulerError::PersistentProgramHashMismatch);
    }
    let parsed = user_elf::parse(bytes)?;
    let entry = parsed.entry;
    let load_segments = parsed.segments.len();
    let pages_before = memory::stats().allocated_pages;
    let process = Process::from_elf(PERSISTENT_PID, bytes)?;
    let (mut processes, metrics) = run_loaded_processes(vec![process], pages_before)?;
    let validation = validate_persistent_outcome(&processes, metrics);
    reclaim_processes(&mut processes)?;
    let pages_after = memory::stats().allocated_pages;
    if pages_after != metrics.pages_before {
        return Err(SchedulerError::PhysicalPageLeak {
            before: metrics.pages_before,
            after: pages_after,
        });
    }
    let exit_status = validation?;
    Ok(PersistentExecutableReport {
        path: PERSISTENT_EXECUTABLE_PATH,
        bytes: bytes.len(),
        sha256: expected_hash,
        entry,
        load_segments,
        exit_status,
        timer_ticks: metrics.ticks_after.saturating_sub(metrics.ticks_before),
        idle_halts: metrics.idle_halts,
        reclaimed_pages: metrics.pages_allocated.saturating_sub(pages_after),
        generation,
        installed,
    })
}

fn persistent_user_executable() -> Result<Vec<u8>, SchedulerError> {
    let normal_image = task_image(
        core::ptr::addr_of!(scheduler_normal_start),
        core::ptr::addr_of!(scheduler_normal_end),
    )?;
    Ok(user_elf::build_flat_rx_executable(
        vm::USER_CODE_BASE,
        normal_image,
    )?)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn run_loaded_processes(
    mut processes: Vec<Process>,
    pages_before: u64,
) -> Result<(Vec<Process>, SchedulerRunMetrics), SchedulerError> {
    if !interrupts::status().hardware_enabled {
        reclaim_processes(&mut processes)?;
        return Err(SchedulerError::TimerInactive);
    }
    if processes.is_empty() {
        return Err(SchedulerError::ProgramImageInvalid);
    }

    let ticks_before = interrupts::status().ticks;
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    scheduler.active = true;
    scheduler.kernel_root = vm::current_root();
    scheduler.current = Some(0);
    scheduler.processes = processes;
    scheduler.timer_preemptions = 0;
    scheduler.context_switches = 0;
    scheduler.failure = None;
    scheduler.idle_halts = 0;
    scheduler.processes[0].dispatches = 1;

    interrupts::register_user_trap_handler(handle_user_trap);
    let first_root = scheduler.processes[0].root();
    let first_entry = scheduler.processes[0].context.frame.rip;
    let first_user_stack = scheduler.processes[0].context.stack_pointer;
    if !interrupts::set_privilege_stack(scheduler.processes[0].kernel_stack_top()) {
        reclaim_processes(&mut scheduler.processes)?;
        scheduler.active = false;
        scheduler.current = None;
        return Err(SchedulerError::PrivilegeStackUnavailable);
    }
    unsafe {
        scheduler_enter_user(first_root, first_entry, first_user_stack);
    }

    let ticks_after = interrupts::status().ticks;
    let metrics = SchedulerRunMetrics {
        runtime_failure: scheduler.failure,
        ticks_before,
        ticks_after,
        timer_preemptions: scheduler.timer_preemptions,
        context_switches: scheduler.context_switches,
        idle_halts: scheduler.idle_halts,
        minimum_dispatches: scheduler
            .processes
            .iter()
            .map(|process| process.dispatches)
            .min()
            .unwrap_or(0),
        pages_before,
        pages_allocated: memory::stats().allocated_pages,
    };
    let processes = core::mem::take(&mut scheduler.processes);
    scheduler.active = false;
    scheduler.current = None;
    Ok((processes, metrics))
}

fn validate_persistent_outcome(
    processes: &[Process],
    metrics: SchedulerRunMetrics,
) -> Result<u64, SchedulerError> {
    if metrics.runtime_failure.is_some() {
        return Err(SchedulerError::RuntimeRootSwitch);
    }
    let Some(process) = processes.first() else {
        return Err(SchedulerError::ProcessDidNotExit(PERSISTENT_PID));
    };
    let ProcessState::Exited(status) = process.state else {
        return Err(SchedulerError::ProcessDidNotExit(process.pid));
    };
    if process.pid != PERSISTENT_PID || status != PERSISTENT_PID {
        return Err(SchedulerError::ProcessDidNotExit(process.pid));
    }
    if metrics.ticks_after <= metrics.ticks_before {
        return Err(SchedulerError::PreemptionNotObserved);
    }
    if metrics.idle_halts == 0 {
        return Err(SchedulerError::IdleNotObserved);
    }
    if process.dispatches < 2 {
        return Err(SchedulerError::FairnessViolation(process.pid));
    }
    Ok(status)
}

fn validate_outcomes(
    processes: &[Process],
    runtime_failure: Option<RuntimeFailure>,
    ticks_before: u64,
    ticks_after: u64,
    timer_preemptions: u64,
    idle_halts: u64,
) -> Result<(), SchedulerError> {
    if runtime_failure.is_some() {
        return Err(SchedulerError::RuntimeRootSwitch);
    }
    for process in [processes.first(), processes.get(2)].into_iter().flatten() {
        if process.state != ProcessState::Exited(process.pid) {
            return Err(SchedulerError::ProcessDidNotExit(process.pid));
        }
    }
    let Some(faulted) = processes.get(1) else {
        return Err(SchedulerError::FaultIsolationFailed);
    };
    if !matches!(
        faulted.state,
        ProcessState::Faulted {
            vector: 14,
            address: ISOLATION_FAULT_ADDRESS,
            error_code
        } if error_code & 0x07 == 0x07
    ) {
        return Err(SchedulerError::FaultIsolationFailed);
    }
    if ticks_after <= ticks_before || timer_preemptions < 2 {
        return Err(SchedulerError::PreemptionNotObserved);
    }
    if idle_halts == 0 {
        return Err(SchedulerError::IdleNotObserved);
    }
    if let Some(process) = processes.iter().find(|process| process.dispatches < 2) {
        return Err(SchedulerError::FairnessViolation(process.pid));
    }
    Ok(())
}

fn reclaim_processes(processes: &mut Vec<Process>) -> Result<(), SchedulerError> {
    for process in processes.iter_mut() {
        if let Some(space) = process.space.take() {
            vm::destroy_user_address_space(space)?;
        }
    }
    processes.clear();
    Ok(())
}

fn handle_user_trap(frame: &mut interrupts::TrapFrame, fault_address: u64) -> bool {
    let scheduler = unsafe { &mut *SCHEDULER.0.get() };
    if !scheduler.active {
        return false;
    }
    let Some(current) = scheduler.current else {
        return false;
    };
    match frame.vector as u8 {
        interrupts::TIMER_VECTOR => scheduler.schedule(frame, true),
        interrupts::USER_SYSCALL_VECTOR => {
            scheduler.processes[current].syscalls =
                scheduler.processes[current].syscalls.saturating_add(1);
            match frame.rax {
                process::syscall::EXIT => {
                    scheduler.processes[current].state = ProcessState::Exited(frame.rdi);
                    scheduler.schedule(frame, false);
                }
                process::syscall::YIELD => {
                    frame.rax = 0;
                    scheduler.schedule(frame, false);
                }
                process::syscall::GETPID => frame.rax = scheduler.processes[current].pid,
                process::syscall::GETTICKS => frame.rax = interrupts::status().ticks,
                process::syscall::SLEEP => {
                    let wake_tick = interrupts::status().ticks.saturating_add(frame.rdi);
                    frame.rax = 0;
                    scheduler.processes[current].state = ProcessState::Sleeping { wake_tick };
                    scheduler.schedule(frame, false);
                }
                _ => frame.rax = (-38_i64) as u64,
            }
        }
        vector @ 0..=31 => {
            scheduler.processes[current].state = ProcessState::Faulted {
                vector,
                address: fault_address,
                error_code: frame.error_code,
            };
            scheduler.schedule(frame, false);
        }
        _ => return false,
    }
    true
}

fn task_image(start: *const u8, end: *const u8) -> Result<&'static [u8], SchedulerError> {
    let start = start as usize;
    let end = end as usize;
    let length = end
        .checked_sub(start)
        .filter(|length| *length != 0 && *length <= bootinfo::PAGE_SIZE as usize)
        .ok_or(SchedulerError::ProgramImageInvalid)?;
    Ok(unsafe { core::slice::from_raw_parts(start as *const u8, length) })
}

core::arch::global_asm!(
    r#"
    .section .bss.scheduler,"aw",@nobits
    .align 8
    scheduler_resume_stack:
        .quad 0

    .section .text.scheduler,"ax"
    .global scheduler_enter_user
    .global scheduler_kernel_resume
    scheduler_enter_user:
        push rbp
        push rbx
        push r12
        push r13
        push r14
        push r15
        sub rsp, 8
        mov qword ptr [rip + scheduler_resume_stack], rsp
        mov r13, rdi
        mov r14, rsi
        mov r15, rdx
        mov cr3, r13
        push {user_data}
        push r15
        push 0x202
        push {user_code}
        push r14
        iretq

    scheduler_kernel_resume:
        mov rsp, qword ptr [rip + scheduler_resume_stack]
        add rsp, 8
        pop r15
        pop r14
        pop r13
        pop r12
        pop rbx
        pop rbp
        ret

    .section .rodata.scheduler_tasks,"a"
    .align 16
    .global scheduler_normal_start
    .global scheduler_normal_end
    scheduler_normal_start:
        mov eax, {getpid}
        int 0x80
        mov r13, rax
        mov eax, {sleep_call}
        mov edi, 2
        int 0x80
        mov eax, {getticks}
        int 0x80
        mov r12, rax
    1:
        pause
        mov eax, {getticks}
        int 0x80
        sub rax, r12
        cmp rax, 4
        jb 1b
        mov eax, {yield_call}
        int 0x80
        mov eax, {getticks}
        int 0x80
        mov r12, rax
    2:
        pause
        mov eax, {getticks}
        int 0x80
        sub rax, r12
        cmp rax, 3
        jb 2b
        mov eax, {exit_call}
        mov rdi, r13
        int 0x80
        ud2
    scheduler_normal_end:

    .align 16
    .global scheduler_fault_start
    .global scheduler_fault_end
    scheduler_fault_start:
        mov eax, {getpid}
        int 0x80
        mov eax, {sleep_call}
        mov edi, 2
        int 0x80
        mov eax, {getticks}
        int 0x80
        mov r12, rax
    3:
        pause
        mov eax, {getticks}
        int 0x80
        sub rax, r12
        cmp rax, 6
        jb 3b
        movabs rax, 0x200000
        mov byte ptr [rax], 0x5a
        ud2
    scheduler_fault_end:
    "#,
    user_data = const interrupts::USER_DATA_SELECTOR,
    user_code = const interrupts::USER_CODE_SELECTOR,
    exit_call = const process::syscall::EXIT,
    yield_call = const process::syscall::YIELD,
    getpid = const process::syscall::GETPID,
    getticks = const process::syscall::GETTICKS,
    sleep_call = const process::syscall::SLEEP,
);
