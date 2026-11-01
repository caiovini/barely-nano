//! SpacemiT K1 Watchdog Driver
//! 
//! The K1 SoC has a hardware watchdog that must be "kicked" periodically
//! to prevent the system from resetting. The reset is identified by "sys: 0x200"
//! in the console output.

use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_arch = "riscv64")]
const WDT_BASE: usize = crate::soc::WDT_BASE;

// Register offsets
#[cfg(target_arch = "riscv64")]
const WDT_WFAR: usize = 0xb0; // Write Fault Address Register (Unlock 1)
#[cfg(target_arch = "riscv64")]
const WDT_WSAR: usize = 0xb4; // Write Service Address Register (Unlock 2)
#[cfg(target_arch = "riscv64")]
const WDT_WCR:  usize = 0xc8; // Watchdog Control Register (Kick)
#[cfg(target_arch = "riscv64")]
const WDT_WMER: usize = 0xb8; // Watchdog Mode Enable Register

static HART6_LAST_HEARTBEAT_MTIME: AtomicU64 = AtomicU64::new(0);

/// Records that Hart 6 (Ledger+Boot) is alive and making progress.
#[inline]
pub fn heartbeat_hart6(mtime: u64) {
    HART6_LAST_HEARTBEAT_MTIME.store(mtime, Ordering::Relaxed);
}

/// "Kicks" (pings) the hardware watchdog to reset its counter.
/// This must be called at least once every 60 seconds (default timeout).
/// Once Hart 6 has started, if Hart 6 stalls for > 60 seconds, we stop kicking
/// the hardware watchdog so the SoC automatically resets and recovers.
pub fn kick() {
    #[cfg(target_arch = "riscv64")]
    {
        let last_h6 = HART6_LAST_HEARTBEAT_MTIME.load(Ordering::Relaxed);
        if last_h6 != 0 {
            let now = crate::clint::mtime();
            if now.saturating_sub(last_h6) > 60 * crate::clint::FREQ {
                return;
            }
        }
        unsafe {
            let base = WDT_BASE as *mut u32;
            
            // 1. Clear watchdog timer status register (0xC0)
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);
            base.add(0xc0 / 4).write_volatile(0x0000);
            
            // 2. Reset/kick watchdog timer counter register (0xC8)
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);
            base.add(WDT_WCR / 4).write_volatile(0x0001);
        }
    }
}

/// Disables the watchdog entirely.
/// Use with caution, but helpful during long initialization.
pub fn disable() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        let base = WDT_BASE as *mut u32;
        
        // 1. Unlock sequence
        base.add(WDT_WFAR / 4).write_volatile(0xbaba);
        base.add(WDT_WSAR / 4).write_volatile(0xeb10);
        
        // 2. Disable mode
        base.add(WDT_WMER / 4).write_volatile(0x0000);
    }
}
