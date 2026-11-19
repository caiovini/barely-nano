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
static HART0_LAST_HEARTBEAT_MTIME: AtomicU64 = AtomicU64::new(0);
static FATAL_STALL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Records that Hart 6 (Ledger+Boot) is alive and making progress.
#[inline]
pub fn heartbeat_hart6(mtime: u64) {
    HART6_LAST_HEARTBEAT_MTIME.store(mtime, Ordering::Relaxed);
}

/// Records that Hart 0 (VoteProc) is alive and making progress.
#[inline]
pub fn heartbeat_hart0(mtime: u64) {
    HART0_LAST_HEARTBEAT_MTIME.store(mtime, Ordering::Relaxed);
}

/// Marks an unrecoverable trap or panic on any hart so `kick()` stops feeding the hardware watchdog.
#[inline]
pub fn mark_fatal_stall() {
    FATAL_STALL.store(true, Ordering::SeqCst);
}

/// "Kicks" (pings) the hardware watchdog to reset its counter.
/// This must be called at least once every 60 seconds (default timeout).
/// Once Hart 6 / Hart 0 have started, if either stalls for > 60 seconds (or a fatal trap/panic
/// occurs), we stop kicking the hardware watchdog so the SoC automatically resets and recovers.
pub fn kick() {
    #[cfg(target_arch = "riscv64")]
    {
        if FATAL_STALL.load(Ordering::Relaxed) {
            return;
        }
        let now = crate::clint::mtime();
        let last_h6 = HART6_LAST_HEARTBEAT_MTIME.load(Ordering::Relaxed);
        if last_h6 != 0 && now.saturating_sub(last_h6) > 60 * crate::clint::FREQ {
            return;
        }
        let last_h0 = HART0_LAST_HEARTBEAT_MTIME.load(Ordering::Relaxed);
        if last_h0 != 0 && now.saturating_sub(last_h0) > 60 * crate::clint::FREQ {
            return;
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

/// Re-enables the SpacemiT K1 hardware watchdog in system-reset mode (`WE | WRIE = 0x0003`)
/// after single-threaded boot initialization completes.
pub fn enable() {
    #[cfg(target_arch = "riscv64")]
    {
        let now = crate::clint::mtime();
        HART6_LAST_HEARTBEAT_MTIME.store(now, Ordering::Relaxed);
        HART0_LAST_HEARTBEAT_MTIME.store(now, Ordering::Relaxed);
        unsafe {
            let base = WDT_BASE as *mut u32;

            // 1. Clear status register (0xC0)
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);
            base.add(0xc0 / 4).write_volatile(0x0000);

            // 2. Reset counter register (0xC8)
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);
            base.add(WDT_WCR / 4).write_volatile(0x0001);

            // 3. Enable counter + reset generation on timeout (`WE | WRIE = 0x0003`)
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);
            base.add(WDT_WMER / 4).write_volatile(0x0003);
        }
    }
}
