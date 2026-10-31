use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// A lock-free, cache-line separated Single-Producer Single-Consumer (SPSC) tunnel.
///
/// Ensures producer and consumer control variables are aligned to separate 64-byte
/// cache lines to prevent cross-cluster cache line bouncing on dual-cluster RISC-V SoCs (e.g., SpacemiT K1).
#[repr(C, align(64))]
pub struct SpscTunnel<T, const CAP: usize> {
    // Written only by Producer (Hart A)
    head: AtomicUsize,
    _pad1: [u8; 56],

    // Written only by Consumer (Hart B)
    tail: AtomicUsize,
    _pad2: [u8; 56],

    // Pre-allocated ring buffer slots
    ring: [UnsafeCell<Option<T>>; CAP],
}

unsafe impl<T: Send, const CAP: usize> Sync for SpscTunnel<T, CAP> {}
unsafe impl<T: Send, const CAP: usize> Send for SpscTunnel<T, CAP> {}

impl<T, const CAP: usize> SpscTunnel<T, CAP> {
    pub const fn new() -> Self {
        assert!(CAP > 0 && (CAP & (CAP - 1)) == 0, "CAP must be a power of two");
        Self {
            head: AtomicUsize::new(0),
            _pad1: [0; 56],
            tail: AtomicUsize::new(0),
            _pad2: [0; 56],
            ring: [const { UnsafeCell::new(None) }; CAP],
        }
    }

    #[inline]
    pub fn push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= CAP {
            return Err(item); // Buffer full
        }
        let slot = &self.ring[head & (CAP - 1)];
        unsafe {
            *slot.get() = Some(item);
        }
        core::sync::atomic::fence(Ordering::Release);
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    #[inline]
    pub fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None; // Buffer empty
        }
        core::sync::atomic::fence(Ordering::Acquire);
        let slot = &self.ring[tail & (CAP - 1)];
        let item = unsafe { (*slot.get()).take() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        item
    }

    #[inline]
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        head.wrapping_sub(tail)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= CAP
    }
}
