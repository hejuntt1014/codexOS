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
pub const TIMER_VECTOR: u8 = 32;
const TIMER_HZ: u32 = 100;
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
const KERNEL_DATA_SELECTOR: u16 = 0x10;
const TSS_SELECTOR: u16 = 0x18;
pub const USER_DATA_SELECTOR: u16 = 0x2b;
pub const USER_CODE_SELECTOR: u16 = 0x33;
pub const USER_SYSCALL_VECTOR: u8 = 0x80;
const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct InterruptStatus {
    pub gdt_loaded: bool,
    pub task_register: u16,
    pub idt_loaded: bool,
    pub hardware_enabled: bool,
    pub ticks: u64,
    pub timer_hz: u32,
    pub breakpoint_hits: u64,
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
        Self::from_addr(handler as usize as u64, selector)
    }

    fn from_addr(addr: u64, selector: u16) -> Self {
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

    fn with_ist(mut self, ist: u8) -> Self {
        self.ist = ist & 0x07;
        self
    }

    fn with_dpl(mut self, dpl: u8) -> Self {
        self.type_attr = (self.type_attr & !0x60) | ((dpl & 0x03) << 5);
        self
    }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
struct TaskStateSegment {
    reserved0: u32,
    privilege_stacks: [u64; 3],
    reserved1: u64,
    interrupt_stacks: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    io_map_base: u16,
}

impl TaskStateSegment {
    const EMPTY: Self = Self {
        reserved0: 0,
        privilege_stacks: [0; 3],
        reserved1: 0,
        interrupt_stacks: [0; 7],
        reserved2: 0,
        reserved3: 0,
        io_map_base: 0,
    };
}

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

#[repr(align(16))]
struct EmergencyStack {
    _storage: [u8; DOUBLE_FAULT_STACK_BYTES],
}

struct GdtCell(UnsafeCell<[u64; 7]>);
struct TssCell(UnsafeCell<TaskStateSegment>);
struct StackCell(UnsafeCell<EmergencyStack>);

unsafe impl Sync for GdtCell {}
unsafe impl Sync for TssCell {}
unsafe impl Sync for StackCell {}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
}

pub type UserTrapHandler = fn(&mut TrapFrame, u64) -> bool;

struct IdtCell(UnsafeCell<[IdtEntry; IDT_ENTRIES]>);

unsafe impl Sync for IdtCell {}

struct UserTrapHandlerCell(UnsafeCell<Option<UserTrapHandler>>);

unsafe impl Sync for UserTrapHandlerCell {}

static IDT: IdtCell = IdtCell(UnsafeCell::new([IdtEntry::MISSING; IDT_ENTRIES]));
static GDT: GdtCell = GdtCell(UnsafeCell::new([0; 7]));
static TSS: TssCell = TssCell(UnsafeCell::new(TaskStateSegment::EMPTY));
static DOUBLE_FAULT_STACK: StackCell = StackCell(UnsafeCell::new(EmergencyStack {
    _storage: [0; DOUBLE_FAULT_STACK_BYTES],
}));
static GDT_LOADED: AtomicBool = AtomicBool::new(false);
static IDT_LOADED: AtomicBool = AtomicBool::new(false);
static HARDWARE_INTERRUPTS_ENABLED: AtomicBool = AtomicBool::new(false);
static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_SEEN: AtomicBool = AtomicBool::new(false);
static BREAKPOINT_HITS: AtomicU64 = AtomicU64::new(0);
static USER_TRAP_HANDLER: UserTrapHandlerCell = UserTrapHandlerCell(UnsafeCell::new(None));

unsafe extern "C" {
    fn interrupt_default_stub();
    fn interrupt_timer_stub();
    fn interrupt_user_syscall_stub();
    fn interrupt_exception_0();
    fn interrupt_exception_1();
    fn interrupt_exception_2();
    fn interrupt_exception_3();
    fn interrupt_exception_4();
    fn interrupt_exception_5();
    fn interrupt_exception_6();
    fn interrupt_exception_7();
    fn interrupt_exception_8();
    fn interrupt_exception_9();
    fn interrupt_exception_10();
    fn interrupt_exception_11();
    fn interrupt_exception_12();
    fn interrupt_exception_13();
    fn interrupt_exception_14();
    fn interrupt_exception_15();
    fn interrupt_exception_16();
    fn interrupt_exception_17();
    fn interrupt_exception_18();
    fn interrupt_exception_19();
    fn interrupt_exception_20();
    fn interrupt_exception_21();
    fn interrupt_exception_22();
    fn interrupt_exception_23();
    fn interrupt_exception_24();
    fn interrupt_exception_25();
    fn interrupt_exception_26();
    fn interrupt_exception_27();
    fn interrupt_exception_28();
    fn interrupt_exception_29();
    fn interrupt_exception_30();
    fn interrupt_exception_31();
}

pub fn init_idt() -> bool {
    if IDT_LOADED.swap(true, Ordering::SeqCst) {
        return false;
    }

    unsafe {
        init_gdt_tss();
        let idt = &mut *IDT.0.get();
        let selector = read_cs();
        for entry in idt.iter_mut() {
            *entry = IdtEntry::new(interrupt_default_stub, selector);
        }
        idt[0] = IdtEntry::new(interrupt_exception_0, selector);
        idt[1] = IdtEntry::new(interrupt_exception_1, selector);
        idt[2] = IdtEntry::new(interrupt_exception_2, selector);
        idt[3] = IdtEntry::new(interrupt_exception_3, selector);
        idt[4] = IdtEntry::new(interrupt_exception_4, selector);
        idt[5] = IdtEntry::new(interrupt_exception_5, selector);
        idt[6] = IdtEntry::new(interrupt_exception_6, selector);
        idt[7] = IdtEntry::new(interrupt_exception_7, selector);
        idt[8] = IdtEntry::new(interrupt_exception_8, selector).with_ist(DOUBLE_FAULT_IST);
        idt[9] = IdtEntry::new(interrupt_exception_9, selector);
        idt[10] = IdtEntry::new(interrupt_exception_10, selector);
        idt[11] = IdtEntry::new(interrupt_exception_11, selector);
        idt[12] = IdtEntry::new(interrupt_exception_12, selector);
        idt[13] = IdtEntry::new(interrupt_exception_13, selector);
        idt[14] = IdtEntry::new(interrupt_exception_14, selector);
        idt[15] = IdtEntry::new(interrupt_exception_15, selector);
        idt[16] = IdtEntry::new(interrupt_exception_16, selector);
        idt[17] = IdtEntry::new(interrupt_exception_17, selector);
        idt[18] = IdtEntry::new(interrupt_exception_18, selector);
        idt[19] = IdtEntry::new(interrupt_exception_19, selector);
        idt[20] = IdtEntry::new(interrupt_exception_20, selector);
        idt[21] = IdtEntry::new(interrupt_exception_21, selector);
        idt[22] = IdtEntry::new(interrupt_exception_22, selector);
        idt[23] = IdtEntry::new(interrupt_exception_23, selector);
        idt[24] = IdtEntry::new(interrupt_exception_24, selector);
        idt[25] = IdtEntry::new(interrupt_exception_25, selector);
        idt[26] = IdtEntry::new(interrupt_exception_26, selector);
        idt[27] = IdtEntry::new(interrupt_exception_27, selector);
        idt[28] = IdtEntry::new(interrupt_exception_28, selector);
        idt[29] = IdtEntry::new(interrupt_exception_29, selector);
        idt[30] = IdtEntry::new(interrupt_exception_30, selector);
        idt[31] = IdtEntry::new(interrupt_exception_31, selector);
        idt[TIMER_VECTOR as usize] = IdtEntry::new(interrupt_timer_stub, selector);
        idt[USER_SYSCALL_VECTOR as usize] =
            IdtEntry::new(interrupt_user_syscall_stub, selector).with_dpl(3);

        let idtr = Idtr {
            limit: (core::mem::size_of::<[IdtEntry; IDT_ENTRIES]>() - 1) as u16,
            base: idt.as_ptr() as u64,
        };
        lidt(&idtr);
    }

    true
}

pub fn activate_hardware() -> bool {
    if !IDT_LOADED.load(Ordering::Acquire)
        || HARDWARE_INTERRUPTS_ENABLED.swap(true, Ordering::SeqCst)
    {
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

pub fn wait_for_interrupt() {
    if HARDWARE_INTERRUPTS_ENABLED.load(Ordering::Acquire) {
        unsafe {
            core::arch::asm!("sti", "hlt", options(nomem, nostack));
        }
    } else {
        core::hint::spin_loop();
    }
}

pub fn verify_exception_path() -> bool {
    if !IDT_LOADED.load(Ordering::Acquire) {
        return false;
    }
    let before = BREAKPOINT_HITS.load(Ordering::Acquire);
    unsafe {
        core::arch::asm!("int3", options(nomem, nostack));
    }
    BREAKPOINT_HITS.load(Ordering::Acquire) == before.wrapping_add(1)
}

pub fn register_user_trap_handler(handler: UserTrapHandler) {
    unsafe {
        *USER_TRAP_HANDLER.0.get() = Some(handler);
    }
}

pub fn set_privilege_stack(stack_top: u64) -> bool {
    if !GDT_LOADED.load(Ordering::Acquire) || stack_top == 0 {
        return false;
    }
    unsafe {
        core::ptr::addr_of_mut!((*TSS.0.get()).privilege_stacks)
            .cast::<u64>()
            .write_unaligned(stack_top);
    }
    true
}

pub fn user_stack(frame: &TrapFrame) -> Option<(u64, u64)> {
    if frame.cs & 3 != 3 {
        return None;
    }
    let stack_fields = (frame as *const TrapFrame).cast::<u8>();
    unsafe {
        let user_rsp = stack_fields
            .add(core::mem::size_of::<TrapFrame>())
            .cast::<u64>()
            .read_unaligned();
        let user_ss = stack_fields
            .add(core::mem::size_of::<TrapFrame>() + core::mem::size_of::<u64>())
            .cast::<u64>()
            .read_unaligned();
        Some((user_rsp, user_ss))
    }
}

pub fn set_user_stack(frame: &mut TrapFrame, user_rsp: u64, user_ss: u64) -> bool {
    if frame.cs & 3 != 3 {
        return false;
    }
    let stack_fields = (frame as *mut TrapFrame).cast::<u8>();
    unsafe {
        stack_fields
            .add(core::mem::size_of::<TrapFrame>())
            .cast::<u64>()
            .write_unaligned(user_rsp);
        stack_fields
            .add(core::mem::size_of::<TrapFrame>() + core::mem::size_of::<u64>())
            .cast::<u64>()
            .write_unaligned(user_ss);
    }
    true
}

pub fn halt() -> ! {
    unsafe {
        disable_interrupts();
    }
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

pub fn status() -> InterruptStatus {
    InterruptStatus {
        gdt_loaded: GDT_LOADED.load(Ordering::Relaxed),
        task_register: read_tr(),
        idt_loaded: IDT_LOADED.load(Ordering::Relaxed),
        hardware_enabled: HARDWARE_INTERRUPTS_ENABLED.load(Ordering::Relaxed),
        ticks: TIMER_TICKS.load(Ordering::Relaxed),
        timer_hz: TIMER_HZ,
        breakpoint_hits: BREAKPOINT_HITS.load(Ordering::Relaxed),
    }
}

unsafe fn init_gdt_tss() {
    if GDT_LOADED.load(Ordering::Acquire) {
        return;
    }

    let tss = TSS.0.get();
    let stack_base = DOUBLE_FAULT_STACK.0.get() as usize;
    let stack_top = (stack_base + DOUBLE_FAULT_STACK_BYTES) & !0x0f;
    unsafe {
        core::ptr::addr_of_mut!((*tss).interrupt_stacks)
            .cast::<u64>()
            .write_unaligned(stack_top as u64);
        core::ptr::addr_of_mut!((*tss).io_map_base)
            .write_unaligned(core::mem::size_of::<TaskStateSegment>() as u16);
    }

    let (tss_low, tss_high) = tss_descriptor(tss as u64);
    let gdt = unsafe { &mut *GDT.0.get() };
    gdt[0] = 0;
    gdt[1] = 0x00af_9a00_0000_ffff;
    gdt[2] = 0x00cf_9200_0000_ffff;
    gdt[3] = tss_low;
    gdt[4] = tss_high;
    gdt[5] = 0x00cf_f200_0000_ffff;
    gdt[6] = 0x00af_fa00_0000_ffff;

    let gdtr = Gdtr {
        limit: (core::mem::size_of::<[u64; 7]>() - 1) as u16,
        base: gdt.as_ptr() as u64,
    };
    unsafe {
        core::arch::asm!(
            "lgdt [{gdtr}]",
            "push {code_selector}",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, {data_selector}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "xor eax, eax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ax, {tss_selector}",
            "ltr ax",
            gdtr = in(reg) &gdtr,
            code_selector = const KERNEL_CODE_SELECTOR,
            data_selector = const KERNEL_DATA_SELECTOR,
            tss_selector = const TSS_SELECTOR,
            out("rax") _,
        );
    }
    GDT_LOADED.store(true, Ordering::Release);
}

fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (core::mem::size_of::<TaskStateSegment>() - 1) as u64;
    let low = (limit & 0xffff)
        | ((base & 0x00ff_ffff) << 16)
        | (0x89_u64 << 40)
        | (((limit >> 16) & 0x0f) << 48)
        | (((base >> 24) & 0xff) << 56);
    (low, base >> 32)
}

#[unsafe(no_mangle)]
extern "C" fn interrupt_dispatch(frame: &mut TrapFrame) -> u64 {
    let vector = frame.vector as u8;
    let mut user_handled = false;
    if frame.cs & 3 == 3 {
        let fault_address = if vector == 14 { read_cr2() } else { 0 };
        let handler = unsafe { *USER_TRAP_HANDLER.0.get() };
        user_handled = handler.is_some_and(|handler| handler(frame, fault_address));
    }
    if vector == TIMER_VECTOR {
        record_timer_tick();
        return u64::from(user_handled && frame.cs & 3 == 0);
    }
    if user_handled {
        return u64::from(frame.cs & 3 == 0);
    }
    match vector {
        TIMER_VECTOR => {
            unreachable!()
        }
        3 => {
            BREAKPOINT_HITS.fetch_add(1, Ordering::Release);
            crate::serial::print(format_args!(
                "interrupts: breakpoint rip=0x{:016x}\r\n",
                frame.rip
            ));
        }
        14 => fatal_page_fault(frame),
        0..=31 => fatal_exception(
            exception_name(frame.vector as u8),
            frame,
            exception_has_error_code(frame.vector as u8).then_some(frame.error_code),
        ),
        _ => fatal_exception("unexpected interrupt", frame, Some(frame.error_code)),
    }
    0
}

fn record_timer_tick() {
    let ticks = TIMER_TICKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if !TIMER_SEEN.swap(true, Ordering::SeqCst) {
        crate::serial::print(format_args!(
            "interrupts: timer tick online hz={} vector={}\r\n",
            TIMER_HZ, TIMER_VECTOR
        ));
    }
    if ticks.is_multiple_of(TIMER_HZ as u64) {
        core::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
    unsafe {
        send_eoi(TIMER_VECTOR);
    }
}

fn fatal_page_fault(frame: &TrapFrame) -> ! {
    let fault_addr = read_cr2();
    crate::serial::print(format_args!(
        "interrupts: page fault rip=0x{:016x} addr=0x{:016x} err=0x{:016x} present={} write={} user={} reserved={} fetch={}\r\n",
        frame.rip,
        fault_addr,
        frame.error_code,
        frame.error_code & 1 != 0,
        frame.error_code & 2 != 0,
        frame.error_code & 4 != 0,
        frame.error_code & 8 != 0,
        frame.error_code & 16 != 0
    ));
    halt_forever()
}

fn exception_has_error_code(vector: u8) -> bool {
    matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
}

fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "divide error",
        1 => "debug exception",
        2 => "non-maskable interrupt",
        3 => "breakpoint",
        4 => "overflow",
        5 => "bound range exceeded",
        6 => "invalid opcode",
        7 => "device not available",
        8 => "double fault",
        9 => "coprocessor segment overrun",
        10 => "invalid tss",
        11 => "segment not present",
        12 => "stack-segment fault",
        13 => "general protection fault",
        14 => "page fault",
        16 => "x87 floating-point exception",
        17 => "alignment check",
        18 => "machine check",
        19 => "simd floating-point exception",
        20 => "virtualization exception",
        21 => "control protection exception",
        28 => "hypervisor injection exception",
        29 => "vmm communication exception",
        30 => "security exception",
        _ => "reserved exception",
    }
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
    halt()
}

unsafe fn remap_pic() {
    unsafe {
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
}

unsafe fn mask_pic(master_mask: u8, slave_mask: u8) {
    unsafe {
        outb(PIC1_DATA, master_mask);
        outb(PIC2_DATA, slave_mask);
    }
}

unsafe fn send_eoi(vector: u8) {
    unsafe {
        if vector >= 40 {
            outb(PIC2_CMD, PIC_EOI);
        }
        outb(PIC1_CMD, PIC_EOI);
    }
}

unsafe fn program_pit(hz: u32) {
    let divisor = (PIT_BASE_HZ / hz.max(1)) as u16;
    unsafe {
        outb(PIT_CMD, 0x36);
        outb(PIT_CH0, (divisor & 0x00FF) as u8);
        outb(PIT_CH0, (divisor >> 8) as u8);
    }
}

unsafe fn lidt(idtr: &Idtr) {
    unsafe {
        core::arch::asm!("lidt [{}]", in(reg) idtr, options(readonly, nostack, preserves_flags));
    }
}

unsafe fn enable_interrupts() {
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn disable_interrupts() {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }
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

fn read_tr() -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("str {:x}", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

unsafe fn io_wait() {
    unsafe {
        outb(0x80, 0);
    }
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

    .macro CALL_DISPATCH
        mov r12, rsp
        mov rdi, r12
        mov rcx, r12
        and rsp, -16
        sub rsp, 32
        cld
        call interrupt_dispatch
        test rax, rax
        jz 9f
        mov rax, qword ptr [r12 + {trap_rip_offset}]
        lea rsp, [r12 + {trap_frame_size}]
        jmp rax
    9:
        mov rsp, r12
    .endm

    .macro ISR_NOERR name, vector
        .global \name
    \name:
        push 0
        push \vector
        PUSH_GPRS
        CALL_DISPATCH
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    .macro ISR_EXCEPTION_NOERR vector
        .global interrupt_exception_\vector
    interrupt_exception_\vector:
        push 0
        push \vector
        PUSH_GPRS
        CALL_DISPATCH
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    .macro ISR_EXCEPTION_ERR vector
        .global interrupt_exception_\vector
    interrupt_exception_\vector:
        push \vector
        PUSH_GPRS
        CALL_DISPATCH
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    .macro ISR_ERR name, vector
        .global \name
    \name:
        push \vector
        PUSH_GPRS
        CALL_DISPATCH
        POP_GPRS
        add rsp, 16
        iretq
    .endm

    ISR_NOERR interrupt_default_stub, 255
    ISR_NOERR interrupt_timer_stub, 32
    ISR_NOERR interrupt_user_syscall_stub, 128

    ISR_EXCEPTION_NOERR 0
    ISR_EXCEPTION_NOERR 1
    ISR_EXCEPTION_NOERR 2
    ISR_EXCEPTION_NOERR 3
    ISR_EXCEPTION_NOERR 4
    ISR_EXCEPTION_NOERR 5
    ISR_EXCEPTION_NOERR 6
    ISR_EXCEPTION_NOERR 7
    ISR_EXCEPTION_ERR 8
    ISR_EXCEPTION_NOERR 9
    ISR_EXCEPTION_ERR 10
    ISR_EXCEPTION_ERR 11
    ISR_EXCEPTION_ERR 12
    ISR_EXCEPTION_ERR 13
    ISR_EXCEPTION_ERR 14
    ISR_EXCEPTION_NOERR 15
    ISR_EXCEPTION_NOERR 16
    ISR_EXCEPTION_ERR 17
    ISR_EXCEPTION_NOERR 18
    ISR_EXCEPTION_NOERR 19
    ISR_EXCEPTION_NOERR 20
    ISR_EXCEPTION_ERR 21
    ISR_EXCEPTION_NOERR 22
    ISR_EXCEPTION_NOERR 23
    ISR_EXCEPTION_NOERR 24
    ISR_EXCEPTION_NOERR 25
    ISR_EXCEPTION_NOERR 26
    ISR_EXCEPTION_NOERR 27
    ISR_EXCEPTION_NOERR 28
    ISR_EXCEPTION_ERR 29
    ISR_EXCEPTION_ERR 30
    ISR_EXCEPTION_NOERR 31

    "#,
    trap_frame_size = const core::mem::size_of::<TrapFrame>(),
    trap_rip_offset = const core::mem::offset_of!(TrapFrame, rip),
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_all_architectural_error_code_exceptions() {
        let vectors: alloc::vec::Vec<u8> =
            (0..32).filter(|&v| exception_has_error_code(v)).collect();
        assert_eq!(vectors, [8, 10, 11, 12, 13, 14, 17, 21, 29, 30]);
    }

    #[test]
    fn names_every_architectural_exception() {
        for vector in 0..32 {
            assert!(!exception_name(vector).is_empty());
        }
    }

    #[test]
    fn builds_a_valid_64_bit_tss_descriptor() {
        assert_eq!(core::mem::size_of::<TaskStateSegment>(), 104);
        let base = 0x1234_5678_9abc_def0;
        let (low, high) = tss_descriptor(base);
        let decoded_base =
            ((low >> 16) & 0x00ff_ffff) | (((low >> 56) & 0xff) << 24) | (high << 32);
        assert_eq!(decoded_base, base);
        assert_eq!((low >> 40) & 0xff, 0x89);
        assert_eq!(low & 0xffff, 103);
    }

    #[test]
    fn exposes_only_the_syscall_gate_to_ring_three() {
        let gate = IdtEntry::from_addr(0x1234, KERNEL_CODE_SELECTOR).with_dpl(3);
        assert_eq!((gate.type_attr >> 5) & 3, 3);
        let selector = gate.selector;
        assert_eq!(selector, KERNEL_CODE_SELECTOR);
    }

    #[test]
    fn trap_frame_prefix_ends_before_the_hardware_user_stack_fields() {
        assert_eq!(core::mem::size_of::<TrapFrame>(), 160);
    }

    #[test]
    fn reads_and_rewrites_hardware_user_stack_fields() {
        #[repr(C)]
        struct CompleteUserFrame {
            frame: TrapFrame,
            user_rsp: u64,
            user_ss: u64,
        }

        let mut complete = CompleteUserFrame {
            frame: TrapFrame {
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
                vector: u64::from(TIMER_VECTOR),
                error_code: 0,
                rip: 0x2000,
                cs: u64::from(USER_CODE_SELECTOR),
                rflags: 0x202,
            },
            user_rsp: 0x8000,
            user_ss: u64::from(USER_DATA_SELECTOR),
        };

        assert_eq!(
            user_stack(&complete.frame),
            Some((0x8000, u64::from(USER_DATA_SELECTOR)))
        );
        assert!(set_user_stack(&mut complete.frame, 0x9000, 0x2b));
        assert_eq!(complete.user_rsp, 0x9000);
        assert_eq!(complete.user_ss, 0x2b);
    }
}
