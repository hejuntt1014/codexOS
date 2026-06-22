const BOOTSTRAP_HEAP_SIZE: usize = 256 * 1024;

pub type KernelHeap = heap_allocator::ExpandableHeap<BOOTSTRAP_HEAP_SIZE>;
