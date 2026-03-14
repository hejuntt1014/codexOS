use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const IDT_ENTRIES: usize = 256;
const INTERRUPT_GATE: u8 = 0x8E;
const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIC_EOI: u8 = 0x20;
const PIT_CMD: u16 = 0x43;
const PIT_CH0: u16 = 0x40;
const PIT_BASE_HZ: u32 = 1_193_182;
const TIMER_VECTOR: u8 = 32;
const TIMER_HZ: u32 = 100;

#[derive(Clone, Copy, Debug)]
pub struct InterruptStatus {
    pub idt_loaded: bool,
    pub hardware_enabled: bool,
    pub ticks: u64,
    pub timer_hz: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: unsafe extern "C" fn(), selector: u16) -> Self {
        let addr = handler as usize as u64;
        Self {
            offset_low: addr as u16,
            selector,
            ist: 0,
            type_attr: INTERRUPT_GATE,
            offset_mid: (addr >> 16) as u16,
            offset_high: (addr >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

#[repr(C)]
struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
}

struct IdtCell(UnsafeCell<[IdtEntry; IDT_ENTRIES]>);

unsafe impl Sync for IdtCell {}

static IDT: IdtCell = IdtCell(UnsafeCell::new([IdtEntry::MISSING; IDT_ENTRIES]));
static IDT_LOADED: AtomicBool = AtomicBool::new(false);
static HARDWARE_INTERRUPTS_ENABLED: AtomicBool = AtomicBool::new(false);
static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_SEEN: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn interrupt_default_stub();
    fn interrupt_divide_stub();
    fn interrupt_breakpoint_stub();
    fn interrupt_double_fault_stub();
    fn interrupt_gpf_stub();
    fn interrupt_page_fault_stub();
    fn interrupt_timer_stub();
}

pub fn init_idt() -> bool {
    if IDT_LOADED.swap(true, Ordering::SeqCst) {
        return false;
    }

    unsafe {
        let idt = &mut *IDT.0.get();
        let selector = read_cs();
        for entry in idt.iter_mut() {
            *entry = IdtEntry::new(interrupt_default_stub, selector);
        }
        idt[0] = IdtEntry::new(interrupt_divide_stub, selector);
        idt[3] = IdtEntry::new(interrupt_breakpoint_stub, selector);
        idt[8] = IdtEntry::new(interrupt_double_fault_stub, selector);
        idt[13] = IdtEntry::new(interrupt_gpf_stub, selector);
        idt[14] = IdtEntry::new(interrupt_page_fault_stub, selector);
        idt[TIMER_VECTOR as usize] = IdtEntry::new(interrupt_timer_stub, selector);

        let idtr = Idtr {
            limit: (core::mem::size_of::<[IdtEntry; IDT_ENTRIES]>() - 1) as u16,
            base: idt.as_ptr() as u64,
        };
        lidt(&idtr);
    }

    true
}

pub fn activate_hardware() -> bool {
    if HARDWARE_INTERRUPTS_ENABLED.swap(true, Ordering::SeqCst) {
        return false;
    }

    unsafe {
        remap_pic();
        mask_pic(0xFE, 0xFF);
        program_pit(TIMER_HZ);
        enable_interrupts();
    }

    true
}

pub fn status() -> InterruptStatus {
    InterruptStatus {
        idt_loaded: IDT_LOADED.load(Ordering::Relaxed),
        hardware_enabled: HARDWARE_INTERRUPTS_ENABLED.load(Ordering::Relaxed),
        ticks: TIMER_TICKS.load(Ordering::Relaxed),
        timer_hz: TIMER_HZ,
    }
}

#[unsafe(no_mangle)]
extern "C" fn interrupt_dispatch(frame: &TrapFrame) {
    match frame.vector as u8 {
        TIMER_VECTOR => {
            let ticks = TIMER_TICKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            if !TIMER_SEEN.swap(true, Ordering::SeqCst) {
                crate::serial::print(format_args!(
                    "interrupts: timer tick online hz={} vector={}\r\n",
                    TIMER_HZ, TIMER_VECTOR
                ));
            }
            if ticks % TIMER_HZ as u64 == 0 {
                core::sync::atomic::compiler_fence(Ordering::SeqCst);
            }
            unsafe {
                send_eoi(TIMER_VECTOR);
            }
        }
        0 => fatal_exception("divide fault", frame, None),
        3 => crate::serial::print(format_args!(
            "interrupts: breakpoint rip=0x{:016x}\r\n",
            frame.rip
        )),
        8 => fatal_exception("double fault", frame, Some(frame.error_code)),
        13 => fatal_exception("general protection fault", frame, Some(frame.error_code)),
        14 => fatal_page_fault(frame),
        _ => fatal_exception("unexpected interrupt", frame, Some(frame.error_code)),
    }
}

fn fatal_page_fault(frame: &TrapFrame) -> ! {
    let fault_addr = read_cr2();
    crate::serial::print(format_args!(
        "interrupts: page fault rip=0x{:016x} addr=0x{:016x} err=0x{:016x}\r\n",
        frame.rip, fault_addr, frame.error_code
    ));
    halt_forever()
}

fn fatal_exception(label: &str, frame: &TrapFrame, error_code: Option<u64>) -> ! {
    match error_code {
        Some(error_code) => crate::serial::print(format_args!(
            "interrupts: {} vector={} rip=0x{:016x} err=0x{:016x}\r\n",
            label, frame.vector, frame.rip, error_code
        )),
        None => crate::serial::print(format_args!(
            "interrupts: {} vector={} rip=0x{:016x}\r\n",
            label, frame.vector, frame.rip
        )),
    }
    halt_forever()
}

fn halt_forever() -> ! {
    unsafe {
        disable_interrupts();
    }
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

unsafe fn remap_pic() {
    let master_mask = inb(PIC1_DATA);
    let slave_mask = inb(PIC2_DATA);

    outb(PIC1_CMD, 0x11);
    io_wait();
    outb(PIC2_CMD, 0x11);
    io_wait();

    outb(PIC1_DATA, TIMER_VECTOR);
    io_wait();
    outb(PIC2_DATA, 40);
    io_wait();

    outb(PIC1_DATA, 0x04);
    io_wait();
    outb(PIC2_DATA, 0x02);
    io_wait();

    outb(PIC1_DATA, 0x01);
    io_wait();
    outb(PIC2_DATA, 0x01);
    io_wait();

    outb(PIC1_DATA, master_mask);
    outb(PIC2_DATA, slave_mask);
}

unsafe fn mask_pic(master_mask: u8, slave_mask: u8) {
    outb(PIC1_DATA, master_mask);
    outb(PIC2_DATA, slave_mask);
}

unsafe fn send_eoi(vector: u8) {
    if vector >= 40 {
        outb(PIC2_CMD, PIC_EOI);
    }
    outb(PIC1_CMD, PIC_EOI);
}

unsafe fn program_pit(hz: u32) {
    let divisor = (PIT_BASE_HZ / hz.max(1)) as u16;
    outb(PIT_CMD, 0x36);
    outb(PIT_CH0, (divisor & 0x00FF) as u8);
    outb(PIT_CH0, (divisor >> 8) as u8);
}

unsafe fn lidt(idtr: &Idtr) {
    core::arch::asm!("lidt [{}]", in(reg) idtr, options(readonly, nostack, preserves_flags));
}

unsafe fn enable_interrupts() {
    core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
}

unsafe fn disable_interrupts() {
    core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
}

fn read_cr2() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_cs() -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("mov {:x}, cs", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!(
        "in al, dx",
        in("dx") port,
        out("al") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

unsafe fn io_wait() {
    outb(0x80, 0);
}

core::arch::global_asm!(
    r#"
    .macro PUSH_GPRS
        push rax
        push rbx
        push rcx
        push rdx
        push rsi
        push rdi
        push rbp
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15
    .endm

    .macro POP_GPRS
        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rbp
        pop rdi
        pop rsi
        pop rdx
        pop rcx
        pop rbx
        pop rax
    .endm

    .macro ISR_NOERR name, vector
        .global \name
    \name:
        push 0
        push \vector
        PUSH_GPRS
        mov rdi, rsp
        call interrupt_dispatch
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    .macro ISR_ERR name, vector
        .global \name
    \name:
        push \vector
        PUSH_GPRS
        mov rdi, rsp
        call interrupt_dispatch
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    ISR_NOERR interrupt_default_stub, 255
    ISR_NOERR interrupt_divide_stub, 0
    ISR_NOERR interrupt_breakpoint_stub, 3
    ISR_ERR interrupt_double_fault_stub, 8
    ISR_ERR interrupt_gpf_stub, 13
    ISR_ERR interrupt_page_fault_stub, 14
    ISR_NOERR interrupt_timer_stub, 32
    "#
);
