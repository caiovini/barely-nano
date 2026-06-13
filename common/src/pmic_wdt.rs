//! SPM8821 PMIC Watchdog Driver
//!
//! The SpacemiT K1 has a second hardware watchdog inside the SPM8821 PMIC,
//! connected on PWR I2C bus 8 (MMIO base 0xd401d800, PMIC I2C addr 0x41).
//!
//! U-Boot starts it with a 60-second timeout ("WDT: Started PMIC_WDT with
//! servicing (60s timeout)").  If our bare-metal kernel does not service or
//! disable it, it triggers "sys: 0x200" reboot independently of the SoC WDT.
//!
//! Register map extracted from u-boot-orangepi/drivers/i2c/ky_i2c.c and
//! u-boot-orangepi/include/power/ky/spm8821.h.

// ── K1 PWR I2C (bus 8) MMIO layout ──────────────────────────────────────────
// struct ky_i2c { u32 icr; u32 isr; u32 isar; u32 idbr; u32 ilcr; u32 iwcr; u32 irst_cyc; u32 ibmr; }
const I2C8_BASE: usize = 0xd401d800;

// Register offsets (u32 words → byte offsets)
const ICR:  usize = 0x00; // Control
const ISR:  usize = 0x04; // Status
const ISAR: usize = 0x08; // Slave address (our address, set 0)
const IDBR: usize = 0x0C; // Data buffer

// ICR bits
const ICR_START:  u32 = 1 << 0;
const ICR_STOP:   u32 = 1 << 1;
const ICR_ACKNAK: u32 = 1 << 2; // 1 = send NAK
const ICR_TB:     u32 = 1 << 3; // transfer byte
const ICR_UR:     u32 = 1 << 10; // unit reset
const ICR_SCLE:   u32 = 1 << 13; // master clock enable
const ICR_IUE:    u32 = 1 << 14; // unit enable
const ICR_GCD:    u32 = 1 << 21; // general call disable
const ICR_ITEIE:  u32 = 1 << 19; // TX interrupt enable
const ICR_IRFIE:  u32 = 1 << 20; // RX interrupt enable
const ICR_BEIE:   u32 = 1 << 22; // bus error interrupt enable
const ICR_ALDIE:  u32 = 1 << 18; // arbitration loss int enable
const ICR_SM:     u32 = 0;       // standard mode (100 kHz)

const I2C_ICR_INIT: u32 = ICR_BEIE | ICR_IRFIE | ICR_ITEIE | ICR_GCD | ICR_SCLE;
const I2C_ISR_INIT: u32 = 0x1FDE000; // clear all status bits

// ISR bits
const ISR_IBB:    u32 = 1 << 16; // bus busy
const ISR_ITE:    u32 = 1 << 19; // TX buffer empty
const ISR_IRF:    u32 = 1 << 20; // RX buffer full
const ISR_ACKNAK: u32 = 1 << 14; // ack/nak status

// ── APBC clock gate for I2C bus 8 ────────────────────────────────────────────
// REG_APBC_APBC_TWSI8_CLK_RST = 0xd4015000 + 0x20
const APBC_BASE: usize       = 0xd4015000;
const APBC_TWSI8_OFFSET: usize = 0x20;
// Functional clock = 33 MHz (I2C_FUNCLK_33MHz = 0) → (0 << 4) | 0x3 = 0x3
const APBC_CLK_EN_RST_DONE: u32 = (0 << 4) | 0x3;

// ── SPM8821 PMIC I2C address and registers ────────────────────────────────────
const PMIC_ADDR: u8 = 0x41;          // 7-bit I2C address (from DTS: reg = <0x41>)
const SPM8821_WDT_CTRL: u8 = 0x44;   // WDT control register
const SPM8821_WDT_ENABLE: u8 = 0x08; // bit 3 = WDT enable

// Timeout for busy-wait loops (iterations).  At ~10 µs per poll this gives
// a ~10 ms window before we declare the bus stuck and bail.
const I2C_TIMEOUT: u32 = 1000;

// ── Raw register helpers ──────────────────────────────────────────────────────

#[inline(always)]
unsafe fn rd(reg: usize) -> u32 {
    ((I2C8_BASE + reg) as *const u32).read_volatile()
}

#[inline(always)]
unsafe fn wr(reg: usize, val: u32) {
    ((I2C8_BASE + reg) as *mut u32).write_volatile(val);
}

// ── Bus primitives ────────────────────────────────────────────────────────────

/// Reset and re-enable the I2C unit.
unsafe fn i2c_reset() {
    let mode = rd(ICR) & 0x300; // preserve speed bits

    wr(ICR, rd(ICR) & !ICR_IUE);  // disable unit
    wr(ICR, rd(ICR) | ICR_UR);    // assert reset
    // small delay (~100 µs equivalent via nop loops)
    for _ in 0..1000 { core::hint::spin_loop(); }
    wr(ICR, rd(ICR) & !ICR_IUE);  // keep disabled

    wr(ISAR, 0x00);               // our slave address = 0
    wr(ICR, I2C_ICR_INIT | ICR_SM | mode); // configure
    wr(ISR, I2C_ISR_INIT);        // clear all status
    wr(ICR, rd(ICR) | ICR_IUE);   // enable unit
    for _ in 0..100 { core::hint::spin_loop(); } // settle
}

/// Wait until `set_mask` bits are set AND `clr_mask` bits are clear.
/// Returns false on timeout.
unsafe fn wait_isr(set_mask: u32, clr_mask: u32) -> bool {
    let mut n = I2C_TIMEOUT;
    loop {
        let isr = rd(ISR);
        if (isr & set_mask) == set_mask && (isr & clr_mask) == 0 {
            return true;
        }
        for _ in 0..10 { core::hint::spin_loop(); } // ~10 µs
        n -= 1;
        if n == 0 { return false; }
    }
}

/// Write a single byte to the bus with the given condition flags.
/// Returns false on error.
unsafe fn i2c_write_byte(data: u8, cond_start: bool, cond_stop: bool, wait_ack: bool) -> bool {
    // Wait for bus idle
    if !wait_isr(0, ISR_IBB) { return false; }

    // Configure START / STOP conditions
    if cond_start {
        wr(ICR, rd(ICR) | ICR_START);
    } else {
        wr(ICR, rd(ICR) & !ICR_START);
    }
    if cond_stop {
        wr(ICR, rd(ICR) | ICR_STOP);
    } else {
        wr(ICR, rd(ICR) & !ICR_STOP);
    }

    // Always send ACK
    wr(ICR, rd(ICR) & !ICR_ACKNAK);

    // Load data and disable arbitration interrupt, then trigger transfer
    wr(IDBR, data as u32);
    wr(ICR, rd(ICR) & !ICR_ALDIE);
    wr(ICR, rd(ICR) | ICR_TB);

    // Wait for TX empty
    if !wait_isr(ISR_ITE, 0) { return false; }
    wr(ISR, rd(ISR) | ISR_ITE); // clear

    // Optionally wait for ACK
    if wait_ack {
        if !wait_isr(0, ISR_ACKNAK) { return false; }
    }

    true
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Disable the SPM8821 PMIC watchdog via PWR I2C bus 8.
///
/// Sequence:
///   1. Enable APB clock gate for I2C8.
///   2. Reset and configure the I2C controller.
///   3. Read SPM8821_WDT_CTRL (0x44).
///   4. Clear bit 3 (SPM8821_WDT_ENABLE) and write back.
///
/// Returns true on success, false if any I2C transaction timed out.
pub fn disable() -> bool {
    unsafe {
        // 1. Enable APB clock for I2C8: write reset-done value
        let clk_reg = (APBC_BASE + APBC_TWSI8_OFFSET) as *mut u32;
        // sequence: set FNCLK | RST (0x4), then also set enable (0x7), then release RST (0x3)
        clk_reg.write_volatile((0 << 4) | 0x4);
        clk_reg.write_volatile((0 << 4) | 0x7);
        clk_reg.write_volatile(APBC_CLK_EN_RST_DONE);
        for _ in 0..500 { core::hint::spin_loop(); } // let clock settle

        // 2. Reset and initialise the controller
        i2c_reset();

        // 3. Read WDT_CTRL: write PMIC address (write mode) → send reg address
        //    START + PMIC_ADDR<<1 (W-bit=0)
        if !i2c_write_byte(PMIC_ADDR << 1, true, false, true) {
            i2c_reset();
            return false;
        }
        //    Send register address byte
        if !i2c_write_byte(SPM8821_WDT_CTRL, false, false, true) {
            i2c_reset();
            return false;
        }

        // Repeated START + PMIC_ADDR<<1|1 (R-bit=1) for read
        if !i2c_write_byte((PMIC_ADDR << 1) | 1, true, false, true) {
            i2c_reset();
            return false;
        }

        // Trigger receive with NAK + STOP (single byte read)
        if !wait_isr(0, ISR_IBB) { i2c_reset(); return false; }
        wr(ICR, rd(ICR) & !ICR_START);
        wr(ICR, rd(ICR) | ICR_STOP);
        wr(ICR, rd(ICR) | ICR_ACKNAK); // send NAK (last byte)
        wr(ICR, rd(ICR) & !ICR_ALDIE);
        wr(ICR, rd(ICR) | ICR_TB);

        if !wait_isr(ISR_IRF, 0) { i2c_reset(); return false; }
        let current = rd(IDBR) as u8;
        wr(ISR, rd(ISR) | ISR_IRF); // clear RX-full flag

        i2c_reset();

        // 4. Write back with WDT_ENABLE bit cleared
        let new_val = current & !SPM8821_WDT_ENABLE;

        // START + PMIC_ADDR<<1 (write)
        if !i2c_write_byte(PMIC_ADDR << 1, true, false, true) {
            i2c_reset();
            return false;
        }
        // register address
        if !i2c_write_byte(SPM8821_WDT_CTRL, false, false, true) {
            i2c_reset();
            return false;
        }
        // data byte + STOP
        if !i2c_write_byte(new_val, false, true, true) {
            i2c_reset();
            return false;
        }

        i2c_reset();
        true
    }
}

/// Kick (reset the countdown of) the SPM8821 PMIC watchdog.
///
/// Writes SPM8821_WDT_CLEAR_STATUS (bit 0) to WDT_CTRL, then restores the
/// current value.  Safe to call periodically from the heartbeat loop.
pub fn kick() -> bool {
    unsafe {
        i2c_reset();

        // ── Read current WDT_CTRL value ──────────────────────────────────────
        if !i2c_write_byte(PMIC_ADDR << 1, true, false, true) { i2c_reset(); return false; }
        if !i2c_write_byte(SPM8821_WDT_CTRL, false, false, true) { i2c_reset(); return false; }
        if !i2c_write_byte((PMIC_ADDR << 1) | 1, true, false, true) { i2c_reset(); return false; }

        if !wait_isr(0, ISR_IBB) { i2c_reset(); return false; }
        wr(ICR, rd(ICR) & !ICR_START);
        wr(ICR, rd(ICR) | ICR_STOP);
        wr(ICR, rd(ICR) | ICR_ACKNAK);
        wr(ICR, rd(ICR) & !ICR_ALDIE);
        wr(ICR, rd(ICR) | ICR_TB);

        if !wait_isr(ISR_IRF, 0) { i2c_reset(); return false; }
        let current = rd(IDBR) as u8;
        wr(ISR, rd(ISR) | ISR_IRF);
        i2c_reset();

        // ── Write with clear-status bit set, then cleared ────────────────────
        let kick_val = current | 0x01; // SPM8821_WDT_CLEAR_STATUS = 0x1

        if !i2c_write_byte(PMIC_ADDR << 1, true, false, true) { i2c_reset(); return false; }
        if !i2c_write_byte(SPM8821_WDT_CTRL, false, false, true) { i2c_reset(); return false; }
        if !i2c_write_byte(kick_val, false, true, true) { i2c_reset(); return false; }
        i2c_reset();

        true
    }
}
