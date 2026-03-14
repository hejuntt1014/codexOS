use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 16 * 1024 * 1024;

#[repr(align(16))]
struct Heap([u8; HEAP_SIZE]);

static HEAP: Heap = Heap([0; HEAP_SIZE]);
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align_mask = layout.align().saturating_sub(1);
        let mut current = NEXT.load(Ordering::Relaxed);

        loop {
            let start = (current + align_mask) & !align_mask;
            let end = match start.checked_add(layout.size()) {
                Some(end) => end,
                None => return null_mut(),
            };

            if end > HEAP_SIZE {
                return null_mut();
            }

            match NEXT.compare_exchange(current, end, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => {
                    let base = HEAP.0.as_ptr() as usize;
                    return (base + start) as *mut u8;
                }
                Err(updated) => current = updated,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
