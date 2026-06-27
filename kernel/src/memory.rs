use alloc::vec::Vec;
use bootinfo::{
    BootInfo, MAX_MEMORY_REGIONS, MAX_RESERVED_MEMORY_RANGES, MemoryRegionKind, PAGE_SIZE,
    ReservedMemoryRange,
};
use core::cell::UnsafeCell;

const MAX_ALLOCATOR_REGIONS: usize = MAX_MEMORY_REGIONS + MAX_RESERVED_MEMORY_RANGES + 8;
const MAX_FREE_EXTENTS: usize = 1024;

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

#[derive(Debug, Clone, Copy)]
struct FreeExtent {
    start: u64,
    page_count: u64,
}

impl FreeExtent {
    const EMPTY: Self = Self {
        start: 0,
        page_count: 0,
    };

    const fn end(self) -> u64 {
        self.start
            .saturating_add(self.page_count.saturating_mul(PAGE_SIZE))
    }
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
    free_extents: [FreeExtent; MAX_FREE_EXTENTS],
    free_extent_count: usize,
}

impl PhysicalPageAllocator {
    const fn new() -> Self {
        Self {
            initialized: false,
            usable_region_count: 0,
            regions: [RegionCursor::EMPTY; MAX_ALLOCATOR_REGIONS],
            total_usable_pages: 0,
            allocated_pages: 0,
            free_extents: [FreeExtent::EMPTY; MAX_FREE_EXTENTS],
            free_extent_count: 0,
        }
    }

    fn init(&mut self, boot_info: &BootInfo) {
        self.initialized = true;
        self.usable_region_count = 0;
        self.regions = [RegionCursor::EMPTY; MAX_ALLOCATOR_REGIONS];
        self.total_usable_pages = 0;
        self.allocated_pages = 0;
        self.free_extents = [FreeExtent::EMPTY; MAX_FREE_EXTENTS];
        self.free_extent_count = 0;

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
        self.allocate_contiguous_pages(1)
    }

    fn allocate_contiguous_pages(&mut self, page_count: u64) -> Option<u64> {
        if page_count == 0 {
            return None;
        }
        if !self.initialized {
            return None;
        }

        if let Some((index, extent)) = self.free_extents[..self.free_extent_count]
            .iter()
            .copied()
            .enumerate()
            .find(|(_, extent)| extent.page_count >= page_count)
        {
            let address = extent.start;
            self.free_extents[index].start = self.free_extents[index]
                .start
                .saturating_add(page_count.saturating_mul(PAGE_SIZE));
            self.free_extents[index].page_count -= page_count;
            if self.free_extents[index].page_count == 0 {
                self.remove_free_extent(index);
            }
            self.allocated_pages = self.allocated_pages.saturating_add(page_count);
            return Some(address);
        }

        for region in &mut self.regions[..self.usable_region_count] {
            if region.remaining_pages() < page_count {
                continue;
            }

            let address = region
                .start
                .saturating_add(region.next_page.saturating_mul(PAGE_SIZE));
            region.next_page = region.next_page.saturating_add(page_count);
            self.allocated_pages = self.allocated_pages.saturating_add(page_count);
            return Some(address);
        }

        None
    }

    fn deallocate_contiguous_pages(&mut self, start: u64, page_count: u64) -> bool {
        if !self.initialized
            || page_count == 0
            || !start.is_multiple_of(PAGE_SIZE)
            || self.allocated_pages < page_count
        {
            return false;
        }
        let Some(length) = page_count.checked_mul(PAGE_SIZE) else {
            return false;
        };
        let Some(end) = start.checked_add(length) else {
            return false;
        };
        let was_allocated = self.regions[..self.usable_region_count]
            .iter()
            .any(|region| {
                let allocated_end = region
                    .start
                    .saturating_add(region.next_page.saturating_mul(PAGE_SIZE));
                start >= region.start && end <= allocated_end
            });
        if !was_allocated
            || self.free_extents[..self.free_extent_count]
                .iter()
                .any(|extent| start < extent.end() && extent.start < end)
        {
            return false;
        }

        let insertion = self.free_extents[..self.free_extent_count]
            .iter()
            .position(|extent| extent.start > start)
            .unwrap_or(self.free_extent_count);
        if insertion > 0 && self.free_extents[insertion - 1].end() == start {
            self.free_extents[insertion - 1].page_count = self.free_extents[insertion - 1]
                .page_count
                .saturating_add(page_count);
            if insertion < self.free_extent_count
                && self.free_extents[insertion - 1].end() == self.free_extents[insertion].start
            {
                self.free_extents[insertion - 1].page_count = self.free_extents[insertion - 1]
                    .page_count
                    .saturating_add(self.free_extents[insertion].page_count);
                self.remove_free_extent(insertion);
            }
            self.allocated_pages -= page_count;
            return true;
        }
        if insertion < self.free_extent_count && end == self.free_extents[insertion].start {
            self.free_extents[insertion].start = start;
            self.free_extents[insertion].page_count = self.free_extents[insertion]
                .page_count
                .saturating_add(page_count);
            self.allocated_pages -= page_count;
            return true;
        }
        if self.free_extent_count >= MAX_FREE_EXTENTS {
            return false;
        }
        for index in (insertion..self.free_extent_count).rev() {
            self.free_extents[index + 1] = self.free_extents[index];
        }
        self.free_extents[insertion] = FreeExtent { start, page_count };
        self.free_extent_count += 1;
        self.allocated_pages -= page_count;
        self.coalesce_free_extents();
        true
    }

    fn remove_free_extent(&mut self, index: usize) {
        for current in index..self.free_extent_count.saturating_sub(1) {
            self.free_extents[current] = self.free_extents[current + 1];
        }
        self.free_extent_count = self.free_extent_count.saturating_sub(1);
        self.free_extents[self.free_extent_count] = FreeExtent::EMPTY;
    }

    fn coalesce_free_extents(&mut self) {
        let mut index = 0;
        while index + 1 < self.free_extent_count {
            let current = self.free_extents[index];
            let next = self.free_extents[index + 1];
            if current.end() == next.start {
                self.free_extents[index].page_count =
                    current.page_count.saturating_add(next.page_count);
                self.remove_free_extent(index + 1);
            } else {
                index += 1;
            }
        }
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

pub fn allocate_contiguous_pages(page_count: u64) -> Option<u64> {
    unsafe { (*PAGE_ALLOCATOR.0.get()).allocate_contiguous_pages(page_count) }
}

pub fn deallocate_page(start: u64) -> bool {
    deallocate_contiguous_pages(start, 1)
}

pub fn deallocate_contiguous_pages(start: u64, page_count: u64) -> bool {
    unsafe { (*PAGE_ALLOCATOR.0.get()).deallocate_contiguous_pages(start, page_count) }
}

pub fn stats() -> MemoryStats {
    unsafe { (*PAGE_ALLOCATOR.0.get()).stats() }
}

pub fn first_usable_kind() -> MemoryRegionKind {
    unsafe { (*PAGE_ALLOCATOR.0.get()).first_usable_kind() }
}

pub fn allocated_ranges() -> Vec<(u64, u64)> {
    unsafe {
        let allocator = &*PAGE_ALLOCATOR.0.get();
        allocator.regions[..allocator.usable_region_count]
            .iter()
            .filter(|region| region.next_page != 0)
            .map(|region| (region.start, region.next_page.saturating_mul(PAGE_SIZE)))
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_allocator(page_count: u64) -> PhysicalPageAllocator {
        let mut allocator = PhysicalPageAllocator::new();
        allocator.initialized = true;
        allocator.usable_region_count = 1;
        allocator.regions[0] = RegionCursor {
            start: 0x10_0000,
            page_count,
            next_page: 0,
            kind: MemoryRegionKind::Conventional,
        };
        allocator.total_usable_pages = page_count;
        allocator
    }

    #[test]
    fn reclaimed_extents_are_coalesced_and_reused() {
        let mut allocator = synthetic_allocator(16);
        let first = allocator.allocate_contiguous_pages(4).unwrap();
        let second = allocator.allocate_contiguous_pages(3).unwrap();
        assert_eq!(allocator.stats().allocated_pages, 7);

        assert!(allocator.deallocate_contiguous_pages(first, 4));
        assert!(allocator.deallocate_contiguous_pages(second, 3));
        assert!(!allocator.deallocate_contiguous_pages(first, 1));
        assert_eq!(allocator.free_extent_count, 1);
        assert_eq!(allocator.free_extents[0].page_count, 7);
        assert_eq!(allocator.stats().allocated_pages, 0);

        assert_eq!(allocator.allocate_contiguous_pages(7), Some(first));
        assert_eq!(allocator.stats().allocated_pages, 7);
        assert_eq!(allocator.free_extent_count, 0);
    }
}
