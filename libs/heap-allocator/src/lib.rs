#![no_std]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::cmp;
use core::hint::spin_loop;
use core::mem::MaybeUninit;
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const FREE_LIST_END: usize = usize::MAX;
const ALLOCATION_MAGIC: u64 = 0x4344_5848_4541_5031;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    pub capacity_bytes: usize,
    pub used_bytes: usize,
    pub peak_used_bytes: usize,
    pub free_bytes: usize,
    pub largest_free_block: usize,
    pub live_allocations: usize,
    pub total_allocations: usize,
    pub total_deallocations: usize,
    pub failed_allocations: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FreeBlock {
    size: usize,
    next: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AllocationHeader {
    magic: u64,
    block_start: usize,
    block_size: usize,
    requested_size: usize,
}

struct AllocatorState {
    initialized: bool,
    free_head: usize,
    used_bytes: usize,
    peak_used_bytes: usize,
    live_allocations: usize,
    total_allocations: usize,
    total_deallocations: usize,
    failed_allocations: usize,
}

impl AllocatorState {
    const fn new() -> Self {
        Self {
            initialized: false,
            free_head: FREE_LIST_END,
            used_bytes: 0,
            peak_used_bytes: 0,
            live_allocations: 0,
            total_allocations: 0,
            total_deallocations: 0,
            failed_allocations: 0,
        }
    }
}

#[repr(align(64))]
struct HeapStorage<const N: usize>(UnsafeCell<MaybeUninit<[u8; N]>>);

unsafe impl<const N: usize> Sync for HeapStorage<N> {}

struct StateLock {
    locked: AtomicBool,
    state: UnsafeCell<AllocatorState>,
}

unsafe impl Sync for StateLock {}

impl StateLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            state: UnsafeCell::new(AllocatorState::new()),
        }
    }

    fn acquire(&self) -> StateGuard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
        StateGuard { lock: self }
    }
}

struct StateGuard<'a> {
    lock: &'a StateLock,
}

impl StateGuard<'_> {
    fn state(&mut self) -> &mut AllocatorState {
        unsafe { &mut *self.lock.state.get() }
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

pub struct ReclaimingHeap<const N: usize> {
    storage: HeapStorage<N>,
    state: StateLock,
}

unsafe impl<const N: usize> Sync for ReclaimingHeap<N> {}

impl<const N: usize> ReclaimingHeap<N> {
    pub const fn new() -> Self {
        assert!(N >= core::mem::size_of::<FreeBlock>());
        assert!(N.is_multiple_of(core::mem::align_of::<FreeBlock>()));
        Self {
            storage: HeapStorage(UnsafeCell::new(MaybeUninit::uninit())),
            state: StateLock::new(),
        }
    }

    pub fn stats(&self) -> HeapStats {
        let mut guard = self.state.acquire();
        let base = self.base_ptr();
        let state = guard.state();
        initialize(state, base, N);

        let mut free_bytes = 0usize;
        let mut largest_free_block = 0usize;
        let mut current = state.free_head;
        while current != FREE_LIST_END {
            let block = unsafe { read_free_block(base, current) };
            free_bytes = free_bytes.saturating_add(block.size);
            largest_free_block = cmp::max(largest_free_block, block.size);
            current = block.next;
        }

        HeapStats {
            capacity_bytes: N,
            used_bytes: state.used_bytes,
            peak_used_bytes: state.peak_used_bytes,
            free_bytes,
            largest_free_block,
            live_allocations: state.live_allocations,
            total_allocations: state.total_allocations,
            total_deallocations: state.total_deallocations,
            failed_allocations: state.failed_allocations,
        }
    }

    fn base_ptr(&self) -> *mut u8 {
        self.storage.0.get().cast::<u8>()
    }
}

impl<const N: usize> Default for ReclaimingHeap<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapInitError {
    AlreadyInitialized,
    InvalidAddress,
    InvalidCapacity,
    OverlapsBootstrap,
}

pub struct ExpandableHeap<const N: usize> {
    bootstrap: ReclaimingHeap<N>,
    external_base: AtomicUsize,
    external_capacity: AtomicUsize,
    external_state: StateLock,
}

unsafe impl<const N: usize> Sync for ExpandableHeap<N> {}

impl<const N: usize> ExpandableHeap<N> {
    pub const fn new() -> Self {
        Self {
            bootstrap: ReclaimingHeap::new(),
            external_base: AtomicUsize::new(0),
            external_capacity: AtomicUsize::new(0),
            external_state: StateLock::new(),
        }
    }

    pub fn initialize_external(&self, base: *mut u8, capacity: usize) -> Result<(), HeapInitError> {
        let address = base as usize;
        if address == 0 || !address.is_multiple_of(core::mem::align_of::<FreeBlock>()) {
            return Err(HeapInitError::InvalidAddress);
        }
        if capacity < core::mem::size_of::<FreeBlock>()
            || !capacity.is_multiple_of(core::mem::align_of::<FreeBlock>())
            || address.checked_add(capacity).is_none()
        {
            return Err(HeapInitError::InvalidCapacity);
        }
        let bootstrap_start = self.bootstrap.base_ptr() as usize;
        let bootstrap_end = bootstrap_start.saturating_add(N);
        let external_end = address.saturating_add(capacity);
        if address < bootstrap_end && bootstrap_start < external_end {
            return Err(HeapInitError::OverlapsBootstrap);
        }

        let mut guard = self.external_state.acquire();
        if self.external_base.load(Ordering::Relaxed) != 0 {
            return Err(HeapInitError::AlreadyInitialized);
        }
        initialize(guard.state(), base, capacity);
        self.external_capacity.store(capacity, Ordering::Relaxed);
        self.external_base.store(address, Ordering::Release);
        Ok(())
    }

    pub fn using_external(&self) -> bool {
        self.external_base.load(Ordering::Acquire) != 0
    }

    pub fn stats(&self) -> HeapStats {
        let mut total = self.bootstrap.stats();
        let base = self.external_base.load(Ordering::Acquire);
        if base == 0 {
            return total;
        }

        let capacity = self.external_capacity.load(Ordering::Relaxed);
        let mut guard = self.external_state.acquire();
        let state = guard.state();
        let external = region_stats(state, base as *mut u8, capacity);
        total.capacity_bytes = total.capacity_bytes.saturating_add(external.capacity_bytes);
        total.used_bytes = total.used_bytes.saturating_add(external.used_bytes);
        total.peak_used_bytes = total
            .peak_used_bytes
            .saturating_add(external.peak_used_bytes);
        total.free_bytes = total.free_bytes.saturating_add(external.free_bytes);
        total.largest_free_block = cmp::max(total.largest_free_block, external.largest_free_block);
        total.live_allocations = total
            .live_allocations
            .saturating_add(external.live_allocations);
        total.total_allocations = total
            .total_allocations
            .saturating_add(external.total_allocations);
        total.total_deallocations = total
            .total_deallocations
            .saturating_add(external.total_deallocations);
        total.failed_allocations = total
            .failed_allocations
            .saturating_add(external.failed_allocations);
        total
    }
}

impl<const N: usize> Default for ExpandableHeap<N> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<const N: usize> GlobalAlloc for ExpandableHeap<N> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.external_base.load(Ordering::Acquire);
        if base != 0 {
            let mut guard = self.external_state.acquire();
            let state = guard.state();
            if let Some(pointer) = allocate_from_free_list(state, base as *mut u8, layout) {
                return pointer;
            }
            state.failed_allocations = state.failed_allocations.saturating_add(1);
        }
        unsafe { self.bootstrap.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if pointer.is_null() {
            return;
        }
        let base = self.external_base.load(Ordering::Acquire);
        let capacity = self.external_capacity.load(Ordering::Relaxed);
        let address = pointer as usize;
        if base != 0 && address >= base && address < base.saturating_add(capacity) {
            let mut guard = self.external_state.acquire();
            unsafe {
                deallocate_into_region(guard.state(), base as *mut u8, capacity, pointer);
            }
        } else {
            unsafe { self.bootstrap.dealloc(pointer, layout) };
        }
    }
}

unsafe impl<const N: usize> GlobalAlloc for ReclaimingHeap<N> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = self.state.acquire();
        let base = self.base_ptr();
        let state = guard.state();
        initialize(state, base, N);

        match allocate_from_free_list(state, base, layout) {
            Some(pointer) => pointer,
            None => {
                state.failed_allocations = state.failed_allocations.saturating_add(1);
                null_mut()
            }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, _layout: Layout) {
        if pointer.is_null() {
            return;
        }

        let mut guard = self.state.acquire();
        let base = self.base_ptr();
        let state = guard.state();
        unsafe { deallocate_into_region(state, base, N, pointer) };
    }
}

fn initialize(state: &mut AllocatorState, base: *mut u8, capacity: usize) {
    if state.initialized {
        return;
    }
    unsafe {
        write_free_block(
            base,
            0,
            FreeBlock {
                size: capacity,
                next: FREE_LIST_END,
            },
        );
    }
    state.free_head = 0;
    state.initialized = true;
}

fn region_stats(state: &AllocatorState, base: *mut u8, capacity: usize) -> HeapStats {
    let mut free_bytes = 0usize;
    let mut largest_free_block = 0usize;
    let mut current = state.free_head;
    while current != FREE_LIST_END {
        let block = unsafe { read_free_block(base, current) };
        free_bytes = free_bytes.saturating_add(block.size);
        largest_free_block = cmp::max(largest_free_block, block.size);
        current = block.next;
    }
    HeapStats {
        capacity_bytes: capacity,
        used_bytes: state.used_bytes,
        peak_used_bytes: state.peak_used_bytes,
        free_bytes,
        largest_free_block,
        live_allocations: state.live_allocations,
        total_allocations: state.total_allocations,
        total_deallocations: state.total_deallocations,
        failed_allocations: state.failed_allocations,
    }
}

fn allocate_from_free_list(
    state: &mut AllocatorState,
    base: *mut u8,
    layout: Layout,
) -> Option<*mut u8> {
    let header_size = core::mem::size_of::<AllocationHeader>();
    let header_align = core::mem::align_of::<AllocationHeader>();
    let free_align = core::mem::align_of::<FreeBlock>();
    let requested_size = cmp::max(layout.size(), 1);
    let alignment = cmp::max(layout.align(), header_align);
    let mut previous = FREE_LIST_END;
    let mut current = state.free_head;

    while current != FREE_LIST_END {
        let block = unsafe { read_free_block(base, current) };
        let block_end = current.checked_add(block.size)?;
        let user_start = align_up(current.checked_add(header_size)?, alignment)?;
        let allocation_end = match user_start.checked_add(requested_size) {
            Some(end) if end <= block_end => end,
            _ => {
                previous = current;
                current = block.next;
                continue;
            }
        };

        let aligned_suffix_start = align_up(allocation_end, free_align)?;
        let (allocated_end, replacement) = if aligned_suffix_start <= block_end
            && block_end - aligned_suffix_start >= core::mem::size_of::<FreeBlock>()
        {
            unsafe {
                write_free_block(
                    base,
                    aligned_suffix_start,
                    FreeBlock {
                        size: block_end - aligned_suffix_start,
                        next: block.next,
                    },
                );
            }
            (aligned_suffix_start, aligned_suffix_start)
        } else {
            (block_end, block.next)
        };

        if previous == FREE_LIST_END {
            state.free_head = replacement;
        } else {
            unsafe {
                (*base.add(previous).cast::<FreeBlock>()).next = replacement;
            }
        }

        let block_size = allocated_end - current;
        unsafe {
            ptr::write(
                base.add(user_start - header_size)
                    .cast::<AllocationHeader>(),
                AllocationHeader {
                    magic: ALLOCATION_MAGIC,
                    block_start: current,
                    block_size,
                    requested_size,
                },
            );
        }
        state.used_bytes = state.used_bytes.saturating_add(block_size);
        state.peak_used_bytes = cmp::max(state.peak_used_bytes, state.used_bytes);
        state.live_allocations = state.live_allocations.saturating_add(1);
        state.total_allocations = state.total_allocations.saturating_add(1);
        return Some(unsafe { base.add(user_start) });
    }
    None
}

unsafe fn deallocate_into_region(
    state: &mut AllocatorState,
    base: *mut u8,
    capacity: usize,
    pointer: *mut u8,
) {
    if !state.initialized {
        return;
    }

    let base_address = base as usize;
    let pointer_address = pointer as usize;
    let heap_end = match base_address.checked_add(capacity) {
        Some(end) => end,
        None => return,
    };
    let header_size = core::mem::size_of::<AllocationHeader>();
    if pointer_address < base_address.saturating_add(header_size) || pointer_address > heap_end {
        return;
    }

    let header_pointer = unsafe { pointer.sub(header_size).cast::<AllocationHeader>() };
    let header = unsafe { ptr::read(header_pointer) };
    let valid_end = header.block_start.checked_add(header.block_size);
    if header.magic != ALLOCATION_MAGIC
        || header.block_start >= capacity
        || header.block_size == 0
        || valid_end.is_none_or(|end| end > capacity)
    {
        return;
    }

    unsafe {
        ptr::write(
            header_pointer,
            AllocationHeader {
                magic: 0,
                block_start: 0,
                block_size: 0,
                requested_size: 0,
            },
        );
    }
    insert_free_block(state, base, header.block_start, header.block_size);
    state.used_bytes = state.used_bytes.saturating_sub(header.block_size);
    state.live_allocations = state.live_allocations.saturating_sub(1);
    state.total_deallocations = state.total_deallocations.saturating_add(1);
}

fn insert_free_block(
    state: &mut AllocatorState,
    base: *mut u8,
    block_start: usize,
    block_size: usize,
) {
    let mut previous = FREE_LIST_END;
    let mut current = state.free_head;
    while current != FREE_LIST_END && current < block_start {
        previous = current;
        current = unsafe { read_free_block(base, current) }.next;
    }

    unsafe {
        write_free_block(
            base,
            block_start,
            FreeBlock {
                size: block_size,
                next: current,
            },
        );
    }
    if previous == FREE_LIST_END {
        state.free_head = block_start;
    } else {
        unsafe {
            (*base.add(previous).cast::<FreeBlock>()).next = block_start;
        }
    }

    if current != FREE_LIST_END && block_start.saturating_add(block_size) == current {
        let next = unsafe { read_free_block(base, current) };
        unsafe {
            write_free_block(
                base,
                block_start,
                FreeBlock {
                    size: block_size.saturating_add(next.size),
                    next: next.next,
                },
            );
        }
    }

    if previous != FREE_LIST_END {
        let previous_block = unsafe { read_free_block(base, previous) };
        if previous.saturating_add(previous_block.size) == block_start {
            let inserted = unsafe { read_free_block(base, block_start) };
            unsafe {
                write_free_block(
                    base,
                    previous,
                    FreeBlock {
                        size: previous_block.size.saturating_add(inserted.size),
                        next: inserted.next,
                    },
                );
            }
        }
    }
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|aligned| aligned & !(alignment - 1))
}

unsafe fn read_free_block(base: *mut u8, offset: usize) -> FreeBlock {
    unsafe { ptr::read(base.add(offset).cast::<FreeBlock>()) }
}

unsafe fn write_free_block(base: *mut u8, offset: usize, block: FreeBlock) {
    unsafe { ptr::write(base.add(offset).cast::<FreeBlock>(), block) }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HEAP_SIZE: usize = 16 * 1024;

    #[test]
    fn reuses_and_coalesces_released_blocks() {
        let heap = ReclaimingHeap::<TEST_HEAP_SIZE>::new();
        let first_layout = Layout::from_size_align(1024, 16).unwrap();
        let second_layout = Layout::from_size_align(2048, 64).unwrap();

        unsafe {
            let first = heap.alloc(first_layout);
            let second = heap.alloc(second_layout);
            assert!(!first.is_null());
            assert!(!second.is_null());
            assert_eq!((second as usize) % 64, 0);
            ptr::write_bytes(first, 0x5a, 1024);
            ptr::write_bytes(second, 0xa5, 2048);
            heap.dealloc(first, first_layout);
            heap.dealloc(second, second_layout);
        }

        let stats = heap.stats();
        assert_eq!(stats.used_bytes, 0);
        assert_eq!(stats.live_allocations, 0);
        assert_eq!(stats.free_bytes, TEST_HEAP_SIZE);
        assert_eq!(stats.largest_free_block, TEST_HEAP_SIZE);
        assert_eq!(stats.total_allocations, 2);
        assert_eq!(stats.total_deallocations, 2);
    }

    #[test]
    fn preserves_live_allocations_while_reusing_a_gap() {
        let heap = ReclaimingHeap::<TEST_HEAP_SIZE>::new();
        let layout = Layout::from_size_align(512, 32).unwrap();

        unsafe {
            let first = heap.alloc(layout);
            let middle = heap.alloc(layout);
            let last = heap.alloc(layout);
            assert!(!first.is_null() && !middle.is_null() && !last.is_null());
            ptr::write_bytes(first, 0x11, 512);
            ptr::write_bytes(last, 0x33, 512);
            heap.dealloc(middle, layout);

            let replacement = heap.alloc(layout);
            assert_eq!(replacement, middle);
            assert!((0..512).all(|index| *first.add(index) == 0x11));
            assert!((0..512).all(|index| *last.add(index) == 0x33));
            heap.dealloc(replacement, layout);
            heap.dealloc(first, layout);
            heap.dealloc(last, layout);
        }
        assert_eq!(heap.stats().free_bytes, TEST_HEAP_SIZE);
    }

    #[test]
    fn reports_exhaustion_without_corrupting_state() {
        let heap = ReclaimingHeap::<1024>::new();
        let too_large = Layout::from_size_align(2048, 8).unwrap();
        let pointer = unsafe { heap.alloc(too_large) };
        assert!(pointer.is_null());

        let stats = heap.stats();
        assert_eq!(stats.failed_allocations, 1);
        assert_eq!(stats.used_bytes, 0);
        assert_eq!(stats.free_bytes, 1024);
    }

    #[test]
    fn switches_to_external_storage_and_routes_deallocation() {
        #[repr(align(64))]
        struct ExternalStorage([u8; 8192]);

        let heap = ExpandableHeap::<1024>::new();
        let mut external = ExternalStorage([0; 8192]);
        heap.initialize_external(external.0.as_mut_ptr(), external.0.len())
            .unwrap();
        assert!(heap.using_external());

        let layout = Layout::from_size_align(2048, 64).unwrap();
        unsafe {
            let pointer = heap.alloc(layout);
            assert!(!pointer.is_null());
            assert!(pointer >= external.0.as_mut_ptr());
            assert!(pointer < external.0.as_mut_ptr().add(external.0.len()));
            ptr::write_bytes(pointer, 0x7c, layout.size());
            heap.dealloc(pointer, layout);
        }

        let stats = heap.stats();
        assert_eq!(stats.capacity_bytes, 9216);
        assert_eq!(stats.used_bytes, 0);
        assert_eq!(stats.live_allocations, 0);
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.total_deallocations, 1);
    }
}
