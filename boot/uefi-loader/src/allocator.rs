const BOOTSTRAP_HEAP_SIZE: usize = 512 * 1024;

pub type LoaderHeap = heap_allocator::ExpandableHeap<BOOTSTRAP_HEAP_SIZE>;
