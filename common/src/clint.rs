// QEMU virt CLINT memory map
pub const CLINT_BASE: usize = 0x0200_0000;
const _MTIME_ADDR: usize = CLINT_BASE + 0x0000_BFF8;
#[cfg(feature = "mmode")]
const MTIMECMP_ADDR: usize = CLINT_BASE + 0x0000_4000; // hart 0

// CLINT timer frequency on QEMU virt machine: 10 MHz
pub const FREQ: u64 = 10_000_000;

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
    mtime_raw() + (TIME_OFFSET_NS.load(Ordering::Relaxed) / 100)
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
