//! # SpacemiT K3 U-Boot Jump Table (`gd->jt`) Bindings
//!
//! Provides a direct, zero-copy FFI bridge between the bare-metal Rust node and
//! U-Boot on SpacemiT K3 (`gd->jt` at byte offset `0x20` of `struct global_data`).
//! Exposes:
//! - Console & Environment (`putc`, `puts`, `udelay`, `get_timer`, `env_get`)
//! - Realtek RTL8127 10GbE SFP+ Ethernet (`eth_init`, `eth_halt`, `eth_send`, `eth_receive`)
//! - Waveshare 5-inch HDMI SimpleFB Display (`video_get_fb`, `video_get_xsize`, `video_get_ysize`,
//!   `video_clear`, `video_fill`, `video_sync_all`, `bmp_display`)
//! - PCIe NVMe Block Storage (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba`)

use core::ffi::{c_char, c_int, c_ulong, c_void};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use spin::Mutex;

static UBOOT_LOCK: Mutex<()> = Mutex::new(());
static UBOOT_NET_LOCK: Mutex<()> = Mutex::new(());
static UBOOT_GP: AtomicUsize = AtomicUsize::new(0);

/// Byte offset of `gd->jt` inside `struct global_data` on 64-bit RISC-V (`GD_JT`).
pub const GD_JT_OFFSET: usize = 0x20;

struct UBootContext {
    old_gp: usize,
}

impl UBootContext {
    #[inline(always)]
    fn enter() -> Self {
        let uboot_gp = UBOOT_GP.load(Ordering::Relaxed);
        let mut old_gp = 0usize;
        #[cfg(target_arch = "riscv64")]
        if uboot_gp != 0 {
            unsafe {
                core::arch::asm!(
                    "mv {old_gp}, gp",
                    "mv gp, {uboot_gp}",
                    old_gp = out(reg) old_gp,
                    uboot_gp = in(reg) uboot_gp,
                );
            }
        }
        #[cfg(not(target_arch = "riscv64"))]
        let _ = (&mut old_gp, uboot_gp);
        Self { old_gp }
    }
}

impl Drop for UBootContext {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(target_arch = "riscv64")]
        if self.old_gp != 0 {
            unsafe {
                core::arch::asm!(
                    "mv gp, {old_gp}",
                    old_gp = in(reg) self.old_gp,
                );
            }
        }
        #[cfg(not(target_arch = "riscv64"))]
        let _ = self.old_gp;
    }
}

/// The SpacemiT K3 U-Boot Jump Table structure (`59` function pointers).
/// Matches `struct jt_funcs` defined in `u-boot-pico-itx-k3/include/_exports.h`.
#[repr(C)]
pub struct JumpTable {
    // 0..17: Console, Heap, Timer, Environment
    pub get_version: unsafe extern "C" fn() -> c_ulong,
    pub getc: unsafe extern "C" fn() -> c_int,
    pub tstc: unsafe extern "C" fn() -> c_int,
    pub putc: unsafe extern "C" fn(c_char),
    pub puts: unsafe extern "C" fn(*const c_char),
    pub flush: unsafe extern "C" fn(),
    pub printf: unsafe extern "C" fn(*const c_char, ...) -> c_int,
    pub vprintf: *const c_void,
    pub malloc: unsafe extern "C" fn(usize) -> *mut c_void,
    pub realloc: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void,
    pub calloc: unsafe extern "C" fn(usize, usize) -> *mut c_void,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub mdelay: unsafe extern "C" fn(c_ulong),
    pub udelay: unsafe extern "C" fn(c_ulong),
    pub get_timer: unsafe extern "C" fn(c_ulong) -> c_ulong,
    pub do_reset: *const c_void,
    pub env_get: unsafe extern "C" fn(*const c_char) -> *const u8,
    pub env_set: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,

    // 18..37: String & memory helpers (20 pointers)
    pub _str_mem_helpers: [*const c_void; 20],

    // 38..41: MDIO/PHY helpers (4 pointers)
    pub _mdio_helpers: [*const c_void; 4],

    // 42..48 (offset 0x150..0x180): Ethernet API (RTL8127 10GbE Fiber / active eth device)
    pub eth_init: unsafe extern "C" fn() -> c_int,
    pub eth_halt: unsafe extern "C" fn(),
    pub eth_send: unsafe extern "C" fn(packet: *mut c_void, length: c_int) -> c_int,
    pub eth_rx: unsafe extern "C" fn() -> c_int,
    pub eth_receive: unsafe extern "C" fn(packet: *mut c_void, length: c_int) -> c_int,
    pub eth_get_name: unsafe extern "C" fn() -> *const c_char,
    pub eth_set_current: unsafe extern "C" fn(),

    // 49..55 (offset 0x188..0x1b8): HDMI / SimpleFB Video API (Waveshare 5-inch HDMI LCD)
    pub video_get_fb: unsafe extern "C" fn() -> c_ulong,
    pub video_get_xsize: unsafe extern "C" fn(dev: *mut c_void) -> c_int,
    pub video_get_ysize: unsafe extern "C" fn(dev: *mut c_void) -> c_int,
    pub video_clear: unsafe extern "C" fn(dev: *mut c_void) -> c_int,
    pub video_fill: unsafe extern "C" fn(dev: *mut c_void, colour: u32) -> c_int,
    pub video_sync_all: unsafe extern "C" fn(),
    pub bmp_display: unsafe extern "C" fn(addr: c_ulong, x: c_int, y: c_int) -> c_int,

    // 56..58 (offset 0x1c0..0x1d0): NVMe / Block Storage API
    pub blk_get_capacity: unsafe extern "C" fn() -> c_ulong,
    pub blk_read_lba:
        unsafe extern "C" fn(start: c_ulong, blkcnt: c_ulong, buffer: *mut c_void) -> c_ulong,
    pub blk_write_lba:
        unsafe extern "C" fn(start: c_ulong, blkcnt: c_ulong, buffer: *const c_void) -> c_ulong,
}

static UBOOT_JT: AtomicPtr<JumpTable> = AtomicPtr::new(core::ptr::null_mut());

pub unsafe fn init(jt_ptr: *const JumpTable, uboot_gp: usize) {
    if !jt_ptr.is_null() {
        UBOOT_JT.store(jt_ptr as *mut _, Ordering::SeqCst);
    }
    let gp_val = if uboot_gp != 0 {
        uboot_gp
    } else {
        let mut cur_gp = 0usize;
        #[cfg(target_arch = "riscv64")]
        core::arch::asm!("mv {}, gp", out(reg) cur_gp);
        #[cfg(not(target_arch = "riscv64"))]
        let _ = &mut cur_gp;
        cur_gp
    };
    UBOOT_GP.store(gp_val, Ordering::SeqCst);
}

#[inline]
pub fn gp_addr() -> usize {
    UBOOT_GP.load(Ordering::Relaxed)
}

#[inline]
pub fn jt_addr() -> usize {
    UBOOT_JT.load(Ordering::Relaxed) as usize
}

#[inline]
pub fn is_initialized() -> bool {
    !UBOOT_JT.load(Ordering::Relaxed).is_null()
}

pub fn putc(c: u8) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).putc)(c as c_char);
        }
    }
}

pub fn puts(s: &str) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).puts)(s.as_ptr() as *const c_char);
        }
    }
}

pub fn udelay(usec: u64) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).udelay)(usec as c_ulong);
        }
    }
}

pub fn get_timer(base: u64) -> u64 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            return ((*jt).get_timer)(base as c_ulong) as u64;
        }
    }
    0
}

pub fn env_get(name: &str) -> Option<*const u8> {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        let mut name_buf = [0u8; 64];
        let len = name.len().min(63);
        name_buf[0..len].copy_from_slice(&name.as_bytes()[0..len]);
        name_buf[len] = 0;
        unsafe {
            let val = ((*jt).env_get)(name_buf.as_ptr() as *const c_char);
            if val.is_null() { None } else { Some(val) }
        }
    } else {
        None
    }
}

pub fn env_set(name: &str, value: &str) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        let mut name_buf = [0u8; 64];
        let nlen = name.len().min(63);
        name_buf[0..nlen].copy_from_slice(&name.as_bytes()[0..nlen]);
        name_buf[nlen] = 0;

        let mut val_buf = [0u8; 128];
        let vlen = value.len().min(127);
        val_buf[0..vlen].copy_from_slice(&value.as_bytes()[0..vlen]);
        val_buf[vlen] = 0;

        unsafe {
            return ((*jt).env_set)(
                name_buf.as_ptr() as *const c_char,
                val_buf.as_ptr() as *const c_char,
            );
        }
    }
    -1
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceInfo {
    pub type_: i32,
    pub _pad1: i32,
    pub cookie: *mut c_void,
    pub block_count: u64,
    pub block_size: u64,
    pub state: i32,
    pub _pad2: i32,
}

unsafe impl Send for DeviceInfo {}
unsafe impl Sync for DeviceInfo {}

pub const DEV_TYP_NET: i32 = 1;
pub const DEV_TYP_STOR: i32 = 2;

/// Static NVMe `if_type` tag (`8 != 6 (MMC)` and `!= 7 (SD)`) referenced by `DeviceInfo::cookie`.
static NVME_COOKIE_IF_TYPE: i32 = 8;
static NET_COOKIE_TAG: i32 = 1;

#[inline]
pub fn eth_disable_tx_auto_poll() {}

#[inline]
pub fn eth_check_tx_stall() {}

/// Returns the active U-Boot Ethernet device name (e.g. `"eth_rtl8169"` for RTL8127 10GbE).
pub fn eth_get_name() -> &'static str {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_NET_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let ptr = ((*jt).eth_get_name)() as *const u8;
            if !ptr.is_null() {
                let mut len = 0usize;
                while len < 64 && *ptr.add(len) != 0 {
                    len += 1;
                }
                if let Ok(s) = core::str::from_utf8(core::slice::from_raw_parts(ptr, len)) {
                    return s;
                }
            }
        }
    }
    "unknown"
}

/// Sends a raw network packet using U-Boot's `eth_send` Jump Table entry.
pub fn eth_send(_info: &DeviceInfo, buffer: &[u8]) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_NET_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            return ((*jt).eth_send)(buffer.as_ptr() as *mut c_void, buffer.len() as c_int);
        }
    }
    -1
}

/// Sends a batch of raw network packets using U-Boot's `eth_send` Jump Table entry,
/// amortizing `UBOOT_NET_LOCK` and `gp` context switching across the entire batch.
pub fn eth_send_batch(
    _info: &DeviceInfo,
    slots: &[[u8; 1536]],
    lens: &[u16],
    start_head: usize,
    count: usize,
) -> usize {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if jt.is_null() || count == 0 {
        return 0;
    }
    let _lock = UBOOT_NET_LOCK.lock();
    let _ctx = UBootContext::enter();
    let total_slots = slots.len();
    let mut sent = 0;
    let mut curr_idx = start_head;

    while sent < count {
        let len = lens[curr_idx] as c_int;
        let ret = unsafe { ((*jt).eth_send)(slots[curr_idx].as_ptr() as *mut c_void, len) };
        if ret < 0 {
            sent += 1;
            break;
        }
        curr_idx = (curr_idx + 1) % total_slots;
        sent += 1;
    }
    sent
}

/// Receives a raw network packet using U-Boot's `eth_receive` Jump Table entry.
pub fn eth_receive(_info: &DeviceInfo, buffer: &mut [u8]) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_NET_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let copied =
                ((*jt).eth_receive)(buffer.as_mut_ptr() as *mut c_void, buffer.len() as c_int);
            if copied > 0 {
                return copied;
            }
        }
    }
    -1
}

/// Receives a batch of raw network packets using U-Boot's `eth_receive` Jump Table entry,
/// draining up to `max_count` frames from the RTL8127 512-descriptor RX ring in one lock hold.
pub fn eth_receive_batch(
    _info: &DeviceInfo,
    slots: &mut [[u8; 1536]],
    lens: &mut [u16],
    start_tail: usize,
    max_count: usize,
) -> usize {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if jt.is_null() || max_count == 0 {
        return 0;
    }
    let _lock = UBOOT_NET_LOCK.lock();
    let _ctx = UBootContext::enter();
    let total_slots = slots.len();
    let mut received = 0;
    let mut curr_idx = start_tail;

    while received < max_count {
        let max_len = slots[curr_idx].len() as c_int;
        let copied =
            unsafe { ((*jt).eth_receive)(slots[curr_idx].as_mut_ptr() as *mut c_void, max_len) };
        if copied > 0 {
            lens[curr_idx] = copied as u16;
            curr_idx = (curr_idx + 1) % total_slots;
            received += 1;
        } else {
            break;
        }
    }
    received
}

pub fn dev_enum(prev: Option<&DeviceInfo>) -> Option<DeviceInfo> {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if jt.is_null() {
        return None;
    }
    match prev {
        None => Some(DeviceInfo {
            type_: DEV_TYP_NET,
            _pad1: 0,
            cookie: (&NET_COOKIE_TAG as *const i32) as *mut c_void,
            block_count: 0,
            block_size: 0,
            state: 0,
            _pad2: 0,
        }),
        Some(p) if p.type_ == DEV_TYP_NET => {
            let cap = {
                let _lock = UBOOT_LOCK.lock();
                let _ctx = UBootContext::enter();
                unsafe { ((*jt).blk_get_capacity)() as u64 }
            };
            if cap > 0 {
                Some(DeviceInfo {
                    type_: DEV_TYP_STOR,
                    _pad1: 0,
                    cookie: (&NVME_COOKIE_IF_TYPE as *const i32) as *mut c_void,
                    block_count: cap,
                    block_size: 512,
                    state: 0,
                    _pad2: 0,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn dev_open(info: &mut DeviceInfo) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if jt.is_null() {
        return -1;
    }
    if info.type_ == DEV_TYP_NET {
        let _lock = UBOOT_NET_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).eth_set_current)();
            ((*jt).eth_halt)();
            ((*jt).eth_init)()
        }
    } else if info.type_ == DEV_TYP_STOR {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        let cap = unsafe { ((*jt).blk_get_capacity)() as u64 };
        if cap > 0 {
            info.block_count = cap;
            info.block_size = 512;
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

pub fn dev_close(info: &mut DeviceInfo) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if jt.is_null() {
        return -1;
    }
    if info.type_ == DEV_TYP_NET {
        let _lock = UBOOT_NET_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).eth_halt)();
        }
    }
    0
}

/// Block-device I/O profiling (CLINT ticks unless noted):
/// [0]=read calls, [1]=read sectors, [2]=read UBOOT_LOCK wait, [3]=read device time,
/// [4]=write calls, [5]=write sectors, [6]=write UBOOT_LOCK wait, [7]=write device time.
pub static DEV_IO_PROF: [core::sync::atomic::AtomicU64; 8] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 8];

pub fn dev_read(_info: &DeviceInfo, start_block: u64, blocks: u64, buffer: *mut u8) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let t0 = crate::clint::mtime();
        let _lock = UBOOT_LOCK.lock();
        let t1 = crate::clint::mtime();
        let _ctx = UBootContext::enter();
        let res = unsafe {
            let read = ((*jt).blk_read_lba)(
                start_block as c_ulong,
                blocks as c_ulong,
                buffer as *mut c_void,
            );
            read as i32
        };
        let t2 = crate::clint::mtime();
        DEV_IO_PROF[0].fetch_add(1, Ordering::Relaxed);
        DEV_IO_PROF[1].fetch_add(blocks, Ordering::Relaxed);
        DEV_IO_PROF[2].fetch_add(t1.wrapping_sub(t0), Ordering::Relaxed);
        DEV_IO_PROF[3].fetch_add(t2.wrapping_sub(t1), Ordering::Relaxed);
        res
    } else {
        -1
    }
}

pub fn dev_write(_info: &DeviceInfo, start_block: u64, blocks: u64, buffer: *const u8) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let t0 = crate::clint::mtime();
        let _lock = UBOOT_LOCK.lock();
        let t1 = crate::clint::mtime();
        let _ctx = UBootContext::enter();
        let res = unsafe {
            let written = ((*jt).blk_write_lba)(
                start_block as c_ulong,
                blocks as c_ulong,
                buffer as *const c_void,
            );
            written as i32
        };
        let t2 = crate::clint::mtime();
        DEV_IO_PROF[4].fetch_add(1, Ordering::Relaxed);
        DEV_IO_PROF[5].fetch_add(blocks, Ordering::Relaxed);
        DEV_IO_PROF[6].fetch_add(t1.wrapping_sub(t0), Ordering::Relaxed);
        DEV_IO_PROF[7].fetch_add(t2.wrapping_sub(t1), Ordering::Relaxed);
        res
    } else {
        -1
    }
}

// ── HDMI / SimpleFB Video Framebuffer API (Waveshare 5-inch HDMI LCD) ────────

/// Returns the physical framebuffer base address (`0` if no HDMI display is active).
pub fn video_get_fb() -> usize {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe { ((*jt).video_get_fb)() as usize }
    } else {
        0
    }
}

/// Returns the horizontal resolution in pixels of the primary HDMI display.
pub fn video_get_xsize() -> usize {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        let w = unsafe { ((*jt).video_get_xsize)(core::ptr::null_mut()) };
        if w > 0 { w as usize } else { 0 }
    } else {
        0
    }
}

/// Returns the vertical resolution in pixels of the primary HDMI display.
pub fn video_get_ysize() -> usize {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        let h = unsafe { ((*jt).video_get_ysize)(core::ptr::null_mut()) };
        if h > 0 { h as usize } else { 0 }
    } else {
        0
    }
}

/// Clears the primary HDMI video framebuffer.
pub fn video_clear() -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe { ((*jt).video_clear)(core::ptr::null_mut()) }
    } else {
        -1
    }
}

/// Fills the primary HDMI video framebuffer with `colour`.
pub fn video_fill(colour: u32) -> i32 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe { ((*jt).video_fill)(core::ptr::null_mut(), colour) }
    } else {
        -1
    }
}

/// Flushes CPU D-cache lines for the HDMI framebuffer to DRAM so scanout reflects updates.
pub fn video_sync_all() {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).video_sync_all)();
        }
    }
}
