//! Bare-metal `no_std` Waveshare 5-inch HDMI SimpleFB status & telemetry dashboard driver
//! for the SpacemiT K3 Pico-ITX (`8x X100 + 8x A100`, 16 GB LPDDR5).
//!
//! Renders a high-contrast 32-bpp XRGB8888 hardware dashboard directly into the U-Boot
//! SimpleFB / DPU HDMI framebuffer (`uboot::video_get_fb()`, `video_get_xsize()`,
//! `video_get_ysize()`) and flushes cache lines via `uboot::video_sync_all()`.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static FB_ADDR: AtomicUsize = AtomicUsize::new(0);
static FB_WIDTH: AtomicUsize = AtomicUsize::new(0);
static FB_HEIGHT: AtomicUsize = AtomicUsize::new(0);
static FB_HEARTBEAT: AtomicBool = AtomicBool::new(false);

const COL_BG: u32 = 0x000B_0F19;
const COL_PANEL: u32 = 0x0013_1B2E;
const COL_HEADER: u32 = 0x001E_293B;
const COL_BORDER: u32 = 0x0033_4155;
const COL_TEXT: u32 = 0x00F8_FAFC;
const COL_MUTED: u32 = 0x0094_A3B8;
const COL_ACCENT: u32 = 0x0038_BDF8;
const COL_GREEN: u32 = 0x0022_C55E;
const COL_AMBER: u32 = 0x00F5_9E0B;

#[cfg(feature = "block-test")]
static TEST_BPS: AtomicUsize = AtomicUsize::new(0);

/// Record the deterministic block test throughput (blocks per second) for HDMI display.
#[cfg(feature = "block-test")]
pub fn set_test_bps(bps: usize) {
    TEST_BPS.store(bps, Ordering::Relaxed);
}

#[inline(always)]
unsafe fn fill_rect(fb: *mut u32, stride: usize, w: usize, h: usize, x: usize, y: usize, rw: usize, rh: usize, color: u32) {
    let x_end = (x + rw).min(w);
    let y_end = (y + rh).min(h);
    let mut row = y;
    while row < y_end {
        let base = row * stride;
        let mut col = x;
        while col < x_end {
            fb.add(base + col).write_volatile(color);
            col += 1;
        }
        row += 1;
    }
}

#[inline(always)]
unsafe fn draw_char(fb: *mut u32, stride: usize, w: usize, h: usize, x: usize, y: usize, ch: u8, scale: usize, fg: u32) {
    let cols = glyph_5x7(ch);
    for (cx, &col_bits) in cols.iter().enumerate() {
        for cy in 0..7usize {
            if (col_bits & (1 << cy)) != 0 {
                let px = x + cx * scale;
                let py = y + cy * scale;
                fill_rect(fb, stride, w, h, px, py, scale, scale, fg);
            }
        }
    }
}

unsafe fn draw_text(fb: *mut u32, stride: usize, w: usize, h: usize, mut x: usize, y: usize, text: &[u8], scale: usize, fg: u32) {
    let adv = 6 * scale;
    for &ch in text {
        if x + adv > w {
            break;
        }
        draw_char(fb, stride, w, h, x, y, ch, scale, fg);
        x += adv;
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

/// Probe the U-Boot SimpleFB / HDMI DPU framebuffer via the jump table and draw the static
/// SpacemiT K3 Pico-ITX dashboard chrome.
pub fn init() -> bool {
    let fb = crate::uboot::video_get_fb();
    let w = crate::uboot::video_get_xsize();
    let h = crate::uboot::video_get_ysize();
    if fb == 0 || w < 320 || h < 240 {
        tracing::debug!("[hdmi_fb] No active U-Boot SimpleFB detected (fb={:#x}, {}x{})", fb, w, h);
        FB_ADDR.store(0, Ordering::SeqCst);
        return false;
    }

    FB_ADDR.store(fb, Ordering::SeqCst);
    FB_WIDTH.store(w, Ordering::SeqCst);
    FB_HEIGHT.store(h, Ordering::SeqCst);

    // SAFETY: `fb` is a valid physical framebuffer base returned by U-Boot's active video device
    // with dimensions `w x h` at 32-bpp XRGB8888.
    unsafe {
        let ptr = fb as *mut u32;
        fill_rect(ptr, w, w, h, 0, 0, w, h, COL_BG);
        fill_rect(ptr, w, w, h, 0, 0, w, 52, COL_HEADER);
        fill_rect(ptr, w, w, h, 0, 52, w, 2, COL_ACCENT);
        draw_text(
            ptr,
            w,
            w,
            h,
            20,
            16,
            b"BARELY-NANO  |  SPACEMIT K3 PICO-ITX (16H / 16GB LPDDR5)",
            2,
            COL_TEXT,
        );
        draw_text(
            ptr,
            w,
            w,
            h,
            20,
            72,
            b"STATUS: BOOTING 64-SHARD NVME LEDGER...",
            2,
            COL_ACCENT,
        );
    }
    crate::uboot::video_sync_all();
    tracing::info!("[hdmi_fb] Initialized Waveshare HDMI SimpleFB ({}x{} @ {:#x})", w, h, fb);
    true
}

/// Render 4 concise status lines onto the HDMI framebuffer.
pub fn show_lines(lines: &[&[u8]; 4]) {
    let fb = FB_ADDR.load(Ordering::Relaxed);
    if fb == 0 {
        return;
    }
    let w = FB_WIDTH.load(Ordering::Relaxed);
    let h = FB_HEIGHT.load(Ordering::Relaxed);
    // SAFETY: `fb` was validated in `init()` as a `w x h` 32-bpp framebuffer.
    unsafe {
        let ptr = fb as *mut u32;
        fill_rect(ptr, w, w, h, 16, 68, w.saturating_sub(32), 140, COL_PANEL);
        for (idx, line) in lines.iter().enumerate() {
            draw_text(ptr, w, w, h, 28, 80 + idx * 30, line, 2, COL_TEXT);
        }
    }
    crate::uboot::video_sync_all();
}

/// Update the HDMI display with concise node status metrics (drop-in replacement for `oled::update_status`).
pub fn update_status(peers: usize, blocks: usize, confirmed: usize, bps: usize, db_kb: usize) {
    update_dashboard(peers, blocks, confirmed, 0, 0, 0, bps, 0, db_kb, 0, 0);
}

/// Render the full SpacemiT K3 16-Hart / 64-Shard hardware telemetry dashboard onto the HDMI display.
#[allow(clippy::too_many_arguments)]
pub fn update_dashboard(
    peers: usize,
    blocks: usize,
    confirmed: usize,
    unchecked: usize,
    bpq: usize,
    aec: usize,
    bps: usize,
    cps: usize,
    db_kb: usize,
    bpm_mb: usize,
    heap_mb: usize,
) {
    let fb = FB_ADDR.load(Ordering::Relaxed);
    if fb == 0 {
        return;
    }
    let w = FB_WIDTH.load(Ordering::Relaxed);
    let h = FB_HEIGHT.load(Ordering::Relaxed);
    let hb = FB_HEARTBEAT.fetch_xor(true, Ordering::Relaxed);

    #[cfg(feature = "block-test")]
    let display_bps = {
        let _ = bps;
        TEST_BPS.load(Ordering::Relaxed)
    };
    #[cfg(not(feature = "block-test"))]
    let display_bps = bps;

    // SAFETY: `fb` was validated in `init()` as a `w x h` 32-bpp framebuffer.
    unsafe {
        let ptr = fb as *mut u32;

        // Heartbeat indicator in top-right header
        fill_rect(
            ptr,
            w,
            w,
            h,
            w.saturating_sub(36),
            18,
            16,
            16,
            if hb { COL_GREEN } else { COL_ACCENT },
        );

        // Main telemetry card
        let card_w = w.saturating_sub(32);
        let card_h = h.saturating_sub(76);
        fill_rect(ptr, w, w, h, 16, 64, card_w, card_h, COL_PANEL);
        fill_rect(ptr, w, w, h, 16, 64, card_w, 2, COL_BORDER);

        // Row 1: Throughput (BPS & CPS) + Peers
        let mut line = [b' '; 64];
        line[0..5].copy_from_slice(b"BPS: ");
        let n = fmt_usize(display_bps, &mut line[5..15]);
        line[5 + n..12 + n].copy_from_slice(b"/S   | ");
        let p0 = 12 + n;
        line[p0..p0 + 5].copy_from_slice(b"CPS: ");
        let n2 = fmt_usize(cps, &mut line[p0 + 5..p0 + 15]);
        let p1 = p0 + 5 + n2;
        line[p1..p1 + 14].copy_from_slice(b"/S   | PEERS: ");
        let _ = fmt_usize(peers, &mut line[p1 + 14..64]);
        draw_text(ptr, w, w, h, 32, 84, &line, 3, COL_GREEN);

        // Row 2: Stored Blocks & Confirmed Blocks
        let mut line2 = [b' '; 64];
        line2[0..10].copy_from_slice(b"STORED:   ");
        let n = fmt_usize(blocks, &mut line2[10..26]);
        let p = 10 + n;
        line2[p..p + 15].copy_from_slice(b"   | CONFIRMED:");
        let _ = fmt_usize(confirmed, &mut line2[p + 16..64]);
        draw_text(ptr, w, w, h, 32, 132, &line2, 2, COL_TEXT);

        // Row 3: Queues (Unchecked, BPQ, Active Elections)
        let mut line3 = [b' '; 64];
        line3[0..10].copy_from_slice(b"UNCHECKED:");
        let n = fmt_usize(unchecked, &mut line3[11..22]);
        let p = 11 + n;
        line3[p..p + 9].copy_from_slice(b"  | BPQ: ");
        let n2 = fmt_usize(bpq, &mut line3[p + 9..p + 20]);
        let p2 = p + 9 + n2;
        line3[p2..p2 + 9].copy_from_slice(b"  | AEC: ");
        let _ = fmt_usize(aec, &mut line3[p2 + 9..64]);
        draw_text(ptr, w, w, h, 32, 168, &line3, 2, COL_MUTED);

        // Row 4: 64-Shard Buffer Pool (12 GB High DRAM) + Heap (3 GB) + NVMe DB Size
        let mut line4 = [b' '; 64];
        line4[0..9].copy_from_slice(b"64S BPM: ");
        let n = fmt_usize(bpm_mb, &mut line4[9..17]);
        let p = 9 + n;
        line4[p..p + 18].copy_from_slice(b"/12288 MB | HEAP: ");
        let n2 = fmt_usize(heap_mb, &mut line4[p + 18..p + 26]);
        let p2 = p + 18 + n2;
        line4[p2..p2 + 16].copy_from_slice(b"/3072 MB | NVME:");
        let db_mb = db_kb / 1024;
        let n3 = fmt_usize(db_mb, &mut line4[p2 + 17..61]);
        if p2 + 17 + n3 + 2 <= 64 {
            line4[p2 + 17 + n3] = b'M';
            line4[p2 + 18 + n3] = b'B';
        }
        draw_text(ptr, w, w, h, 32, 204, &line4, 2, COL_ACCENT);

        // Row 5: 16-Hart Topology summary
        draw_text(
            ptr,
            w,
            w,
            h,
            32,
            244,
            b"C0(H0..3):10G+MSG+BOOT | C1(H4..7):AEC+3xLEDGER | C2-3(H8..15):VLEN1024 CRYPTO",
            2,
            COL_AMBER,
        );
    }

    crate::uboot::video_sync_all();
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
        b'|' => [0x00, 0x00, 0x7F, 0x00, 0x00],
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
        b'+' => [0x08, 0x08, 0x3E, 0x08, 0x08],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00],
    }
}
