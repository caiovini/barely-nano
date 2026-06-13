//! SpacemiT K1 Watchdog Driver
//! 
//! The K1 SoC has a hardware watchdog that must be "kicked" periodically
//! to prevent the system from resetting. The reset is identified by "sys: 0x200"
//! in the console output.

const WDT_BASE: usize = 0xd4080000;

// Register offsets
const WDT_WFAR: usize = 0xb0; // Write Fault Address Register (Unlock 1)
const WDT_WSAR: usize = 0xb4; // Write Service Address Register (Unlock 2)
const WDT_WCR:  usize = 0xc8; // Watchdog Control Register (Kick)
const WDT_WMER: usize = 0xb8; // Watchdog Mode Enable Register

/// "Kicks" (pings) the hardware watchdog to reset its counter.
/// This must be called at least once every 60 seconds (default timeout).
pub fn kick() {
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

/// Disables the watchdog entirely.
/// Use with caution, but helpful during long initialization.
pub fn disable() {
    unsafe {
        let base = WDT_BASE as *mut u32;
        
        // 1. Unlock sequence
        base.add(WDT_WFAR / 4).write_volatile(0xbaba);
        base.add(WDT_WSAR / 4).write_volatile(0xeb10);
        
        // 2. Disable mode
        base.add(WDT_WMER / 4).write_volatile(0x0000);
    }
}
