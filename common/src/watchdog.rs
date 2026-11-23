//! SpacemiT K3 Watchdog Driver
//!
//! Probes `WDT_BASE` trap-safely after `trap::init()` so unmapped or disabled
//! watchdog MMIO on SpacemiT K3 never causes an unhandled load/store fault.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

#[cfg(target_arch = "riscv64")]
const WDT_BASE: usize = crate::soc::WDT_BASE;

// Register offsets
#[cfg(target_arch = "riscv64")]
const WDT_WFAR: usize = 0xb0; // Write Fault Address Register (Unlock 1)
#[cfg(target_arch = "riscv64")]
const WDT_WSAR: usize = 0xb4; // Write Service Address Register (Unlock 2)
#[cfg(target_arch = "riscv64")]
const WDT_WCR: usize = 0xc8; // Watchdog Control Register (Kick)
#[cfg(target_arch = "riscv64")]
const WDT_WMER: usize = 0xb8; // Watchdog Mode Enable Register

static HART6_LAST_HEARTBEAT_MTIME: AtomicU64 = AtomicU64::new(0);
static HART0_LAST_HEARTBEAT_MTIME: AtomicU64 = AtomicU64::new(0);
static FATAL_STALL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
#[cfg(target_arch = "riscv64")]
static WDT_STATUS: AtomicU8 = AtomicU8::new(2); // 2 = APB clock-gated on SpacemiT K3 (P1 PMIC WDT handled via I2C8)

#[cfg(target_arch = "riscv64")]
#[inline]
fn wdt_accessible() -> bool {
    match WDT_STATUS.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            // Do not probe unclocked APB watchdog slaves: on SpacemiT K3, `watchdog@d4014000`
            // has its APB clock/reset gated in U-Boot, and reading an unclocked APB slave
            // stalls PREADY indefinitely without raising a CPU exception.
            WDT_STATUS.store(2, Ordering::Relaxed);
            false
        }
    }
}

/// Records that Hart 6 (Ledger Coordinator) is alive and making progress.
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

/// "Kicks" (pings) the hardware watchdog to reset its counter if mapped.
pub fn kick() {
    #[cfg(target_arch = "riscv64")]
    {
        if !wdt_accessible() || FATAL_STALL.load(Ordering::Relaxed) {
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

/// Disables the watchdog if mapped.
pub fn disable() {
    #[cfg(target_arch = "riscv64")]
    {
        if !wdt_accessible() {
            return;
        }
        unsafe {
            let base = WDT_BASE as *mut u32;

            // 1. Unlock sequence
            base.add(WDT_WFAR / 4).write_volatile(0xbaba);
            base.add(WDT_WSAR / 4).write_volatile(0xeb10);

            // 2. Disable mode
            base.add(WDT_WMER / 4).write_volatile(0x0000);
        }
    }
}

/// Re-enables the SpacemiT hardware watchdog in system-reset mode (`WE | WRIE = 0x0003`)
/// after single-threaded boot initialization completes, if mapped.
pub fn enable() {
    #[cfg(target_arch = "riscv64")]
    {
        let now = crate::clint::mtime();
        HART6_LAST_HEARTBEAT_MTIME.store(now, Ordering::Relaxed);
        HART0_LAST_HEARTBEAT_MTIME.store(now, Ordering::Relaxed);
        if !wdt_accessible() {
            return;
        }
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
