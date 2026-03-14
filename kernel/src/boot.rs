use bootinfo::{BootInfo, ReservedMemoryKind, ReservedMemoryRange};
use core::cell::UnsafeCell;

use crate::vm;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPhase {
    FirmwareEntry,
    VmInitialized,
    PostEbsActive,
}

impl BootPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FirmwareEntry => "firmware-entry",
            Self::VmInitialized => "vm-initialized",
            Self::PostEbsActive => "post-ebs-active",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReservedAlias {
    pub range: ReservedMemoryRange,
    pub virt: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct BootSnapshot {
    pub phase: BootPhase,
    pub root_table_phys: u64,
    pub hhdm_base: u64,
    pub boot_report: Option<vm::BootVmReport>,
    pub hhdm_probe: Option<vm::HhdmProbe>,
    pub framebuffer_alias: Option<u64>,
    pub loader_alias: Option<ReservedAlias>,
    pub memory_map_alias: Option<ReservedAlias>,
}

struct BootState {
    phase: BootPhase,
    root_table_phys: u64,
    hhdm_base: u64,
    boot_report: Option<vm::BootVmReport>,
    hhdm_probe: Option<vm::HhdmProbe>,
    framebuffer_alias: Option<u64>,
    loader_alias: Option<ReservedAlias>,
    memory_map_alias: Option<ReservedAlias>,
}

impl BootState {
    const fn new() -> Self {
        Self {
            phase: BootPhase::FirmwareEntry,
            root_table_phys: 0,
            hhdm_base: 0,
            boot_report: None,
            hhdm_probe: None,
            framebuffer_alias: None,
            loader_alias: None,
            memory_map_alias: None,
        }
    }

    fn record_firmware_entry(&mut self) {
        self.phase = BootPhase::FirmwareEntry;
        self.hhdm_base = vm::high_half_base();
        self.boot_report = None;
        self.hhdm_probe = None;
        self.framebuffer_alias = None;
        self.loader_alias = None;
        self.memory_map_alias = None;
    }

    fn record_vm_initialized(&mut self, root_table_phys: u64) {
        self.phase = BootPhase::VmInitialized;
        self.root_table_phys = root_table_phys;
        self.hhdm_base = vm::high_half_base();
    }

    fn record_post_ebs_active(&mut self, boot_info: &BootInfo, root_table_phys: u64) {
        self.phase = BootPhase::PostEbsActive;
        self.root_table_phys = root_table_phys;
        self.hhdm_base = vm::high_half_base();
        self.boot_report = vm::boot_report();
        self.hhdm_probe = vm::probe_higher_half();
        self.framebuffer_alias = vm::physical_to_high_half(boot_info.framebuffer.base as u64);
        self.loader_alias = reserved_alias(boot_info, ReservedMemoryKind::LoaderImage);
        self.memory_map_alias = reserved_alias(boot_info, ReservedMemoryKind::MemoryMap);
    }

    fn snapshot(&self) -> BootSnapshot {
        BootSnapshot {
            phase: self.phase,
            root_table_phys: self.root_table_phys,
            hhdm_base: self.hhdm_base,
            boot_report: self.boot_report,
            hhdm_probe: self.hhdm_probe,
            framebuffer_alias: self.framebuffer_alias,
            loader_alias: self.loader_alias,
            memory_map_alias: self.memory_map_alias,
        }
    }
}

struct BootCell(UnsafeCell<BootState>);

unsafe impl Sync for BootCell {}

static BOOT_STATE: BootCell = BootCell(UnsafeCell::new(BootState::new()));

pub fn record_firmware_entry() {
    unsafe {
        (*BOOT_STATE.0.get()).record_firmware_entry();
    }
}

pub fn record_vm_initialized(root_table_phys: u64) {
    unsafe {
        (*BOOT_STATE.0.get()).record_vm_initialized(root_table_phys);
    }
}

pub fn record_post_ebs_active(boot_info: &BootInfo, root_table_phys: u64) {
    unsafe {
        (*BOOT_STATE.0.get()).record_post_ebs_active(boot_info, root_table_phys);
    }
}

pub fn snapshot() -> BootSnapshot {
    unsafe { (*BOOT_STATE.0.get()).snapshot() }
}

fn reserved_alias(boot_info: &BootInfo, kind: ReservedMemoryKind) -> Option<ReservedAlias> {
    boot_info.reserved_range(kind).and_then(|range| {
        vm::physical_to_high_half(range.start).map(|virt| ReservedAlias { range, virt })
    })
}
