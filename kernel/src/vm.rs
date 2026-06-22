use alloc::vec::Vec;
use bootinfo::{
    BootInfo, KERNEL_SEGMENT_FLAG_EXECUTE, KERNEL_SEGMENT_FLAG_WRITE, KernelImageInfo, PAGE_SIZE,
    ReservedMemoryKind,
};
use core::cell::UnsafeCell;

use crate::memory;

const DEMO_VIRT_BASE: u64 = 0x0000_4000_0000_0000;
const HIGH_HALF_DIRECT_MAP_BASE: u64 = 0xffff_8000_0000_0000;
const IDENTITY_BOOT_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
const HIGH_HALF_BOOT_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
const STACK_WINDOW_BYTES: u64 = 2 * 1024 * 1024;
const PAGE_TABLE_ENTRIES: usize = 512;
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const ENTRY_PRESENT: u64 = 1 << 0;
const ENTRY_WRITABLE: u64 = 1 << 1;
const ENTRY_NO_EXECUTE: u64 = 1 << 63;

#[derive(Debug, Clone, Copy)]
pub struct VmStats {
    pub initialized: bool,
    pub committed: bool,
    pub active: bool,
    pub root_table_phys: u64,
    pub high_half_base: u64,
    pub table_pages: u64,
    pub mapped_pages: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Mapping {
    pub virt_start: u64,
    pub phys_start: u64,
    pub page_count: u64,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PageWalk {
    pub virt: u64,
    pub indices: [u16; 4],
    pub l4_entry: Option<u64>,
    pub l3_entry: Option<u64>,
    pub l2_entry: Option<u64>,
    pub l1_entry: Option<u64>,
    pub phys: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct L4Key([u16; 1]);

#[derive(Clone, Copy, PartialEq, Eq)]
struct L3Key([u16; 2]);

#[derive(Clone, Copy, PartialEq, Eq)]
struct L2Key([u16; 3]);

#[derive(Clone)]
struct PageTablePage {
    phys_addr: u64,
    entries: [u64; PAGE_TABLE_ENTRIES],
}

impl PageTablePage {
    const fn zeroed() -> Self {
        Self {
            phys_addr: 0,
            entries: [0; PAGE_TABLE_ENTRIES],
        }
    }

    fn new(phys_addr: u64) -> Self {
        Self {
            phys_addr,
            entries: [0; PAGE_TABLE_ENTRIES],
        }
    }
}

struct L3Table {
    key: L4Key,
    page: PageTablePage,
}

struct L2Table {
    key: L3Key,
    page: PageTablePage,
}

struct L1Table {
    key: L2Key,
    page: PageTablePage,
}

struct VirtualMemoryManager {
    root: PageTablePage,
    initialized: bool,
    committed: bool,
    active: bool,
    l3_tables: Vec<L3Table>,
    l2_tables: Vec<L2Table>,
    l1_tables: Vec<L1Table>,
    mappings: Vec<Mapping>,
    mapped_pages: u64,
    boot_report: Option<BootVmReport>,
}

impl VirtualMemoryManager {
    const fn new() -> Self {
        Self {
            root: PageTablePage::zeroed(),
            initialized: false,
            committed: false,
            active: false,
            l3_tables: Vec::new(),
            l2_tables: Vec::new(),
            l1_tables: Vec::new(),
            mappings: Vec::new(),
            mapped_pages: 0,
            boot_report: None,
        }
    }

    fn init(&mut self) -> Option<u64> {
        if self.initialized {
            return Some(self.root.phys_addr);
        }

        let root = memory::allocate_page()?;
        self.root = PageTablePage::new(root);
        self.initialized = true;
        self.committed = false;
        self.active = false;
        self.boot_report = None;
        Some(root)
    }

    fn stats(&self) -> VmStats {
        VmStats {
            initialized: self.initialized,
            committed: self.committed,
            active: self.active,
            root_table_phys: self.root.phys_addr,
            high_half_base: HIGH_HALF_DIRECT_MAP_BASE,
            table_pages: if self.initialized {
                1 + self.l3_tables.len() as u64
                    + self.l2_tables.len() as u64
                    + self.l1_tables.len() as u64
            } else {
                0
            },
            mapped_pages: self.mapped_pages,
        }
    }

    fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    fn map_range(
        &mut self,
        virt_start: u64,
        page_count: usize,
        writable: bool,
        executable: bool,
    ) -> Result<Mapping, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }
        if page_count == 0 {
            return Err(VmError::InvalidPageCount);
        }
        if !virt_start.is_multiple_of(PAGE_SIZE) {
            return Err(VmError::UnalignedVirtualAddress);
        }

        let total_pages = page_count as u64;
        let range_bytes = total_pages
            .checked_mul(PAGE_SIZE)
            .ok_or(VmError::AddressOverflow)?;
        let virt_end = virt_start
            .checked_add(range_bytes)
            .ok_or(VmError::AddressOverflow)?;

        if self.mappings.iter().any(|existing| {
            ranges_overlap(
                virt_start,
                virt_end,
                existing.virt_start,
                mapping_end(*existing),
            )
        }) {
            return Err(VmError::AlreadyMapped);
        }

        let mut first_phys = None;
        let mut newly_mapped_pages = 0_u64;
        for page_index in 0..page_count {
            let virt = virt_start + (page_index as u64 * PAGE_SIZE);
            let phys = memory::allocate_page().ok_or(VmError::OutOfPhysicalPages)?;
            if first_phys.is_none() {
                first_phys = Some(phys);
            }

            if self.ensure_leaf_mapping(virt, phys, writable, executable)? {
                newly_mapped_pages = newly_mapped_pages.saturating_add(1);
            }
        }

        self.mapped_pages = self.mapped_pages.saturating_add(newly_mapped_pages);
        let mapping = Mapping {
            virt_start,
            phys_start: first_phys.unwrap_or(0),
            page_count: newly_mapped_pages,
            writable,
            executable,
        };
        if newly_mapped_pages != 0 {
            self.mappings.push(mapping);
        }
        self.sync_pages()?;
        Ok(mapping)
    }

    fn map_identity_range(
        &mut self,
        start: u64,
        end: u64,
        writable: bool,
        executable: bool,
    ) -> Result<u64, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }

        let virt_start = align_down(start);
        let virt_end = align_up(end);
        if virt_end <= virt_start {
            return Ok(0);
        }

        let mut newly_mapped_pages = 0_u64;
        let mut current = virt_start;
        while current < virt_end {
            if self.ensure_leaf_mapping(current, current, writable, executable)? {
                newly_mapped_pages = newly_mapped_pages.saturating_add(1);
            }
            current = current.saturating_add(PAGE_SIZE);
        }

        if newly_mapped_pages != 0 {
            self.mapped_pages = self.mapped_pages.saturating_add(newly_mapped_pages);
            self.mappings.push(Mapping {
                virt_start,
                phys_start: virt_start,
                page_count: newly_mapped_pages,
                writable,
                executable,
            });
        }

        Ok(newly_mapped_pages)
    }

    fn map_identity_range_excluding(
        &mut self,
        start: u64,
        end: u64,
        holes: &[(u64, u64)],
        writable: bool,
        executable: bool,
    ) -> Result<u64, VmError> {
        let aligned_start = align_down(start);
        let aligned_end = align_up(end);
        if aligned_end <= aligned_start {
            return Ok(0);
        }

        let mut total_pages = 0_u64;
        let mut current = aligned_start;
        for &(hole_start, hole_end) in holes {
            let clipped_start = align_down(hole_start.max(aligned_start));
            let clipped_end = align_up(hole_end.min(aligned_end));
            if clipped_end <= clipped_start {
                continue;
            }
            if clipped_start > current {
                total_pages = total_pages.saturating_add(self.map_identity_range(
                    current,
                    clipped_start,
                    writable,
                    executable,
                )?);
            }
            current = current.max(clipped_end);
            if current >= aligned_end {
                break;
            }
        }

        if current < aligned_end {
            total_pages = total_pages.saturating_add(self.map_identity_range(
                current,
                aligned_end,
                writable,
                executable,
            )?);
        }

        Ok(total_pages)
    }

    fn map_window_range(
        &mut self,
        virt_start: u64,
        phys_start: u64,
        end: u64,
        writable: bool,
        executable: bool,
    ) -> Result<u64, VmError> {
        let aligned_phys_start = align_down(phys_start);
        let aligned_phys_end = align_up(end);
        if aligned_phys_end <= aligned_phys_start {
            return Ok(0);
        }
        let aligned_virt_start =
            virt_start.saturating_sub(phys_start.saturating_sub(aligned_phys_start));

        let page_count = aligned_phys_end
            .saturating_sub(aligned_phys_start)
            .checked_div(PAGE_SIZE)
            .ok_or(VmError::AddressOverflow)? as usize;
        self.map_fixed_range(
            aligned_virt_start,
            aligned_phys_start,
            page_count,
            writable,
            executable,
        )
    }

    fn map_fixed_range(
        &mut self,
        virt_start: u64,
        phys_start: u64,
        page_count: usize,
        writable: bool,
        executable: bool,
    ) -> Result<u64, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }
        if page_count == 0 {
            return Ok(0);
        }
        if !virt_start.is_multiple_of(PAGE_SIZE) {
            return Err(VmError::UnalignedVirtualAddress);
        }
        if !phys_start.is_multiple_of(PAGE_SIZE) {
            return Err(VmError::UnalignedPhysicalAddress);
        }

        let total_pages = page_count as u64;
        let range_bytes = total_pages
            .checked_mul(PAGE_SIZE)
            .ok_or(VmError::AddressOverflow)?;
        let _virt_end = virt_start
            .checked_add(range_bytes)
            .ok_or(VmError::AddressOverflow)?;

        let mut newly_mapped_pages = 0_u64;
        for page_index in 0..page_count {
            let virt = virt_start + (page_index as u64 * PAGE_SIZE);
            let phys = phys_start + (page_index as u64 * PAGE_SIZE);
            if self.ensure_leaf_mapping(virt, phys, writable, executable)? {
                newly_mapped_pages = newly_mapped_pages.saturating_add(1);
            }
        }

        if newly_mapped_pages != 0 {
            self.mapped_pages = self.mapped_pages.saturating_add(newly_mapped_pages);
            self.mappings.push(Mapping {
                virt_start,
                phys_start,
                page_count: newly_mapped_pages,
                writable,
                executable,
            });
        }

        Ok(newly_mapped_pages)
    }

    fn prepare_boot_identity_map(&mut self, boot_info: &BootInfo) -> Result<BootVmReport, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }

        let mut report = BootVmReport {
            identity_ranges: 0,
            identity_pages: 0,
            kernel_image_ranges: 0,
            kernel_image_pages: 0,
            kernel_writable_pages: 0,
            kernel_executable_pages: 0,
            kernel_wx_pages: 0,
            stack_window_start: 0,
            stack_window_pages: 0,
            higher_half_ranges: 0,
            higher_half_pages: 0,
            higher_half_base: HIGH_HALF_DIRECT_MAP_BASE,
        };

        let kernel_holes = kernel_virtual_holes(boot_info, 0, IDENTITY_BOOT_WINDOW_BYTES);
        let pages = self
            .map_identity_range_excluding(0, IDENTITY_BOOT_WINDOW_BYTES, &kernel_holes, true, true)
            .map_err(|err| remap_conflict(err, VmError::BootIdentityConflict))?;
        if pages != 0 {
            report.identity_ranges += 1;
            report.identity_pages = report.identity_pages.saturating_add(pages);
        }

        let stack_pointer = current_stack_pointer();
        let stack_start = stack_pointer.saturating_sub(STACK_WINDOW_BYTES / 2);
        let stack_end = stack_pointer.saturating_add(STACK_WINDOW_BYTES / 2);
        let pages = self
            .map_identity_range(stack_start, stack_end, true, false)
            .map_err(|err| remap_conflict(err, VmError::BootStackConflict))?;
        if pages != 0 {
            report.identity_ranges += 1;
            report.identity_pages = report.identity_pages.saturating_add(pages);
            report.stack_window_start = align_down(stack_start);
            report.stack_window_pages = pages;
        }

        let boot_window_virt = higher_half_phys(0).ok_or(VmError::AddressOverflow)?;
        let pages = self
            .map_window_range(
                boot_window_virt,
                0,
                HIGH_HALF_BOOT_WINDOW_BYTES,
                true,
                false,
            )
            .map_err(|err| remap_conflict(err, VmError::BootWindowConflict))?;
        if pages != 0 {
            report.higher_half_ranges += 1;
            report.higher_half_pages = report.higher_half_pages.saturating_add(pages);
        }

        for range in boot_info.reserved_memory() {
            let identity_executable = matches!(
                range.kind,
                ReservedMemoryKind::LoaderImage | ReservedMemoryKind::LowMemory
            );
            let keep_identity = matches!(
                range.kind,
                ReservedMemoryKind::LoaderImage
                    | ReservedMemoryKind::LowMemory
                    | ReservedMemoryKind::RuntimeHeap
                    | ReservedMemoryKind::KernelHeap
                    | ReservedMemoryKind::DescriptorTables
            );
            if keep_identity {
                let pages = self
                    .map_identity_range(range.start, range.end(), true, identity_executable)
                    .map_err(|err| remap_conflict(err, VmError::ReservedIdentityConflict))?;
                if pages != 0 {
                    report.identity_ranges += 1;
                    report.identity_pages = report.identity_pages.saturating_add(pages);
                }
            }

            let window_virt = higher_half_phys(range.start).ok_or(VmError::AddressOverflow)?;
            let pages = self
                .map_window_range(window_virt, range.start, range.end(), true, false)
                .map_err(|err| remap_conflict(err, VmError::ReservedWindowConflict))?;
            if pages != 0 {
                report.higher_half_ranges += 1;
                report.higher_half_pages = report.higher_half_pages.saturating_add(pages);
            }
        }

        for mapping in kernel_page_mappings(&boot_info.kernel_image)? {
            let pages = self
                .map_fixed_range(
                    mapping.virt_start,
                    mapping.phys_start,
                    mapping.page_count,
                    mapping.writable,
                    mapping.executable,
                )
                .map_err(|err| remap_conflict(err, VmError::KernelImageConflict))?;
            report.kernel_image_ranges = report.kernel_image_ranges.saturating_add(1);
            report.kernel_image_pages = report.kernel_image_pages.saturating_add(pages);
            if mapping.writable {
                report.kernel_writable_pages = report.kernel_writable_pages.saturating_add(pages);
            }
            if mapping.executable {
                report.kernel_executable_pages =
                    report.kernel_executable_pages.saturating_add(pages);
            }
            if mapping.writable && mapping.executable {
                report.kernel_wx_pages = report.kernel_wx_pages.saturating_add(pages);
            }
        }

        self.sync_pages()?;
        self.boot_report = Some(report);
        Ok(report)
    }

    fn get_or_create_l3(&mut self, l4_index: u16) -> Result<usize, VmError> {
        let key = L4Key([l4_index]);
        if let Some(index) = self.l3_tables.iter().position(|table| table.key == key) {
            return Ok(index);
        }

        let phys = memory::allocate_page().ok_or(VmError::OutOfPhysicalPages)?;
        self.root.entries[l4_index as usize] = make_table_entry(phys);
        self.committed = false;
        self.l3_tables.push(L3Table {
            key,
            page: PageTablePage::new(phys),
        });
        Ok(self.l3_tables.len() - 1)
    }

    fn get_or_create_l2(
        &mut self,
        l4_index: u16,
        l3_index: u16,
        parent_l3_index: usize,
    ) -> Result<usize, VmError> {
        let key = L3Key([l4_index, l3_index]);
        if let Some(index) = self.l2_tables.iter().position(|table| table.key == key) {
            return Ok(index);
        }

        let phys = memory::allocate_page().ok_or(VmError::OutOfPhysicalPages)?;
        self.l3_tables[parent_l3_index].page.entries[l3_index as usize] = make_table_entry(phys);
        self.committed = false;
        self.l2_tables.push(L2Table {
            key,
            page: PageTablePage::new(phys),
        });
        Ok(self.l2_tables.len() - 1)
    }

    fn get_or_create_l1(
        &mut self,
        l4_index: u16,
        l3_index: u16,
        l2_index: u16,
        parent_l2_index: usize,
    ) -> Result<usize, VmError> {
        let key = L2Key([l4_index, l3_index, l2_index]);
        if let Some(index) = self.l1_tables.iter().position(|table| table.key == key) {
            return Ok(index);
        }

        let phys = memory::allocate_page().ok_or(VmError::OutOfPhysicalPages)?;
        self.l2_tables[parent_l2_index].page.entries[l2_index as usize] = make_table_entry(phys);
        self.committed = false;
        self.l1_tables.push(L1Table {
            key,
            page: PageTablePage::new(phys),
        });
        Ok(self.l1_tables.len() - 1)
    }

    fn walk(&self, virt: u64) -> PageWalk {
        let indices = split_indices(virt);
        let offset = virt & (PAGE_SIZE - 1);

        let l4_entry = self.root.entries[indices[0] as usize];
        if !is_present(l4_entry) {
            return PageWalk {
                virt,
                indices,
                l4_entry: None,
                l3_entry: None,
                l2_entry: None,
                l1_entry: None,
                phys: None,
            };
        }

        let l3_table = self.find_l3(entry_address(l4_entry));
        let Some(l3_table) = l3_table else {
            return PageWalk {
                virt,
                indices,
                l4_entry: Some(l4_entry),
                l3_entry: None,
                l2_entry: None,
                l1_entry: None,
                phys: None,
            };
        };

        let l3_entry = l3_table.entries[indices[1] as usize];
        if !is_present(l3_entry) {
            return PageWalk {
                virt,
                indices,
                l4_entry: Some(l4_entry),
                l3_entry: None,
                l2_entry: None,
                l1_entry: None,
                phys: None,
            };
        }

        let l2_table = self.find_l2(entry_address(l3_entry));
        let Some(l2_table) = l2_table else {
            return PageWalk {
                virt,
                indices,
                l4_entry: Some(l4_entry),
                l3_entry: Some(l3_entry),
                l2_entry: None,
                l1_entry: None,
                phys: None,
            };
        };

        let l2_entry = l2_table.entries[indices[2] as usize];
        if !is_present(l2_entry) {
            return PageWalk {
                virt,
                indices,
                l4_entry: Some(l4_entry),
                l3_entry: Some(l3_entry),
                l2_entry: None,
                l1_entry: None,
                phys: None,
            };
        }

        let l1_table = self.find_l1(entry_address(l2_entry));
        let Some(l1_table) = l1_table else {
            return PageWalk {
                virt,
                indices,
                l4_entry: Some(l4_entry),
                l3_entry: Some(l3_entry),
                l2_entry: Some(l2_entry),
                l1_entry: None,
                phys: None,
            };
        };

        let l1_entry = l1_table.entries[indices[3] as usize];
        let phys = if is_present(l1_entry) {
            Some(entry_address(l1_entry).saturating_add(offset))
        } else {
            None
        };

        PageWalk {
            virt,
            indices,
            l4_entry: Some(l4_entry),
            l3_entry: Some(l3_entry),
            l2_entry: Some(l2_entry),
            l1_entry: if is_present(l1_entry) {
                Some(l1_entry)
            } else {
                None
            },
            phys,
        }
    }

    fn translate(&self, virt: u64) -> Option<u64> {
        self.walk(virt).phys
    }

    fn activate(&mut self) -> Result<u64, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }
        if !self.committed {
            self.sync_pages()?;
        }

        load_cr3(self.root.phys_addr);
        self.active = true;
        Ok(self.root.phys_addr)
    }

    fn adopt_active_root(&mut self, root_table_phys: u64) -> Result<u64, VmError> {
        if !root_table_phys.is_multiple_of(PAGE_SIZE) {
            return Err(VmError::UnalignedPhysicalAddress);
        }

        self.root = PageTablePage::new(root_table_phys);
        self.initialized = true;
        self.committed = true;
        self.active = true;
        self.l3_tables.clear();
        self.l2_tables.clear();
        self.l1_tables.clear();
        self.mappings.clear();
        self.mapped_pages = 0;
        self.boot_report = None;
        Ok(root_table_phys)
    }

    fn sync_pages(&mut self) -> Result<u64, VmError> {
        if !self.initialized {
            return Err(VmError::NotInitialized);
        }

        write_page_table(&self.root, self.active);
        for table in &self.l3_tables {
            write_page_table(&table.page, self.active);
        }
        for table in &self.l2_tables {
            write_page_table(&table.page, self.active);
        }
        for table in &self.l1_tables {
            write_page_table(&table.page, self.active);
        }

        self.committed = true;
        Ok(self.root.phys_addr)
    }

    fn read_table_entry(&self, table_phys: u64, index: usize) -> Option<u64> {
        if !self.committed || index >= PAGE_TABLE_ENTRIES {
            return None;
        }

        Some(read_table_entry(table_phys, index, self.active))
    }

    fn probe_higher_half(&self) -> Option<HhdmProbe> {
        if !self.active {
            return None;
        }

        let virt_addr = higher_half_phys(self.root.phys_addr)?;
        Some(HhdmProbe {
            virt_addr,
            root_entry0: read_u64(virt_addr),
        })
    }

    fn boot_report(&self) -> Option<BootVmReport> {
        self.boot_report
    }

    fn ensure_leaf_mapping(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
        executable: bool,
    ) -> Result<bool, VmError> {
        let indices = split_indices(virt);
        let l3_index = self.get_or_create_l3(indices[0])?;
        let l2_index = self.get_or_create_l2(indices[0], indices[1], l3_index)?;
        let l1_index = self.get_or_create_l1(indices[0], indices[1], indices[2], l2_index)?;

        let entry = make_leaf_entry(phys, writable, executable);
        let l1_page = &mut self.l1_tables[l1_index].page;
        let slot = indices[3] as usize;
        let existing = l1_page.entries[slot];
        if is_present(existing) {
            if existing == entry {
                return Ok(false);
            }
            return Err(VmError::AlreadyMapped);
        }

        l1_page.entries[slot] = entry;
        self.committed = false;
        Ok(true)
    }

    fn find_l3(&self, phys: u64) -> Option<&PageTablePage> {
        self.l3_tables
            .iter()
            .find(|table| table.page.phys_addr == phys)
            .map(|table| &table.page)
    }

    fn find_l2(&self, phys: u64) -> Option<&PageTablePage> {
        self.l2_tables
            .iter()
            .find(|table| table.page.phys_addr == phys)
            .map(|table| &table.page)
    }

    fn find_l1(&self, phys: u64) -> Option<&PageTablePage> {
        self.l1_tables
            .iter()
            .find(|table| table.page.phys_addr == phys)
            .map(|table| &table.page)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    FirmwareServicesActive,
    NotInitialized,
    InvalidPageCount,
    UnalignedVirtualAddress,
    UnalignedPhysicalAddress,
    BootIdentityConflict,
    BootStackConflict,
    BootWindowConflict,
    ReservedIdentityConflict,
    ReservedWindowConflict,
    KernelImageConflict,
    KernelWritableExecutable,
    AddressOverflow,
    AlreadyMapped,
    OutOfPhysicalPages,
}

#[derive(Debug, Clone, Copy)]
pub struct BootVmReport {
    pub identity_ranges: u64,
    pub identity_pages: u64,
    pub kernel_image_ranges: u64,
    pub kernel_image_pages: u64,
    pub kernel_writable_pages: u64,
    pub kernel_executable_pages: u64,
    pub kernel_wx_pages: u64,
    pub stack_window_start: u64,
    pub stack_window_pages: u64,
    pub higher_half_ranges: u64,
    pub higher_half_pages: u64,
    pub higher_half_base: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct HhdmProbe {
    pub virt_addr: u64,
    pub root_entry0: u64,
}

struct VmCell(UnsafeCell<VirtualMemoryManager>);

unsafe impl Sync for VmCell {}

static VM: VmCell = VmCell(UnsafeCell::new(VirtualMemoryManager::new()));

pub fn init() -> Option<u64> {
    unsafe { (*VM.0.get()).init() }
}

pub fn stats() -> VmStats {
    unsafe { (*VM.0.get()).stats() }
}

pub fn map_demo_range(page_count: usize) -> Result<Mapping, VmError> {
    let virt_base = next_demo_base();
    unsafe { (*VM.0.get()).map_range(virt_base, page_count, true, false) }
}

pub fn prepare_boot_identity_map(boot_info: &BootInfo) -> Result<BootVmReport, VmError> {
    unsafe { (*VM.0.get()).prepare_boot_identity_map(boot_info) }
}

pub fn sync() -> Result<u64, VmError> {
    unsafe { (*VM.0.get()).sync_pages() }
}

pub fn activate() -> Result<u64, VmError> {
    unsafe { (*VM.0.get()).activate() }
}

pub fn adopt_current_root() -> Result<u64, VmError> {
    unsafe { (*VM.0.get()).adopt_active_root(read_cr3()) }
}

pub fn translate(virt: u64) -> Option<u64> {
    unsafe { (*VM.0.get()).translate(virt) }
}

pub fn walk(virt: u64) -> PageWalk {
    unsafe { (*VM.0.get()).walk(virt) }
}

pub fn mappings() -> Vec<Mapping> {
    unsafe { (*VM.0.get()).mappings().to_vec() }
}

pub fn read_committed_entry(table_phys: u64, index: usize) -> Option<u64> {
    unsafe { (*VM.0.get()).read_table_entry(table_phys, index) }
}

pub fn probe_higher_half() -> Option<HhdmProbe> {
    unsafe { (*VM.0.get()).probe_higher_half() }
}

pub fn boot_report() -> Option<BootVmReport> {
    unsafe { (*VM.0.get()).boot_report() }
}

pub fn high_half_base() -> u64 {
    HIGH_HALF_DIRECT_MAP_BASE
}

pub fn physical_to_high_half(phys: u64) -> Option<u64> {
    higher_half_phys(phys)
}

fn next_demo_base() -> u64 {
    let stats = stats();
    DEMO_VIRT_BASE + stats.mapped_pages.saturating_mul(PAGE_SIZE)
}

fn split_indices(virt: u64) -> [u16; 4] {
    [
        ((virt >> 39) & 0x1ff) as u16,
        ((virt >> 30) & 0x1ff) as u16,
        ((virt >> 21) & 0x1ff) as u16,
        ((virt >> 12) & 0x1ff) as u16,
    ]
}

fn mapping_end(mapping: Mapping) -> u64 {
    mapping
        .virt_start
        .saturating_add(mapping.page_count.saturating_mul(PAGE_SIZE))
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && b_start < a_end
}

fn make_table_entry(phys: u64) -> u64 {
    (phys & ADDRESS_MASK) | ENTRY_PRESENT | ENTRY_WRITABLE
}

fn make_leaf_entry(phys: u64, writable: bool, executable: bool) -> u64 {
    let mut entry = (phys & ADDRESS_MASK) | ENTRY_PRESENT;
    if writable {
        entry |= ENTRY_WRITABLE;
    }
    if !executable {
        entry |= ENTRY_NO_EXECUTE;
    }
    entry
}

fn is_present(entry: u64) -> bool {
    entry & ENTRY_PRESENT != 0
}

fn entry_address(entry: u64) -> u64 {
    entry & ADDRESS_MASK
}

fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> u64 {
    value.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn higher_half_phys(phys: u64) -> Option<u64> {
    HIGH_HALF_DIRECT_MAP_BASE.checked_add(phys)
}

fn kernel_virtual_holes(boot_info: &BootInfo, start: u64, end: u64) -> Vec<(u64, u64)> {
    let mut holes = Vec::new();
    for segment in boot_info.kernel_image.segments() {
        if segment.load_page_count == 0 || segment.memory_size == 0 {
            continue;
        }

        let hole_start = align_down(segment.virtual_address.max(start));
        let hole_end = align_up(
            segment
                .virtual_address
                .saturating_add(segment.memory_size)
                .min(end),
        );
        if hole_end > hole_start {
            holes.push((hole_start, hole_end));
        }
    }

    holes.sort_unstable_by_key(|(hole_start, _)| *hole_start);
    holes
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct KernelPagePermissions {
    present: bool,
    writable: bool,
    executable: bool,
}

struct KernelPageMapping {
    virt_start: u64,
    phys_start: u64,
    page_count: usize,
    writable: bool,
    executable: bool,
}

fn kernel_page_mappings(info: &KernelImageInfo) -> Result<Vec<KernelPageMapping>, VmError> {
    if info.load_page_count == 0 || info.load_base == 0 {
        return Ok(Vec::new());
    }

    let image_base = info
        .segments()
        .iter()
        .filter(|segment| segment.load_page_count != 0 && segment.memory_size != 0)
        .map(|segment| align_down(segment.virtual_address))
        .min()
        .ok_or(VmError::KernelImageConflict)?;
    let page_count = usize::try_from(info.load_page_count).map_err(|_| VmError::AddressOverflow)?;
    let mut plan = alloc::vec![KernelPagePermissions::default(); page_count];

    for segment in info.segments() {
        if segment.load_page_count == 0 || segment.memory_size == 0 {
            continue;
        }
        let segment_start = align_down(segment.virtual_address);
        let first_page = segment_start
            .checked_sub(image_base)
            .ok_or(VmError::KernelImageConflict)?
            / PAGE_SIZE;
        let first_page = usize::try_from(first_page).map_err(|_| VmError::AddressOverflow)?;
        let segment_pages =
            usize::try_from(segment.load_page_count).map_err(|_| VmError::AddressOverflow)?;
        let end_page = first_page
            .checked_add(segment_pages)
            .ok_or(VmError::AddressOverflow)?;
        if end_page > plan.len() {
            return Err(VmError::KernelImageConflict);
        }

        let writable = segment.flags & KERNEL_SEGMENT_FLAG_WRITE != 0;
        let executable = segment.flags & KERNEL_SEGMENT_FLAG_EXECUTE != 0;
        for page in &mut plan[first_page..end_page] {
            page.present = true;
            page.writable |= writable;
            page.executable |= executable;
            if page.writable && page.executable {
                return Err(VmError::KernelWritableExecutable);
            }
        }
    }

    let mut mappings = Vec::new();
    let mut index = 0usize;
    while index < plan.len() {
        let permissions = plan[index];
        if !permissions.present {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < plan.len() && plan[index] == permissions {
            index += 1;
        }
        let page_offset = (start as u64)
            .checked_mul(PAGE_SIZE)
            .ok_or(VmError::AddressOverflow)?;
        mappings.push(KernelPageMapping {
            virt_start: image_base
                .checked_add(page_offset)
                .ok_or(VmError::AddressOverflow)?,
            phys_start: info
                .load_base
                .checked_add(page_offset)
                .ok_or(VmError::AddressOverflow)?,
            page_count: index - start,
            writable: permissions.writable,
            executable: permissions.executable,
        });
    }
    Ok(mappings)
}

fn remap_conflict(error: VmError, remapped: VmError) -> VmError {
    if matches!(error, VmError::AlreadyMapped) {
        remapped
    } else {
        error
    }
}

#[cfg(target_arch = "x86_64")]
fn current_stack_pointer() -> u64 {
    let stack: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) stack,
            options(nomem, nostack, preserves_flags)
        );
    }
    stack
}

#[cfg(not(target_arch = "x86_64"))]
fn current_stack_pointer() -> u64 {
    0
}

fn write_page_table(page: &PageTablePage, active: bool) {
    unsafe {
        core::ptr::copy_nonoverlapping(
            page.entries.as_ptr(),
            phys_to_mut_ptr::<u64>(page.phys_addr, active),
            PAGE_TABLE_ENTRIES,
        );
    }
}

fn read_table_entry(table_phys: u64, index: usize, active: bool) -> u64 {
    unsafe { *phys_to_const_ptr::<u64>(table_phys, active).add(index) }
}

fn read_u64(addr: u64) -> u64 {
    unsafe { *(addr as *const u64) }
}

fn phys_to_const_ptr<T>(phys: u64, active: bool) -> *const T {
    phys_to_addr(phys, active) as *const T
}

fn phys_to_mut_ptr<T>(phys: u64, active: bool) -> *mut T {
    phys_to_addr(phys, active) as *mut T
}

fn phys_to_addr(phys: u64, active: bool) -> u64 {
    if active {
        higher_half_phys(phys).unwrap_or(phys)
    } else {
        phys
    }
}

#[cfg(target_arch = "x86_64")]
fn load_cr3(root_table_phys: u64) {
    unsafe {
        core::arch::asm!(
            "mov cr3, {root}",
            root = in(reg) root_table_phys,
            options(nostack, preserves_flags)
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn read_cr3() -> u64 {
    let root_table_phys: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, cr3",
            out(reg) root_table_phys,
            options(nostack, preserves_flags)
        );
    }
    root_table_phys
}

#[cfg(not(target_arch = "x86_64"))]
fn read_cr3() -> u64 {
    0
}

#[cfg(not(target_arch = "x86_64"))]
fn load_cr3(_root_table_phys: u64) {}

#[cfg(test)]
mod tests {
    use super::*;
    use bootinfo::{
        KERNEL_SEGMENT_FLAG_EXECUTE, KERNEL_SEGMENT_FLAG_READ, KERNEL_SEGMENT_FLAG_WRITE,
        KernelImageSegment,
    };

    fn segment(virtual_address: u64, pages: u64, flags: u32) -> KernelImageSegment {
        KernelImageSegment {
            virtual_address,
            physical_address: virtual_address,
            file_offset: 0,
            file_size: pages * PAGE_SIZE,
            memory_size: pages * PAGE_SIZE,
            flags,
            load_address: 0,
            load_page_count: pages,
        }
    }

    #[test]
    fn builds_distinct_rx_ro_and_rw_kernel_ranges() {
        let mut image = KernelImageInfo {
            load_base: 0x10_0000,
            load_page_count: 4,
            load_segment_count: 3,
            load_segment_total: 3,
            loaded_segment_count: 3,
            ..KernelImageInfo::EMPTY
        };
        image.segments[0] = segment(
            0x20_0000,
            2,
            KERNEL_SEGMENT_FLAG_READ | KERNEL_SEGMENT_FLAG_EXECUTE,
        );
        image.segments[1] = segment(0x20_2000, 1, KERNEL_SEGMENT_FLAG_READ);
        image.segments[2] = segment(
            0x20_3000,
            1,
            KERNEL_SEGMENT_FLAG_READ | KERNEL_SEGMENT_FLAG_WRITE,
        );

        let mappings = kernel_page_mappings(&image).unwrap();
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].page_count, 2);
        assert!(mappings[0].executable && !mappings[0].writable);
        assert!(!mappings[1].executable && !mappings[1].writable);
        assert!(!mappings[2].executable && mappings[2].writable);
    }

    #[test]
    fn rejects_overlapping_writable_and_executable_segments() {
        let mut image = KernelImageInfo {
            load_base: 0x10_0000,
            load_page_count: 3,
            load_segment_count: 2,
            load_segment_total: 2,
            loaded_segment_count: 2,
            ..KernelImageInfo::EMPTY
        };
        image.segments[0] = segment(
            0x20_0000,
            2,
            KERNEL_SEGMENT_FLAG_READ | KERNEL_SEGMENT_FLAG_EXECUTE,
        );
        image.segments[1] = segment(
            0x20_1000,
            2,
            KERNEL_SEGMENT_FLAG_READ | KERNEL_SEGMENT_FLAG_WRITE,
        );

        assert!(matches!(
            kernel_page_mappings(&image),
            Err(VmError::KernelWritableExecutable)
        ));
    }
}
