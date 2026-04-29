// QEMU virt CLINT memory map
pub const CLINT_BASE: usize = 0x0200_0000;
const MTIME_ADDR: usize = CLINT_BASE + 0x0000_BFF8;
#[cfg(feature = "mmode")]
const MTIMECMP_ADDR: usize = CLINT_BASE + 0x0000_4000; // hart 0

// CLINT timer frequency on QEMU virt machine: 10 MHz
pub const FREQ: u64 = 10_000_000;

/// Read the current time.
/// - M-mode: reads directly from CLINT MMIO (fast).
/// - S-mode: reads the `time` CSR virtualised by OpenSBI (CLINT MMIO is PMP-blocked).
#[cfg(feature = "mmode")]
pub fn mtime() -> u64 {
    unsafe { (MTIME_ADDR as *const u64).read_volatile() }
}

#[cfg(feature = "smode")]
pub fn mtime() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("csrr {}, time", out(reg) t, options(nostack)) };
    t
}

/// Write mtimecmp for hart 0. M-mode only — S-mode must use SBI set_timer.
#[cfg(feature = "mmode")]
pub fn set_mtimecmp(val: u64) {
    unsafe { (MTIMECMP_ADDR as *mut u64).write_volatile(val) };
}
