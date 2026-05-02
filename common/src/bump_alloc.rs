use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

pub struct BumpAllocator {
    heap_start: AtomicUsize,
    heap_end: AtomicUsize,
    next: AtomicUsize,
}

impl BumpAllocator {
    pub const fn empty() -> Self {
        Self {
            heap_start: AtomicUsize::new(0),
            heap_end: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }

    pub unsafe fn init(&self, start: usize, len: usize) {
        self.heap_start.store(start, Ordering::SeqCst);
        self.heap_end.store(start + len, Ordering::SeqCst);
        self.next.store(start, Ordering::SeqCst);
    }

    pub fn used(&self) -> usize {
        self.next.load(Ordering::Relaxed) - self.heap_start.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> usize {
        self.heap_end.load(Ordering::Relaxed) - self.heap_start.load(Ordering::Relaxed)
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        let size = layout.size();

        loop {
            let current_next = self.next.load(Ordering::SeqCst);
            let alloc_start = (current_next + align - 1) & !(align - 1);
            let alloc_end = alloc_start + size;

            if alloc_end > self.heap_end.load(Ordering::SeqCst) {
                return core::ptr::null_mut();
            }

            if self.next.compare_exchange(
                current_next,
                alloc_end,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ).is_ok() {
                return alloc_start as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never deallocates
    }
}
