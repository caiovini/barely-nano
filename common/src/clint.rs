// QEMU virt CLINT memory map
pub const CLINT_BASE: usize = 0x0200_0000;
const _MTIME_ADDR: usize = CLINT_BASE + 0x0000_BFF8;
#[cfg(feature = "mmode")]
const MTIMECMP_ADDR: usize = CLINT_BASE + 0x0000_4000; // hart 0

// CLINT timer frequency on Orange Pi RV2 (SpacemiT K1): 24 MHz
pub const FREQ: u64 = 24_000_000;

/// Read the current time.
/// - M-mode: reads directly from CLINT MMIO (fast).
/// - S-mode: reads the `time` CSR virtualised by OpenSBI (CLINT MMIO is PMP-blocked).
#[cfg(feature = "mmode")]
pub fn mtime_raw() -> u64 {
    unsafe { (_MTIME_ADDR as *const u64).read_volatile() }
}

#[cfg(all(feature = "smode", not(feature = "mmode")))]
pub fn mtime_raw() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("csrr {}, time", out(reg) t, options(nostack)) };
    t
}

#[cfg(all(not(feature = "mmode"), not(feature = "smode")))]
pub fn mtime_raw() -> u64 {
    0 // Fallback
}

pub fn mtime() -> u64 {
    let offset_ns = TIME_OFFSET_NS.load(Ordering::Relaxed);
    // Use u128 to avoid overflow when offset_ns is large (e.g. Unix epoch nanoseconds)
    // (1.7e18 ns * 24e6 Hz) >> 64
    let offset_cycles = (offset_ns as u128 * FREQ as u128 / 1_000_000_000) as u64;
    mtime_raw().wrapping_add(offset_cycles)
}

/// Write mtimecmp for hart 0. M-mode only — S-mode must use SBI set_timer.
#[cfg(feature = "mmode")]
pub fn set_mtimecmp(val: u64) {
    unsafe { (MTIMECMP_ADDR as *mut u64).write_volatile(val) };
}

use core::sync::atomic::{AtomicU64, Ordering};

static TIME_OFFSET_NS: AtomicU64 = AtomicU64::new(0);

pub fn set_time_offset_ns(offset: u64) {
    TIME_OFFSET_NS.store(offset, Ordering::SeqCst);
}

pub fn get_time_offset_ns() -> u64 {
    TIME_OFFSET_NS.load(Ordering::SeqCst)
}
