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
#[cfg(target_arch = "riscv64")]
static WDT_STATUS: AtomicU8 = AtomicU8::new(0); // 0 = unprobed, 1 = accessible, 2 = unmapped

#[cfg(target_arch = "riscv64")]
#[inline]
fn wdt_accessible() -> bool {
    match WDT_STATUS.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let ok = crate::trap::probe_read_u32(WDT_BASE + WDT_WMER).is_some();
            WDT_STATUS.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
            ok
        }
    }
}

/// Records that Hart 6 (Ledger Coordinator) is alive and making progress.
#[inline]
pub fn heartbeat_hart6(mtime: u64) {
    HART6_LAST_HEARTBEAT_MTIME.store(mtime, Ordering::Relaxed);
}

/// "Kicks" (pings) the hardware watchdog to reset its counter if mapped.
pub fn kick() {
    #[cfg(target_arch = "riscv64")]
    {
        if !wdt_accessible() {
            return;
        }
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
