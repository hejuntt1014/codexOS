use bootinfo::{
    BootInfo, MAX_MEMORY_REGIONS, MAX_RESERVED_MEMORY_RANGES, MemoryRegionKind, PAGE_SIZE,
    ReservedMemoryRange,
};
use core::cell::UnsafeCell;

const MAX_ALLOCATOR_REGIONS: usize = MAX_MEMORY_REGIONS + MAX_RESERVED_MEMORY_RANGES + 8;

#[derive(Debug, Clone, Copy)]
struct RegionCursor {
    start: u64,
    page_count: u64,
    next_page: u64,
    kind: MemoryRegionKind,
}

impl RegionCursor {
    const EMPTY: Self = Self {
        start: 0,
        page_count: 0,
        next_page: 0,
        kind: MemoryRegionKind::Reserved,
    };

    const fn remaining_pages(self) -> u64 {
        self.page_count.saturating_sub(self.next_page)
    }
}

#[derive(Debug, Clone, Copy)]
struct RegionSpan {
    start: u64,
    end: u64,
}

impl RegionSpan {
    const EMPTY: Self = Self { start: 0, end: 0 };

    const fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryStats {
    pub usable_region_count: usize,
    pub total_usable_pages: u64,
    pub allocated_pages: u64,
    pub remaining_pages: u64,
}

struct PhysicalPageAllocator {
    initialized: bool,
    usable_region_count: usize,
    regions: [RegionCursor; MAX_ALLOCATOR_REGIONS],
    total_usable_pages: u64,
    allocated_pages: u64,
}

impl PhysicalPageAllocator {
    const fn new() -> Self {
        Self {
            initialized: false,
            usable_region_count: 0,
            regions: [RegionCursor::EMPTY; MAX_ALLOCATOR_REGIONS],
            total_usable_pages: 0,
            allocated_pages: 0,
        }
    }

    fn init(&mut self, boot_info: &BootInfo) {
        self.initialized = true;
        self.usable_region_count = 0;
        self.regions = [RegionCursor::EMPTY; MAX_ALLOCATOR_REGIONS];
        self.total_usable_pages = 0;
        self.allocated_pages = 0;

        for region in boot_info.memory_regions() {
            if !boot_info
                .firmware_mode
                .region_is_currently_usable(region.kind)
                || region.page_count == 0
            {
                continue;
            }

            self.add_region_fragments(
                region.start,
                region.start.saturating_add(region.size_bytes()),
                region.kind,
                boot_info.reserved_memory(),
            );
        }
    }

    fn allocate_page(&mut self) -> Option<u64> {
        if !self.initialized {
            return None;
        }

        for region in &mut self.regions[..self.usable_region_count] {
            if region.remaining_pages() == 0 {
                continue;
            }

            let address = region
                .start
                .saturating_add(region.next_page.saturating_mul(PAGE_SIZE));
            region.next_page = region.next_page.saturating_add(1);
            self.allocated_pages = self.allocated_pages.saturating_add(1);
            return Some(address);
        }

        None
    }

    fn stats(&self) -> MemoryStats {
        MemoryStats {
            usable_region_count: self.usable_region_count,
            total_usable_pages: self.total_usable_pages,
            allocated_pages: self.allocated_pages,
            remaining_pages: self.total_usable_pages.saturating_sub(self.allocated_pages),
        }
    }

    fn first_usable_kind(&self) -> MemoryRegionKind {
        self.regions[..self.usable_region_count]
            .first()
            .copied()
            .unwrap_or(RegionCursor::EMPTY)
            .kind
    }

    fn add_region_fragments(
        &mut self,
        start: u64,
        end: u64,
        kind: MemoryRegionKind,
        reserved: &[ReservedMemoryRange],
    ) {
        let mut active = [RegionSpan::EMPTY; MAX_RESERVED_MEMORY_RANGES + 1];
        let mut scratch = [RegionSpan::EMPTY; MAX_RESERVED_MEMORY_RANGES + 1];
        let mut active_count = 1;
        active[0] = RegionSpan { start, end };

        for range in reserved {
            if range.length == 0 {
                continue;
            }

            let reserved_start = align_down(range.start);
            let reserved_end = align_up(range.end());
            if reserved_end <= reserved_start {
                continue;
            }

            let mut scratch_count = 0;
            for span in &active[..active_count] {
                if span.is_empty() {
                    continue;
                }

                let overlap_start = span.start.max(reserved_start);
                let overlap_end = span.end.min(reserved_end);

                if overlap_end <= overlap_start {
                    push_span(&mut scratch, &mut scratch_count, *span);
                    continue;
                }

                if span.start < overlap_start {
                    push_span(
                        &mut scratch,
                        &mut scratch_count,
                        RegionSpan {
                            start: span.start,
                            end: overlap_start,
                        },
                    );
                }

                if overlap_end < span.end {
                    push_span(
                        &mut scratch,
                        &mut scratch_count,
                        RegionSpan {
                            start: overlap_end,
                            end: span.end,
                        },
                    );
                }
            }

            active = scratch;
            active_count = scratch_count;
            scratch = [RegionSpan::EMPTY; MAX_RESERVED_MEMORY_RANGES + 1];
            if active_count == 0 {
                break;
            }
        }

        for span in &active[..active_count] {
            self.push_region(*span, kind);
        }
    }

    fn push_region(&mut self, span: RegionSpan, kind: MemoryRegionKind) {
        let start = align_up(span.start);
        let end = align_down(span.end);
        if end <= start || self.usable_region_count >= MAX_ALLOCATOR_REGIONS {
            return;
        }

        let page_count = (end - start) / PAGE_SIZE;
        if page_count == 0 {
            return;
        }

        self.regions[self.usable_region_count] = RegionCursor {
            start,
            page_count,
            next_page: 0,
            kind,
        };
        self.usable_region_count += 1;
        self.total_usable_pages = self.total_usable_pages.saturating_add(page_count);
    }
}

struct GlobalAllocatorCell(UnsafeCell<PhysicalPageAllocator>);

unsafe impl Sync for GlobalAllocatorCell {}

static PAGE_ALLOCATOR: GlobalAllocatorCell =
    GlobalAllocatorCell(UnsafeCell::new(PhysicalPageAllocator::new()));

pub fn init(boot_info: &BootInfo) {
    unsafe {
        (*PAGE_ALLOCATOR.0.get()).init(boot_info);
    }
}

pub fn allocate_page() -> Option<u64> {
    unsafe { (*PAGE_ALLOCATOR.0.get()).allocate_page() }
}

pub fn stats() -> MemoryStats {
    unsafe { (*PAGE_ALLOCATOR.0.get()).stats() }
}

pub fn first_usable_kind() -> MemoryRegionKind {
    unsafe { (*PAGE_ALLOCATOR.0.get()).first_usable_kind() }
}

fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> u64 {
    value.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn push_span(spans: &mut [RegionSpan], count: &mut usize, span: RegionSpan) {
    if span.is_empty() || *count >= spans.len() {
        return;
    }

    spans[*count] = span;
    *count += 1;
}
