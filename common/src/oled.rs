//! Bare-metal no_std I2C OLED status display driver (SSD1306 / SH1106 128x64).
//!
//! Supports both hardware `I2C4` (`0xd401_2800`) and hardware-timer-paced GPIO I2C
//! across the Orange Pi RV2 26-pin header (including auto-detection if `SCK` and
//! `SDA` are swapped on Pins 3 & 5 or connected to Pins 8 & 10).

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

// ── SpacemiT K1 / Ky X1 MMIO bases ───────────────────────────────────────────
const I2C4_BASE: usize = 0xd401_2800;

const APBC_BASE: usize = 0xd401_5000;
const APBC_GPIO_CLK_RST: usize = 0x08;
const APBC_AIB_CLK_RST: usize = 0x3c;
const APBC_TWSI4_CLK_RST: usize = 0x40;

const PINCTRL_BASE: usize = 0xd401_e000;
const PINCTRL_MASK: u32 = 0xffff;
// MUX_MODE4 (4) | SPU_EN (1<<3) | EDGE_NONE (1<<6) | PAD_3V_DS4 (1<<10) | PULL_UP (6<<13)
const PINCTRL_I2C_MUX4: u32 = 0xc44c;
// MUX_MODE0 (0, GPIO) | SPU_EN (1<<3) | EDGE_NONE (1<<6) | PAD_3V_DS4 (1<<10) | PULL_UP (6<<13)
const PINCTRL_GPIO_MUX0: u32 = 0xc448;

const GPIO_BASE: usize = 0xd401_9000;
const GPLR_OFFSET: usize = 0x00; // Pin Level Register
const GPSR_OFFSET: usize = 0x18; // Pin Output Set Register
const GPCR_OFFSET: usize = 0x24; // Pin Output Clear Register
const GSDR_OFFSET: usize = 0x54; // Bitwise Set Direction (Output)
const GCDR_OFFSET: usize = 0x60; // Bitwise Clear Direction (Input)

// ── Ky I2C controller registers & bits ───────────────────────────────────────
const ICR: usize = 0x00;
const ISR: usize = 0x04;
const ISAR: usize = 0x08;
const IDBR: usize = 0x0C;
const IBMR: usize = 0x1C;

const ICR_START: u32 = 1 << 0;
const ICR_STOP: u32 = 1 << 1;
const ICR_ACKNAK: u32 = 1 << 2;
const ICR_TB: u32 = 1 << 3;
const ICR_SM: u32 = 0; // Standard Mode (100 kHz)
const ICR_UR: u32 = 1 << 10;
const ICR_SCLE: u32 = 1 << 13;
const ICR_IUE: u32 = 1 << 14;
const ICR_ALDIE: u32 = 1 << 18;
const ICR_ITEIE: u32 = 1 << 19;
const ICR_IRFIE: u32 = 1 << 20;
const ICR_GCD: u32 = 1 << 21;
const ICR_BEIE: u32 = 1 << 22;

const I2C_ICR_INIT: u32 = ICR_BEIE | ICR_IRFIE | ICR_ITEIE | ICR_GCD | ICR_SCLE | ICR_SM;
const I2C_ISR_INIT: u32 = 0x01FD_E000;

const ISR_ACKNAK: u32 = 1 << 14;
const ISR_IBB: u32 = 1 << 16;
const ISR_ITE: u32 = 1 << 19;

const I2C_TIMEOUT: u32 = 1000;

// ── Display state ────────────────────────────────────────────────────────────
// Mode: 0 = disabled, 1 = hardware I2C, 2 = GPIO bit-bang I2C
static OLED_MODE: AtomicU8 = AtomicU8::new(0);
static OLED_I2C_BASE: AtomicUsize = AtomicUsize::new(I2C4_BASE);
static OLED_I2C_ADDR: AtomicU8 = AtomicU8::new(0x3C);
static OLED_SCL_GPIO: AtomicU8 = AtomicU8::new(51);
static OLED_SDA_GPIO: AtomicU8 = AtomicU8::new(52);
static OLED_HEARTBEAT: AtomicBool = AtomicBool::new(false);

const LINE_CHARS: usize = 15;
const NUM_LINES: usize = 4;

static mut PREV_LINES: [[u8; LINE_CHARS]; NUM_LINES] = [[0; LINE_CHARS]; NUM_LINES];

#[inline(always)]
fn delay_us(us: u64) {
    let start = crate::clint::mtime();
    let ticks = us * (crate::clint::FREQ / 1_000_000);
    while crate::clint::mtime().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

#[inline(always)]
unsafe fn rd(base: usize, reg: usize) -> u32 {
    ((base + reg) as *const u32).read_volatile()
}

#[inline(always)]
unsafe fn wr(base: usize, reg: usize, val: u32) {
    ((base + reg) as *mut u32).write_volatile(val);
}

unsafe fn configure_pad_gpio(gpio: u8, val: u32) {
    // PINID(x) = x + 1; register offset = PINID(x) * 4
    let offset = ((gpio as usize) + 1) * 4;
    let reg = (PINCTRL_BASE + offset) as *mut u32;
    let cur = reg.read_volatile();
    reg.write_volatile((cur & !PINCTRL_MASK) | (val & PINCTRL_MASK));
}

// ── GPIO Bit-Bang I2C primitives ─────────────────────────────────────────────

#[inline(always)]
fn gpio_bank_base(gpio: u8) -> usize {
    const BANK_OFFSET: [usize; 4] = [0x00, 0x04, 0x08, 0x100];
    GPIO_BASE + BANK_OFFSET[((gpio >> 5) as usize) & 3]
}

#[inline(always)]
fn gpio_bit(gpio: u8) -> u32 {
    1u32 << (gpio & 0x1f)
}

#[inline(always)]
unsafe fn gpio_drive_low(gpio: u8) {
    let base = gpio_bank_base(gpio);
    let bit = gpio_bit(gpio);
    ((base + GPCR_OFFSET) as *mut u32).write_volatile(bit);
    ((base + GSDR_OFFSET) as *mut u32).write_volatile(bit);
}

#[inline(always)]
unsafe fn gpio_drive_high(gpio: u8) {
    let base = gpio_bank_base(gpio);
    let bit = gpio_bit(gpio);
    ((base + GPSR_OFFSET) as *mut u32).write_volatile(bit);
    ((base + GSDR_OFFSET) as *mut u32).write_volatile(bit);
}

#[inline(always)]
unsafe fn gpio_release_input(gpio: u8) {
    let base = gpio_bank_base(gpio);
    let bit = gpio_bit(gpio);
    ((base + GCDR_OFFSET) as *mut u32).write_volatile(bit);
}

#[inline(always)]
unsafe fn gpio_read(gpio: u8) -> bool {
    let base = gpio_bank_base(gpio);
    let bit = gpio_bit(gpio);
    (((base + GPLR_OFFSET) as *const u32).read_volatile() & bit) != 0
}

unsafe fn bb_init_pins(scl: u8, sda: u8) {
    configure_pad_gpio(scl, PINCTRL_GPIO_MUX0);
    configure_pad_gpio(sda, PINCTRL_GPIO_MUX0);
    gpio_release_input(scl);
    gpio_release_input(sda);
    delay_us(10);
}

unsafe fn bb_start(scl: u8, sda: u8) {
    gpio_drive_high(sda);
    gpio_drive_high(scl);
    delay_us(4);
    gpio_drive_low(sda);
    delay_us(4);
    gpio_drive_low(scl);
    delay_us(4);
}

unsafe fn bb_stop(scl: u8, sda: u8) {
    gpio_drive_low(sda);
    delay_us(4);
    gpio_drive_high(scl);
    delay_us(4);
    gpio_drive_high(sda);
    delay_us(4);
    gpio_release_input(sda);
    gpio_release_input(scl);
}

/// Write 1 byte over GPIO I2C and return `true` if slave ACKed (pulled SDA low on 9th clock).
unsafe fn bb_write_byte(scl: u8, sda: u8, byte: u8) -> bool {
    for bit_idx in (0..8).rev() {
        if (byte & (1 << bit_idx)) != 0 {
            gpio_drive_high(sda);
        } else {
            gpio_drive_low(sda);
        }
        delay_us(3);
        gpio_drive_high(scl);
        delay_us(4);
        gpio_drive_low(scl);
        delay_us(2);
    }

    // 9th clock: release SDA to input (with internal + external pull-up) and read ACK
    gpio_release_input(sda);
    delay_us(5);
    gpio_drive_high(scl);
    delay_us(4);
    let ack = !gpio_read(sda); // ACK = SDA pulled low (0)
    gpio_drive_low(scl);
    delay_us(2);
    ack
}

unsafe fn bb_write_stream(scl: u8, sda: u8, addr: u8, ctrl: u8, payload: &[u8]) -> bool {
    bb_start(scl, sda);
    if !bb_write_byte(scl, sda, addr << 1) {
        bb_stop(scl, sda);
        return false;
    }
    if !bb_write_byte(scl, sda, ctrl) {
        bb_stop(scl, sda);
        return false;
    }
    for &b in payload {
        if !bb_write_byte(scl, sda, b) {
            bb_stop(scl, sda);
            return false;
        }
    }
    bb_stop(scl, sda);
    true
}

// ── Hardware I2C primitives ──────────────────────────────────────────────────

unsafe fn i2c_reset(base: usize) {
    wr(base, ICR, rd(base, ICR) & !ICR_IUE);
    wr(base, ICR, rd(base, ICR) | ICR_UR);
    delay_us(100);
    wr(base, ICR, rd(base, ICR) & !ICR_IUE);
    wr(base, ISAR, 0x00);
    wr(base, ICR, I2C_ICR_INIT);
    wr(base, ISR, I2C_ISR_INIT);
    wr(base, ICR, rd(base, ICR) | ICR_IUE);
    delay_us(10);
}

unsafe fn wait_isr(base: usize, set_mask: u32, clr_mask: u32) -> bool {
    let mut n = I2C_TIMEOUT;
    loop {
        let isr = rd(base, ISR);
        if (isr & set_mask) == set_mask && (isr & clr_mask) == 0 {
            return true;
        }
        delay_us(10);
        n -= 1;
        if n == 0 {
            return false;
        }
    }
}

unsafe fn hw_i2c_write_byte(base: usize, data: u8, cond_start: bool, cond_stop: bool) -> bool {
    if !wait_isr(base, 0, ISR_IBB) {
        return false;
    }

    wr(base, ICR, rd(base, ICR) & !ICR_START);
    wr(base, ICR, rd(base, ICR) & !ICR_STOP);
    wr(base, IDBR, data as u32);
    if cond_start {
        wr(base, ICR, rd(base, ICR) | ICR_START);
    }
    if cond_stop {
        wr(base, ICR, rd(base, ICR) | ICR_STOP);
    }
    wr(base, ICR, rd(base, ICR) & !ICR_ACKNAK);
    wr(base, ICR, rd(base, ICR) & !ICR_ALDIE);
    wr(base, ICR, rd(base, ICR) | ICR_TB);

    if !wait_isr(base, ISR_ITE, 0) {
        return false;
    }
    wr(base, ISR, rd(base, ISR) | ISR_ITE);

    if !wait_isr(base, 0, ISR_ACKNAK) {
        return false;
    }
    true
}

unsafe fn hw_write_stream(base: usize, addr: u8, ctrl: u8, payload: &[u8]) -> bool {
    if payload.is_empty() {
        return true;
    }
    i2c_reset(base);

    // Byte 0: START + Slave Address (Write) — never combine START and STOP on the same byte
    if !hw_i2c_write_byte(base, addr << 1, true, false) {
        i2c_reset(base);
        return false;
    }

    // Byte 1: Control byte (0x00 = commands, 0x40 = display RAM data)
    if !hw_i2c_write_byte(base, ctrl, false, false) {
        i2c_reset(base);
        return false;
    }

    for (i, &b) in payload.iter().enumerate() {
        let is_last = i + 1 == payload.len();
        if !hw_i2c_write_byte(base, b, false, is_last) {
            i2c_reset(base);
            return false;
        }
    }

    i2c_reset(base);
    true
}

unsafe fn oled_write_stream(ctrl: u8, payload: &[u8]) -> bool {
    let mode = OLED_MODE.load(Ordering::Relaxed);
    let addr = OLED_I2C_ADDR.load(Ordering::Relaxed);
    match mode {
        1 => {
            let base = OLED_I2C_BASE.load(Ordering::Relaxed);
            hw_write_stream(base, addr, ctrl, payload)
        }
        2 => {
            let scl = OLED_SCL_GPIO.load(Ordering::Relaxed);
            let sda = OLED_SDA_GPIO.load(Ordering::Relaxed);
            bb_write_stream(scl, sda, addr, ctrl, payload)
        }
        _ => false,
    }
}

unsafe fn enable_apbc_clk(offset: usize) {
    let clk_reg = (APBC_BASE + offset) as *mut u32;
    clk_reg.write_volatile(0x4);
    clk_reg.write_volatile(0x7);
    clk_reg.write_volatile(0x3);
    delay_us(20);
}

/// Initialize I2C4 (or GPIO I2C fallback across candidate header pins), probe for an
/// SSD1306/SH1106 OLED at 0x3C/0x3D, initialize the display, and render the boot screen.
pub fn init() -> bool {
    unsafe {
        // Ensure parent clock gates, AIB (pinctrl), and GPIO clocks are active
        let mpmu_acgr = 0xd405_1024 as *mut u32;
        mpmu_acgr.write_volatile(mpmu_acgr.read_volatile() | (1 << 6));
        let apb_spare2 = 0xd409_0104 as *mut u32;
        apb_spare2.write_volatile(apb_spare2.read_volatile() | (1 << 4));
        ((APBC_BASE + APBC_AIB_CLK_RST) as *mut u32).write_volatile(0x3);
        ((APBC_BASE + APBC_GPIO_CLK_RST) as *mut u32).write_volatile(0x3);

        // 1. Try Hardware I2C4 on Pin 5 (GPIO_51 = SCL) & Pin 3 (GPIO_52 = SDA)
        //    using a valid multi-byte write (START -> addr -> 0x00 -> 0xE3 NOP + STOP).
        configure_pad_gpio(51, PINCTRL_I2C_MUX4);
        configure_pad_gpio(52, PINCTRL_I2C_MUX4);
        enable_apbc_clk(APBC_TWSI4_CLK_RST);
        i2c_reset(I2C4_BASE);

        let ibmr = rd(I2C4_BASE, IBMR);
        crate::uart::print(b"[oled] I2C4 IBMR=0x");
        crate::uart::print_usize_hex(ibmr as usize);
        crate::uart::print(b"\r\n");

        let mut detected = false;
        for &addr in &[0x3Cu8, 0x3Du8] {
            if hw_write_stream(I2C4_BASE, addr, 0x00, &[0xE3]) {
                OLED_MODE.store(1, Ordering::SeqCst);
                OLED_I2C_BASE.store(I2C4_BASE, Ordering::SeqCst);
                OLED_I2C_ADDR.store(addr, Ordering::SeqCst);
                crate::uart::print(b"[oled] Detected display on HW I2C4 (Pin3=SDA, Pin5=SCL) at 0x");
                crate::uart::print_usize_hex(addr as usize);
                crate::uart::print(b"\r\n");
                detected = true;
                break;
            }
        }

        // 2. Fallback: scan candidate header pin pairs via Open-Drain GPIO I2C
        //    (handles swapped SCK/SDA on Pins 3/5, or connection on Pins 8/10 or 23/24)
        if !detected {
            const CANDIDATE_PAIRS: [(u8, u8, &[u8]); 6] = [
                (51, 52, b"Pin5=SCL(GPIO51), Pin3=SDA(GPIO52)"),
                (52, 51, b"Pin3=SCL(GPIO52), Pin5=SDA(GPIO51) [swapped]"),
                (47, 48, b"Pin8=SCL(GPIO47), Pin10=SDA(GPIO48)"),
                (48, 47, b"Pin10=SCL(GPIO48), Pin8=SDA(GPIO47) [swapped]"),
                (75, 76, b"Pin23=SCL(GPIO75), Pin24=SDA(GPIO76)"),
                (76, 75, b"Pin24=SCL(GPIO76), Pin23=SDA(GPIO75) [swapped]"),
            ];

            for &(scl, sda, label) in &CANDIDATE_PAIRS {
                bb_init_pins(scl, sda);
                for &addr in &[0x3Cu8, 0x3Du8] {
                    if bb_write_stream(scl, sda, addr, 0x00, &[0xE3]) {
                        OLED_MODE.store(2, Ordering::SeqCst);
                        OLED_SCL_GPIO.store(scl, Ordering::SeqCst);
                        OLED_SDA_GPIO.store(sda, Ordering::SeqCst);
                        OLED_I2C_ADDR.store(addr, Ordering::SeqCst);
                        crate::uart::print(b"[oled] Detected display via GPIO I2C on ");
                        crate::uart::print(label);
                        crate::uart::print(b" at 0x");
                        crate::uart::print_usize_hex(addr as usize);
                        crate::uart::print(b"\r\n");
                        detected = true;
                        break;
                    }
                }
                if detected {
                    break;
                }
            }
        }

        if !detected {
            bb_init_pins(51, 52);
            bb_init_pins(47, 48);
            crate::uart::print(b"[oled] No display ACKed. Pin levels: GPIO51(Pin5)=");
            crate::uart::print_usize(gpio_read(51) as usize);
            crate::uart::print(b" GPIO52(Pin3)=");
            crate::uart::print_usize(gpio_read(52) as usize);
            crate::uart::print(b" GPIO47(Pin8)=");
            crate::uart::print_usize(gpio_read(47) as usize);
            crate::uart::print(b" GPIO48(Pin10)=");
            crate::uart::print_usize(gpio_read(48) as usize);
            crate::uart::print(b"\r\n");
            return false;
        }

        // Wait 20ms in case display power rail just ramped up
        delay_us(20_000);

        // SSD1306 / SH1106 128x64 initialization sequence
        const INIT_CMDS: [u8; 26] = [
            0xAE,       // Display OFF
            0xD5, 0x80, // Clock divide ratio / osc freq
            0xA8, 0x3F, // Multiplex ratio = 63 (64 rows)
            0xD3, 0x00, // Display offset = 0
            0x40,       // Start line = 0
            0x8D, 0x14, // Enable internal charge pump (3.3V supply)
            0x20, 0x02, // Page Addressing Mode (compatible with SSD1306 & SH1106)
            0xA1,       // Segment remap (col 127 -> SEG0)
            0xC8,       // COM output scan direction remapped
            0xDA, 0x12, // COM pins hardware config (128x64)
            0x81, 0xCF, // Contrast = 0xCF
            0xD9, 0xF1, // Pre-charge period
            0xDB, 0x40, // VCOMH deselect level
            0xA4,       // Entire display ON from RAM
            0xA6,       // Normal display (non-inverted)
            0x2E,       // Deactivate scroll
            0xAF,       // Display ON
        ];

        if !oled_write_stream(0x00, &INIT_CMDS) {
            crate::uart::print(b"[oled] Init command stream failed\r\n");
            OLED_MODE.store(0, Ordering::SeqCst);
            return false;
        }

        // Clear all 8 pages
        let zero_page = [0u8; 132];
        for page in 0..8u8 {
            let page_cmds = [0xB0 | page, 0x00, 0x10];
            let _ = oled_write_stream(0x00, &page_cmds);
            let _ = oled_write_stream(0x40, &zero_page);
        }

        PREV_LINES = [[0; LINE_CHARS]; NUM_LINES];

        show_lines(&[
            b"BARELY NANO    ",
            b"RISC-V NO_STD  ",
            b"STATUS: BOOTING",
            b"INIT LEDGER... ",
        ]);
        crate::uart::print(b"[oled] Boot screen rendered!\r\n");
        true
    }
}

/// Render up to 4 lines (15 ASCII chars each) onto the 128x64 OLED.
/// Each line uses an 8x16 bold font (occupying 2 vertical pages per line).
pub fn show_lines(lines: &[&[u8]; NUM_LINES]) {
    if OLED_MODE.load(Ordering::Relaxed) == 0 {
        return;
    }

    unsafe {
        for row in 0..NUM_LINES {
            let mut line_buf = [b' '; LINE_CHARS];
            let src = lines[row];
            let copy_len = src.len().min(LINE_CHARS);
            line_buf[..copy_len].copy_from_slice(&src[..copy_len]);

            if PREV_LINES[row] == line_buf {
                continue;
            }
            PREV_LINES[row] = line_buf;

            let mut top_page = [0u8; 128];
            let mut bot_page = [0u8; 128];

            for (ch_idx, &ch) in line_buf.iter().enumerate() {
                let x_base = 4 + ch_idx * 8;
                let cols5 = glyph_5x7(ch);
                let is_center_stem = cols5[0] == 0 && cols5[4] == 0;
                let cols8: [u8; 8] = if is_center_stem {
                    [0, cols5[1], cols5[2], cols5[2], cols5[3], 0, 0, 0]
                } else {
                    [
                        cols5[0], cols5[0], cols5[1], cols5[2], cols5[3], cols5[4], cols5[4], 0,
                    ]
                };

                for dx in 0..8 {
                    let col7 = cols8[dx];
                    let mut col16: u16 = 0;
                    let mut b = 0;
                    while b < 7 {
                        if (col7 & (1 << b)) != 0 {
                            let shift = 1 + b * 2;
                            col16 |= 0b11 << shift;
                        }
                        b += 1;
                    }
                    top_page[x_base + dx] = (col16 & 0xFF) as u8;
                    bot_page[x_base + dx] = ((col16 >> 8) & 0xFF) as u8;
                }
            }

            let page_top_idx = (row * 2) as u8;
            let page_bot_idx = (row * 2 + 1) as u8;

            let cmds_top = [0xB0 | page_top_idx, 0x00, 0x10];
            if !oled_write_stream(0x00, &cmds_top) {
                return;
            }
            if !oled_write_stream(0x40, &top_page) {
                return;
            }

            let cmds_bot = [0xB0 | page_bot_idx, 0x00, 0x10];
            if !oled_write_stream(0x00, &cmds_bot) {
                return;
            }
            if !oled_write_stream(0x40, &bot_page) {
                return;
            }
        }
    }
}

fn fmt_usize(mut val: usize, out: &mut [u8]) -> usize {
    if val == 0 {
        if !out.is_empty() {
            out[0] = b'0';
            return 1;
        }
        return 0;
    }
    let mut tmp = [0u8; 20];
    let mut n = 0;
    while val > 0 && n < tmp.len() {
        tmp[n] = b'0' + (val % 10) as u8;
        val /= 10;
        n += 1;
    }
    let write_len = n.min(out.len());
    for i in 0..write_len {
        out[i] = tmp[n - 1 - i];
    }
    write_len
}

/// Update the OLED with concise node status metrics:
/// - Line 0: `NANO P:<peers>   *`
/// - Line 1: `BLK: <blocks>`
/// - Line 2: `CNF: <confirmed>`
/// - Line 3: `<bps>/S DB:<size>`
pub fn update_status(peers: usize, blocks: usize, confirmed: usize, bps: usize, db_kb: usize) {
    if OLED_MODE.load(Ordering::Relaxed) == 0 {
        return;
    }

    let hb = OLED_HEARTBEAT.fetch_xor(true, Ordering::Relaxed);

    let mut l0 = [b' '; LINE_CHARS];
    l0[0..7].copy_from_slice(b"NANO P:");
    let _ = fmt_usize(peers, &mut l0[7..13]);
    l0[14] = if hb { b'*' } else { b'.' };

    let mut l1 = [b' '; LINE_CHARS];
    l1[0..5].copy_from_slice(b"BLK: ");
    let _ = fmt_usize(blocks, &mut l1[5..LINE_CHARS]);

    let mut l2 = [b' '; LINE_CHARS];
    l2[0..5].copy_from_slice(b"CNF: ");
    let _ = fmt_usize(confirmed, &mut l2[5..LINE_CHARS]);

    let mut l3 = [b' '; LINE_CHARS];
    let n_bps = fmt_usize(bps, &mut l3[0..5]);
    if n_bps + 2 <= 7 {
        l3[n_bps] = b'/';
        l3[n_bps + 1] = b'S';
    }
    l3[7..10].copy_from_slice(b"DB:");
    if db_kb >= 1024 {
        let n_db = fmt_usize(db_kb / 1024, &mut l3[10..14]);
        if 10 + n_db < LINE_CHARS {
            l3[10 + n_db] = b'M';
        }
    } else {
        let n_db = fmt_usize(db_kb, &mut l3[10..14]);
        if 10 + n_db < LINE_CHARS {
            l3[10 + n_db] = b'K';
        }
    }

    show_lines(&[&l0, &l1, &l2, &l3]);
}

fn glyph_5x7(ch: u8) -> [u8; 5] {
    let c = if ch >= b'a' && ch <= b'z' {
        ch - 32
    } else {
        ch
    };
    match c {
        b'0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        b'1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        b'2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        b'3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        b'4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        b'5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        b'6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        b'7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        b'8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        b'9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        b'A' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        b'B' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        b'C' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        b'D' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        b'E' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        b'F' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        b'G' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        b'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        b'I' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        b'J' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        b'K' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        b'L' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        b'M' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        b'N' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        b'O' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        b'P' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        b'Q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        b'R' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        b'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        b'T' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        b'U' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        b'V' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        b'W' => [0x3F, 0x40, 0x38, 0x40, 0x3F],
        b'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        b'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        b'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        b':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        b'.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        b'/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        b'-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        b'_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        b'*' => [0x14, 0x08, 0x3E, 0x08, 0x14],
        b'[' => [0x00, 0x7F, 0x41, 0x41, 0x00],
        b']' => [0x00, 0x41, 0x41, 0x7F, 0x00],
        b'(' => [0x00, 0x1C, 0x22, 0x41, 0x00],
        b')' => [0x00, 0x41, 0x22, 0x1C, 0x00],
        b'%' => [0x23, 0x13, 0x08, 0x64, 0x62],
        b'!' => [0x00, 0x00, 0x5F, 0x00, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00],
    }
}
