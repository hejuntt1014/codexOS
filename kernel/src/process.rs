#[cfg(target_os = "none")]
use core::cell::UnsafeCell;

use crate::vm;
#[cfg(target_os = "none")]
use crate::{interrupts, serial};

#[cfg(target_os = "none")]
const PROCESS_ID: u64 = 1;
pub const SYSCALL_ABI_VERSION: u32 = 2;
pub mod syscall {
    pub const LOG: u64 = 1;
    pub const EXIT: u64 = 2;
    pub const YIELD: u64 = 3;
    pub const GETPID: u64 = 4;
    pub const GETTICKS: u64 = 5;
    pub const SLEEP: u64 = 6;
}
#[cfg(any(target_os = "none", test))]
const ERR_INVALID_ARGUMENT: u64 = (-22_i64) as u64;
#[cfg(any(target_os = "none", test))]
const ERR_NOT_IMPLEMENTED: u64 = (-38_i64) as u64;
#[cfg(any(target_os = "none", test))]
const KERNEL_WRITE_PROBE_ADDRESS: u64 = 0x20_0000;
#[cfg(target_os = "none")]
const MAX_USER_LOG_BYTES: usize = 256;
#[cfg(target_os = "none")]
const USER_ISOLATION_MESSAGE: &str = "ring3 syscall boundary active";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationReport {
    pub syscall_abi_version: u32,
    pub process_id: u64,
    pub kernel_root: u64,
    pub user_root: u64,
    pub entry: u64,
    pub stack_top: u64,
    pub syscall_count: u64,
    pub denied_address: u64,
    pub page_fault_error: u64,
    pub kernel_mapping_supervisor_only: bool,
    pub code_read_execute: bool,
    pub stack_read_write_no_execute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    VirtualMemory(vm::VmError),
    KernelMappingExposed,
    InvalidCodePermissions,
    InvalidStackPermissions,
    ProgramImageInvalid,
    ProbeReturnedWithoutFault,
    UnexpectedExit(u64),
    UnexpectedFault {
        vector: u8,
        address: u64,
        error_code: u64,
    },
    KernelRootRestoreFailed,
}

impl From<vm::VmError> for ProcessError {
    fn from(error: vm::VmError) -> Self {
        Self::VirtualMemory(error)
    }
}

#[cfg(target_os = "none")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessOutcome {
    NotStarted,
    Running,
    IsolationEnforced {
        address: u64,
        error_code: u64,
    },
    Exited(u64),
    Fault {
        vector: u8,
        address: u64,
        error_code: u64,
    },
    RootRestoreFailed,
}

#[cfg(target_os = "none")]
struct ProcessState {
    active: bool,
    kernel_root: u64,
    code_start: u64,
    code_end: u64,
    syscall_count: u64,
    outcome: ProcessOutcome,
}

#[cfg(target_os = "none")]
impl ProcessState {
    const fn new() -> Self {
        Self {
            active: false,
            kernel_root: 0,
            code_start: 0,
            code_end: 0,
            syscall_count: 0,
            outcome: ProcessOutcome::NotStarted,
        }
    }
}

#[cfg(target_os = "none")]
struct ProcessStateCell(UnsafeCell<ProcessState>);

#[cfg(target_os = "none")]
unsafe impl Sync for ProcessStateCell {}

#[cfg(target_os = "none")]
static PROCESS_STATE: ProcessStateCell = ProcessStateCell(UnsafeCell::new(ProcessState::new()));

#[cfg(target_os = "none")]
unsafe extern "sysv64" {
    fn process_enter_user(root: u64, entry: u64, stack_top: u64);
    fn process_user_resume();
    static user_isolation_probe_start: u8;
    static user_isolation_probe_end: u8;
}

#[cfg(target_os = "none")]
pub fn run_isolation_probe() -> Result<IsolationReport, ProcessError> {
    let program = user_probe_image()?;
    serial::print(format_args!(
        "process[{}]: preparing isolated address space\r\n",
        PROCESS_ID
    ));
    let space = vm::create_user_address_space(program)?;
    if !space.kernel_mapping_supervisor_only {
        vm::destroy_user_address_space(space)?;
        return Err(ProcessError::KernelMappingExposed);
    }
    if !space.code_user_read_execute {
        vm::destroy_user_address_space(space)?;
        return Err(ProcessError::InvalidCodePermissions);
    }
    if !space.stack_user_read_write {
        vm::destroy_user_address_space(space)?;
        return Err(ProcessError::InvalidStackPermissions);
    }

    interrupts::register_user_trap_handler(handle_user_trap);
    let state = unsafe { &mut *PROCESS_STATE.0.get() };
    state.active = true;
    state.kernel_root = space.kernel_root_phys;
    state.code_start = space.entry;
    state.code_end = space
        .entry
        .checked_add(space.code_bytes as u64)
        .ok_or(ProcessError::ProgramImageInvalid)?;
    state.syscall_count = 0;
    state.outcome = ProcessOutcome::Running;

    serial::print(format_args!(
        "process[{}]: enter ring3 root=0x{:016x} entry=0x{:016x} stack=0x{:016x} recovery=0x{:016x}\r\n",
        PROCESS_ID,
        space.root_phys,
        space.entry,
        space.stack_top,
        process_user_resume as usize as u64
    ));
    unsafe {
        process_enter_user(space.root_phys, space.entry, space.stack_top);
    }

    if vm::current_root() != space.kernel_root_phys {
        return Err(ProcessError::KernelRootRestoreFailed);
    }
    let state = unsafe { &*PROCESS_STATE.0.get() };
    let outcome = state.outcome;
    let syscall_count = state.syscall_count;
    let result = match outcome {
        ProcessOutcome::IsolationEnforced {
            address,
            error_code,
        } => Ok(IsolationReport {
            syscall_abi_version: SYSCALL_ABI_VERSION,
            process_id: PROCESS_ID,
            kernel_root: space.kernel_root_phys,
            user_root: space.root_phys,
            entry: space.entry,
            stack_top: space.stack_top,
            syscall_count,
            denied_address: address,
            page_fault_error: error_code,
            kernel_mapping_supervisor_only: space.kernel_mapping_supervisor_only,
            code_read_execute: space.code_user_read_execute,
            stack_read_write_no_execute: space.stack_user_read_write,
        }),
        ProcessOutcome::Exited(status) => Err(ProcessError::UnexpectedExit(status)),
        ProcessOutcome::Fault {
            vector,
            address,
            error_code,
        } => Err(ProcessError::UnexpectedFault {
            vector,
            address,
            error_code,
        }),
        ProcessOutcome::RootRestoreFailed => Err(ProcessError::KernelRootRestoreFailed),
        ProcessOutcome::NotStarted | ProcessOutcome::Running => {
            Err(ProcessError::ProbeReturnedWithoutFault)
        }
    };
    vm::destroy_user_address_space(space)?;
    result
}

#[cfg(target_os = "none")]
fn handle_user_trap(frame: &mut interrupts::TrapFrame, fault_address: u64) -> bool {
    let state = unsafe { &mut *PROCESS_STATE.0.get() };
    if !state.active {
        return false;
    }

    match frame.vector as u8 {
        0x80 => handle_syscall(state, frame),
        14 => {
            let expected_error_bits = frame.error_code & 0x07 == 0x07;
            if fault_address == KERNEL_WRITE_PROBE_ADDRESS && expected_error_bits {
                serial::print(format_args!(
                    "process[{}]: kernel write denied addr=0x{:016x} err=0x{:x}\r\n",
                    PROCESS_ID, fault_address, frame.error_code
                ));
                state.outcome = ProcessOutcome::IsolationEnforced {
                    address: fault_address,
                    error_code: frame.error_code,
                };
            } else {
                state.outcome = ProcessOutcome::Fault {
                    vector: 14,
                    address: fault_address,
                    error_code: frame.error_code,
                };
            }
            terminate_user_process(state, frame);
            true
        }
        vector @ 0..=31 => {
            state.outcome = ProcessOutcome::Fault {
                vector,
                address: fault_address,
                error_code: frame.error_code,
            };
            terminate_user_process(state, frame);
            true
        }
        _ => false,
    }
}

#[cfg(target_os = "none")]
fn handle_syscall(state: &mut ProcessState, frame: &mut interrupts::TrapFrame) -> bool {
    state.syscall_count = state.syscall_count.saturating_add(1);
    match frame.rax {
        syscall::LOG => {
            let address = frame.rdi;
            let Ok(length) = usize::try_from(frame.rsi) else {
                frame.rax = ERR_INVALID_ARGUMENT;
                return true;
            };
            if !user_buffer_within(
                state.code_start,
                state.code_end,
                address,
                length,
                MAX_USER_LOG_BYTES,
            ) {
                frame.rax = ERR_INVALID_ARGUMENT;
                return true;
            }
            let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, length) };
            match core::str::from_utf8(bytes) {
                Ok(message) => {
                    serial::print(format_args!(
                        "process[{}]: user log: {}\r\n",
                        PROCESS_ID, message
                    ));
                    frame.rax = length as u64;
                }
                Err(_) => frame.rax = ERR_INVALID_ARGUMENT,
            }
            true
        }
        syscall::EXIT => {
            state.outcome = ProcessOutcome::Exited(frame.rdi);
            terminate_user_process(state, frame);
            true
        }
        _ => {
            frame.rax = ERR_NOT_IMPLEMENTED;
            true
        }
    }
}

#[cfg(any(target_os = "none", test))]
fn user_buffer_within(
    region_start: u64,
    region_end: u64,
    address: u64,
    length: usize,
    maximum_length: usize,
) -> bool {
    length <= maximum_length
        && address >= region_start
        && address
            .checked_add(length as u64)
            .is_some_and(|end| end <= region_end)
}

#[cfg(target_os = "none")]
fn terminate_user_process(state: &mut ProcessState, frame: &mut interrupts::TrapFrame) {
    if vm::switch_root(state.kernel_root).is_err() {
        state.outcome = ProcessOutcome::RootRestoreFailed;
    }
    state.active = false;
    #[cfg(target_os = "none")]
    {
        frame.rip = process_user_resume as usize as u64;
        frame.cs = u64::from(interrupts::KERNEL_CODE_SELECTOR);
        frame.rflags = (frame.rflags | 0x02) & !0x400;
    }
}

#[cfg(target_os = "none")]
fn user_probe_image() -> Result<&'static [u8], ProcessError> {
    let start = core::ptr::addr_of!(user_isolation_probe_start) as usize;
    let end = core::ptr::addr_of!(user_isolation_probe_end) as usize;
    let length = end
        .checked_sub(start)
        .filter(|length| *length != 0 && *length <= bootinfo::PAGE_SIZE as usize)
        .ok_or(ProcessError::ProgramImageInvalid)?;
    Ok(unsafe { core::slice::from_raw_parts(start as *const u8, length) })
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
extern "sysv64" fn process_set_kernel_stack(stack_top: u64) {
    if !interrupts::set_privilege_stack(stack_top) {
        serial::print(format_args!(
            "process[{}]: privilege stack installation failed\r\n",
            PROCESS_ID
        ));
        interrupts::halt();
    }
}

#[cfg(target_os = "none")]
core::arch::global_asm!(
    r#"
    .section .text.process,"ax"
    .global process_enter_user
    .global process_user_resume
    process_enter_user:
        push rbp
        push rbx
        push r12
        push r13
        push r14
        push r15
        sub rsp, 8
        mov r13, rdi
        mov r14, rsi
        mov r15, rdx
        mov r12, rsp
        mov rdi, r12
        call process_set_kernel_stack
        mov rsp, r12
        mov cr3, r13
        push {user_data}
        push r15
        push 0x202
        push {user_code}
        push r14
        iretq

    process_user_resume:
        add rsp, 24
        pop r15
        pop r14
        pop r13
        pop r12
        pop rbx
        pop rbp
        ret

    .section .rodata.user_probe,"a"
    .align 16
    .global user_isolation_probe_start
    .global user_isolation_probe_end
    user_isolation_probe_start:
        mov eax, 1
        lea rdi, [rip + user_isolation_message]
        mov esi, {message_length}
        int 0x80
        movabs rax, 0x200000
        mov byte ptr [rax], 0x41
        mov eax, 2
        mov edi, 0xbad
        int 0x80
        ud2
    user_isolation_message:
        .ascii "ring3 syscall boundary active"
    user_isolation_message_end:
    user_isolation_probe_end:
    "#,
    user_data = const interrupts::USER_DATA_SELECTOR,
    user_code = const interrupts::USER_CODE_SELECTOR,
    message_length = const USER_ISOLATION_MESSAGE.len(),
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_error_values_follow_negative_errno_convention() {
        assert_eq!(ERR_INVALID_ARGUMENT as i64, -22);
        assert_eq!(ERR_NOT_IMPLEMENTED as i64, -38);
    }

    #[test]
    fn kernel_probe_targets_the_loaded_text_page() {
        assert_eq!(KERNEL_WRITE_PROBE_ADDRESS, 0x20_0000);
    }

    #[test]
    fn user_buffer_validation_rejects_escape_and_overflow() {
        assert!(user_buffer_within(0x1000, 0x2000, 0x1800, 16, 256));
        assert!(!user_buffer_within(0x1000, 0x2000, 0x0fff, 16, 256));
        assert!(!user_buffer_within(0x1000, 0x2000, 0x1ff8, 16, 256));
        assert!(!user_buffer_within(0x1000, 0x2000, u64::MAX - 3, 8, 256));
        assert!(!user_buffer_within(0x1000, 0x2000, 0x1800, 257, 256));
    }
}
