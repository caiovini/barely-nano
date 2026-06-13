//! # U-Boot Standalone API Bindings
//!
//! This module provides a high-performance bridge between the bare-metal Rust node
//! and the underlying U-Boot bootloader. By leveraging U-Boot's Jump Table and
//! Syscall API, we can use mature C-based drivers for Ethernet, SSD (NVMe), and
//! I2C without the overhead of a full operating system.

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use spin::Mutex;

static UBOOT_LOCK: Mutex<()> = Mutex::new(());
static UBOOT_GP: AtomicUsize = AtomicUsize::new(0);

struct UBootContext {
    old_gp: usize,
}

impl UBootContext {
    #[inline(always)]
    fn enter() -> Self {
        let uboot_gp = UBOOT_GP.load(Ordering::Relaxed);
        let mut old_gp = 0usize;
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
        Self { old_gp }
    }
}

impl Drop for UBootContext {
    #[inline(always)]
    fn drop(&mut self) {
        if self.old_gp != 0 {
            unsafe {
                core::arch::asm!(
                    "mv gp, {old_gp}",
                    old_gp = in(reg) self.old_gp,
                );
            }
        }
    }
}

/// The U-Boot Jump Table structure.
/// This matches the `struct jt_funcs` defined in U-Boot's `include/_exports.h`.
#[repr(C)]
pub struct JumpTable {
    pub get_version: extern "C" fn() -> unsafe extern "C" fn() -> u64,
    pub getc: extern "C" fn() -> i32,
    pub tstc: extern "C" fn() -> i32,
    pub putc: extern "C" fn(c: u8),
    pub puts: extern "C" fn(s: *const u8),
    pub printf: extern "C" fn(fmt: *const u8, ...),
    pub install_hdlr: extern "C" fn(),
    pub free_hdlr: extern "C" fn(),
    pub malloc: extern "C" fn(size: usize) -> *mut c_void,
    pub free: extern "C" fn(ptr: *mut c_void),
    pub udelay: extern "C" fn(usec: u64),
    pub get_timer: extern "C" fn(base: u64) -> u64,
    pub vprintf: extern "C" fn(fmt: *const u8, args: *mut c_void) -> i32,
    pub do_reset: extern "C" fn() -> i32,
    pub env_get: extern "C" fn(name: *const u8) -> *const u8,
    pub env_set: extern "C" fn(name: *const u8, value: *const u8) -> i32,
    pub simple_strtoul: extern "C" fn(cp: *const u8, endp: *mut *const u8, base: u32) -> u64,
    pub strict_strtoul: extern "C" fn(cp: *const u8, base: u32, res: *mut u64) -> i32,
    pub simple_strtol: extern "C" fn(cp: *const u8, endp: *mut *const u8, base: u32) -> i64,
    pub strcmp: extern "C" fn(cs: *const u8, ct: *const u8) -> i32,
    pub i2c_write: extern "C" fn(chip: u8, addr: u32, alen: i32, buffer: *const u8, len: i32) -> i32,
    pub i2c_read: extern "C" fn(chip: u8, addr: u32, alen: i32, buffer: *mut u8, len: i32) -> i32,
    pub spi_setup_slave: extern "C" fn(),
    pub spi_free_slave: extern "C" fn(),
    pub spi_claim_bus: extern "C" fn(),
    pub spi_release_bus: extern "C" fn(),
    pub spi_xfer: extern "C" fn(),
    pub ustrtoul: extern "C" fn(cp: *const u8, endp: *mut *const u8, base: u32) -> u64,
    pub ustrtoull: extern "C" fn(cp: *const u8, endp: *mut *const u8, base: u32) -> u64,
    pub strcpy: extern "C" fn(dest: *mut u8, src: *const u8) -> *mut u8,
    pub mdelay: extern "C" fn(msec: u64),
    pub memset: extern "C" fn(s: *mut c_void, c: i32, n: usize) -> *mut c_void,
}

static UBOOT_JT: AtomicPtr<JumpTable> = AtomicPtr::new(core::ptr::null_mut());

pub unsafe fn init(jt_ptr: *const JumpTable) {
    if !jt_ptr.is_null() {
        UBOOT_JT.store(jt_ptr as *mut _, Ordering::SeqCst);
    }
    let mut gp_val = 0usize;
    core::arch::asm!("mv {}, gp", out(reg) gp_val);
    UBOOT_GP.store(gp_val, Ordering::SeqCst);
}

pub fn putc(c: u8) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).putc)(c);
        }
    }
}

pub fn puts(s: &str) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).puts)(s.as_ptr());
        }
    }
}

pub fn udelay(usec: u64) {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            ((*jt).udelay)(usec);
        }
    }
}

pub fn get_timer(base: u64) -> u64 {
    let jt = UBOOT_JT.load(Ordering::Relaxed);
    if !jt.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            return ((*jt).get_timer)(base);
        }
    }
    0
}

#[repr(C)]
pub struct ApiSignature {
    pub signature: [u8; 8],
    pub version: u16,
    pub _pad1: u16,
    pub checksum: u32,
    pub syscall: extern "C" fn(type_: i32, ret: *mut i32, ...) -> i32,
}

pub const API_DEV_ENUM: i32 = 9;
pub const API_DEV_OPEN: i32 = 10;
pub const API_DEV_CLOSE: i32 = 11;
pub const API_DEV_READ: i32 = 12;
pub const API_DEV_WRITE: i32 = 13;

static UBOOT_API: AtomicPtr<ApiSignature> = AtomicPtr::new(core::ptr::null_mut());

pub fn validate_api(ptr: usize) -> bool {
    unsafe {
        let sig = &*(ptr as *const ApiSignature);
        sig.signature == *b"UBootAPI"
    }
}

pub unsafe fn init_api(sig_ptr: *const ApiSignature) {
    if sig_ptr.is_null() { return; }
    UBOOT_API.store(sig_ptr as *mut _, Ordering::SeqCst);
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

/// Sends a raw network packet using the U-Boot Ethernet driver.
pub fn eth_send(info: &DeviceInfo, buffer: &[u8]) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    let mut ret = 0i32;
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let len = buffer.len() as i32;
            // API_dev_write for network takes 3 arguments: di, buf, len
            ((*api).syscall)(API_DEV_WRITE, &mut ret, info as *const DeviceInfo, buffer.as_ptr(), &len);
        }
    }
    ret
}

/// Receives a raw network packet using the U-Boot Ethernet driver.
pub fn eth_receive(info: &DeviceInfo, buffer: &mut [u8]) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    let mut ret = 0i32;
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut len = buffer.len() as i32;
            let mut actual_len = 0i32;
            // API_dev_read for network takes 4 arguments: di, buf, len, act_len
            ((*api).syscall)(API_DEV_READ, &mut ret, info as *const DeviceInfo, buffer.as_mut_ptr(), &mut len, &mut actual_len);
            if ret == 0 {
                return actual_len;
            } else {
                return -ret;
            }
        }
    }
    -1
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
            let val = ((*jt).env_get)(name_buf.as_ptr());
            if val.is_null() { None } else { Some(val) }
        }
    } else {
        None
    }
}

pub fn dev_enum(prev: Option<&DeviceInfo>) -> Option<DeviceInfo> {
    let api = UBOOT_API.load(Ordering::Relaxed);
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut ret = 0;
            let mut info = core::mem::zeroed::<DeviceInfo>();
            info.cookie = prev.map(|d| d.cookie).unwrap_or(core::ptr::null_mut());
            ((*api).syscall)(API_DEV_ENUM, &mut ret, &mut info);
            if ret == 0 && !info.cookie.is_null() {
                Some(info)
            } else {
                None
            }
        }
    } else {
        None
    }
}

pub fn dev_open(info: &mut DeviceInfo) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut ret = 0;
            ((*api).syscall)(API_DEV_OPEN, &mut ret, info as *mut DeviceInfo);
            ret
        }
    } else {
        -1
    }
}

pub fn dev_close(info: &mut DeviceInfo) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut ret = 0;
            ((*api).syscall)(API_DEV_CLOSE, &mut ret, info as *mut DeviceInfo);
            ret
        }
    } else {
        -1
    }
}

pub fn dev_read(info: &DeviceInfo, start_block: u64, blocks: u64, buffer: *mut u8) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut ret = 0;
            let mut actual_blocks = 0u64;
            ((*api).syscall)(API_DEV_READ, &mut ret, info as *const DeviceInfo, buffer, &blocks, &start_block, &mut actual_blocks);
            if ret == 0 { actual_blocks as i32 } else { -ret }
        }
    } else {
        -1
    }
}

pub fn dev_write(info: &DeviceInfo, start_block: u64, blocks: u64, buffer: *const u8) -> i32 {
    let api = UBOOT_API.load(Ordering::Relaxed);
    if !api.is_null() {
        let _lock = UBOOT_LOCK.lock();
        let _ctx = UBootContext::enter();
        unsafe {
            let mut ret = 0;
            ((*api).syscall)(API_DEV_WRITE, &mut ret, info as *const DeviceInfo, buffer, &blocks, &start_block);
            if ret == 0 { blocks as i32 } else { -ret }
        }
    } else {
        -1
    }
}
