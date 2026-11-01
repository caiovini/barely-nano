use core::sync::atomic::{AtomicU64, Ordering};

// CLINT timer frequency (configured in `crate::soc::CLINT_FREQ`, 24 MHz on SpacemiT K1/K3)
pub const FREQ: u64 = crate::soc::CLINT_FREQ;

static TIME_OFFSET_NS: AtomicU64 = AtomicU64::new(0);
static TIME_OFFSET_CYCLES: AtomicU64 = AtomicU64::new(0);

/// Read the current time from the `time` CSR virtualised by OpenSBI.
#[inline(always)]
pub fn mtime_raw() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let t: u64;
        unsafe { core::arch::asm!("csrr {}, time", out(reg) t, options(nomem, nostack, preserves_flags)) };
        t
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        0
    }
}

#[inline(always)]
pub fn mtime() -> u64 {
    mtime_raw().wrapping_add(TIME_OFFSET_CYCLES.load(Ordering::Relaxed))
}

pub fn set_time_offset_ns(offset: u64) {
    let offset_cycles = (offset as u128 * FREQ as u128 / 1_000_000_000) as u64;
    TIME_OFFSET_NS.store(offset, Ordering::SeqCst);
    TIME_OFFSET_CYCLES.store(offset_cycles, Ordering::SeqCst);
}

pub fn get_time_offset_ns() -> u64 {
    TIME_OFFSET_NS.load(Ordering::SeqCst)
}
