#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use core::arch::global_asm;
use core::future::Future;
use core::panic::PanicInfo;
use core::pin::Pin;
use core::str::FromStr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kernel_common::{trap, uart};
use linked_list_allocator::LockedHeap;
use rsnano_core::utils::Tickable;
use rsnano_node::Node;
use rsnano_rpc_handler::RpcCommandHandler;
use rsnano_rpc_server::bare_metal::BareMetalRpcServer;

static SLAB_FREE: [spin::Mutex<(usize, usize)>; 16] = [const { spin::Mutex::new((0, 0)) }; 16];

struct InterruptSafeAllocator(LockedHeap);

impl InterruptSafeAllocator {
    #[inline]
    fn slab_class(layout: core::alloc::Layout) -> Option<usize> {
        let size = layout.size();
        if size == 0 || size > (64 << 15) || layout.align() > 64 {
            return None;
        }
        // Exclude boot-time 4160-byte BufferPool Frame allocations so they are not rounded up to 8 KB.
        if size == core::mem::size_of::<kernel_common::storage::bpm::Frame>() && layout.align() == 64 {
            return None;
        }
        let sz = size.max(64).next_power_of_two();
        Some((sz.trailing_zeros() as usize) - 6)
    }

    /// Bytes of freed memory currently parked in the slab free lists (counted as "used" by
    /// `LockedHeap` but reusable only for the same size class until `drain_slabs` runs).
    fn slab_cached_bytes() -> usize {
        let mut total = 0usize;
        for c in 0..SLAB_FREE.len() {
            total += SLAB_FREE[c].lock().1 * (64usize << c);
        }
        total
    }

    /// Prints allocator state after a final allocation failure (slabs already drained), so the
    /// post-mortem in orpi.log shows whether the heap is exhausted or just fragmented.
    /// Uses only the UART (no allocation) and `try_lock`, so it cannot deadlock or recurse.
    fn report_oom(&self, layout: core::alloc::Layout) {
        let hart_id: usize;
        // SAFETY: reads the `tp` register, which holds the hart id on this platform.
        unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id) };
        let (used, free, size) = match self.0.try_lock() {
            Some(h) => (h.used(), h.free(), h.size()),
            None => (0, 0, 0),
        };
        let _g = kernel_common::uart::UART_LOCK.try_lock();
        kernel_common::uart::print_no_lock(b"[OOM] hart=");
        kernel_common::uart::print_usize_no_lock(hart_id);
        kernel_common::uart::print_no_lock(b" req=");
        kernel_common::uart::print_usize_no_lock(layout.size());
        kernel_common::uart::print_no_lock(b" align=");
        kernel_common::uart::print_usize_no_lock(layout.align());
        kernel_common::uart::print_no_lock(b" heap_size_mb=");
        kernel_common::uart::print_usize_no_lock(size / 1024 / 1024);
        kernel_common::uart::print_no_lock(b" used_mb=");
        kernel_common::uart::print_usize_no_lock(used / 1024 / 1024);
        kernel_common::uart::print_no_lock(b" free_mb=");
        kernel_common::uart::print_usize_no_lock(free / 1024 / 1024);
        kernel_common::uart::print_no_lock(b" slab_cached_mb=");
        kernel_common::uart::print_usize_no_lock(Self::slab_cached_bytes() / 1024 / 1024);
        kernel_common::uart::print_no_lock(b"\n");
    }

    unsafe fn drain_slabs(&self) {
        let mut heap = self.0.lock();
        // Drain all cached slabs (0..SLAB_FREE.len()) back to LockedHeap on allocation failure
        // so adjacent small blocks can coalesce into large contiguous spans.
        for c in 0..SLAB_FREE.len() {
            let slab_layout = core::alloc::Layout::from_size_align_unchecked(64 << c, 64);
            let mut slot = SLAB_FREE[c].lock();
            let mut cur = slot.0;
            slot.0 = 0;
            slot.1 = 0;
            drop(slot);
            while cur != 0 {
                let next = *(cur as *const usize);
                heap.deallocate(core::ptr::NonNull::new_unchecked(cur as *mut u8), slab_layout);
                cur = next;
            }
        }
    }
}

impl InterruptSafeAllocator {
    unsafe fn alloc_raw(&self, layout: core::alloc::Layout) -> *mut u8 {
        use core::alloc::GlobalAlloc as _;
        let _intr = kernel_common::uart::InterruptGuard::new();
        if let Some(c) = Self::slab_class(layout) {
            {
                let mut slot = SLAB_FREE[c].lock();
                if slot.0 != 0 {
                    let ptr = slot.0 as *mut usize;
                    slot.0 = *ptr;
                    slot.1 -= 1;
                    return ptr as *mut u8;
                }
            }
            let slab_layout = core::alloc::Layout::from_size_align_unchecked(64 << c, 64);
            let p = self.0.alloc(slab_layout);
            if !p.is_null() {
                return p;
            }
            self.drain_slabs();
            let p = self.0.alloc(slab_layout);
            if p.is_null() {
                self.report_oom(layout);
            }
            return p;
        }
        let p = self.0.alloc(layout);
        if !p.is_null() {
            return p;
        }
        self.drain_slabs();
        let p = self.0.alloc(layout);
        if p.is_null() {
            self.report_oom(layout);
        }
        p
    }
    unsafe fn dealloc_raw(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        use core::alloc::GlobalAlloc as _;
        let _intr = kernel_common::uart::InterruptGuard::new();
        if let Some(c) = Self::slab_class(layout) {
            let mut slot = SLAB_FREE[c].lock();
            *(ptr as *mut usize) = slot.0;
            slot.0 = ptr as usize;
            slot.1 += 1;
            return;
        }
        self.0.dealloc(ptr, layout)
    }
}

// SAFETY: forwards every call unchanged to `alloc_raw`/`dealloc_raw`.
unsafe impl core::alloc::GlobalAlloc for InterruptSafeAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.alloc_raw(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        self.dealloc_raw(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: InterruptSafeAllocator = InterruptSafeAllocator(LockedHeap::empty());

global_asm!(include_str!("entry.S"));

static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(999);
static BOOT_COMPLETE: AtomicBool = AtomicBool::new(false);
static TRAP_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);
static NODE_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);
/// Set by Hart 1 AFTER it has acquired node_arc+exec and released NODE/EXECUTOR locks.
/// Secondary harts wait for this before grabbing globals to avoid lock contention.
static HART_DISPATCH_READY: AtomicBool = AtomicBool::new(false);
pub static DB_PAGES_FREE: AtomicUsize = AtomicUsize::new(1024);
static HEAP_MAGIC: AtomicUsize = AtomicUsize::new(0);
const MAGIC_VAL: usize = 0xACE1FEED;
static NODE: spin::Mutex<Option<NodeWrapper>> = spin::Mutex::new(None);

const CLINT_FREQ: u64 = kernel_common::clint::FREQ;

#[inline]
fn seconds(s: u64) -> u64 {
    s * CLINT_FREQ
}

#[inline]
fn millis(ms: u64) -> u64 {
    ms * CLINT_FREQ / 1000
}

fn parse_hex_env(ptr: *const u8) -> Option<usize> {
    if ptr.is_null() {
        return None;
    }
    let mut val = 0_usize;
    unsafe {
        let mut p = ptr;
        // Skip leading '0x' or '0X' if present
        if *p == b'0' && (*p.add(1) == b'x' || *p.add(1) == b'X') {
            p = p.add(2);
        }
        let mut has_digits = false;
        while *p != 0 {
            let digit = match *p {
                b'0'..=b'9' => *p - b'0',
                b'a'..=b'f' => *p - b'a' + 10,
                b'A'..=b'F' => *p - b'A' + 10,
                _ => break,
            };
            val = (val << 4) | (digit as usize);
            p = p.add(1);
            has_digits = true;
        }
        if has_digits {
            Some(val)
        } else {
            None
        }
    }
}

fn parse_mac_str(s: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut count = 0;
    for part in s.split(':') {
        if count >= 6 {
            return None;
        }
        let byte = u8::from_str_radix(part.trim(), 16).ok()?;
        mac[count] = byte;
        count += 1;
    }
    if count == 6 {
        Some(mac)
    } else {
        None
    }
}

/// Default RAM address where `boot.cmd` fatloads `config.toml` (`0x10F00000`, 1 MB below kernel `_start`).
const CONFIG_RAM_ADDR: usize = 0x10F0_0000;
const CONFIG_RAM_MAX_LEN: usize = 64 * 1024;

/// Reads `config.toml` fatloaded into RAM by U-Boot (`0x10F00000`), or falls back
/// to the compile-time embedded `config.toml` if not present in RAM.
fn load_config_toml_str() -> &'static str {
    let addr = kernel_common::uboot::env_get("config_addr")
        .and_then(parse_hex_env)
        .unwrap_or(CONFIG_RAM_ADDR);

    let env_size = kernel_common::uboot::env_get("config_size")
        .and_then(parse_hex_env)
        .unwrap_or(0);

    invalidate_cache_range(addr, CONFIG_RAM_MAX_LEN);

    let ptr = addr as *const u8;
    let slice = unsafe {
        if env_size > 0 && env_size <= CONFIG_RAM_MAX_LEN {
            core::slice::from_raw_parts(ptr, env_size)
        } else {
            // Scan up to first NUL byte if `mw.b 0x10F00000 0 0x10000` was used
            let mut len = 0;
            while len < CONFIG_RAM_MAX_LEN {
                let b = core::ptr::read_volatile(ptr.add(len));
                if b == 0 || b == 0xFF {
                    break;
                }
                len += 1;
            }
            core::slice::from_raw_parts(ptr, len)
        }
    };

    if !slice.is_empty() {
        if let Ok(s) = core::str::from_utf8(slice) {
            if s.contains('[') && s.contains('=') {
                tracing::info!(
                    "[boot] Loaded config.toml from SD/RAM at {:#x} ({} bytes)",
                    addr,
                    slice.len()
                );
                return s;
            }
        }
    }

    tracing::warn!(
        "[boot] No valid config.toml in RAM at {:#x}, using embedded config.toml",
        addr
    );
    include_str!("../../config.toml")
}

fn scan_for_api_sig() -> Option<usize> {
    let start_addr = 0x7b000000_usize;
    let end_addr = 0x7f000000_usize;
    let magic = b"UBootAPI";

    // We scan in 8-byte steps as structures are 8-byte aligned in RAM
    for addr in (start_addr..end_addr).step_by(8) {
        let ptr = addr as *const u8;
        unsafe {
            let mut matched = true;
            for i in 0..8 {
                if core::ptr::read_volatile(ptr.add(i)) != magic[i] {
                    matched = false;
                    break;
                }
            }
            if matched {
                if kernel_common::uboot::validate_api(addr) {
                    return Some(addr);
                }
            }
        }
    }
    None
}

#[inline]
pub fn clean_cache_range(addr: usize, len: usize) {
    let block_size = 64; // SpacemiT K1 / X60 cache line size is 64 bytes
    let start = addr & !(block_size - 1);
    let end = (addr + len + block_size - 1) & !(block_size - 1);
    let mut current = start;
    while current < end {
        unsafe {
            core::arch::asm!(
                "cbo.clean ({0})",
                in(reg) current,
                options(nostack)
            );
        }
        current += block_size;
    }
    // Also execute a memory fence to ensure the writes are ordered
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
}

#[inline]
pub fn invalidate_cache_range(addr: usize, len: usize) {
    let block_size = 64;
    let start = addr & !(block_size - 1);
    let end = (addr + len + block_size - 1) & !(block_size - 1);
    let mut current = start;
    while current < end {
        unsafe {
            // Use cbo.flush (clean + invalidate) instead of cbo.inval because
            // cbo.inval in S-mode traps if M-mode firmware leaves menvcfg.CBIE == 0,
            // whereas cbo.flush is always permitted alongside cbo.clean (CBCFE == 1).
            core::arch::asm!(
                "cbo.flush ({0})",
                in(reg) current,
                options(nostack)
            );
        }
        current += block_size;
    }
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
}


#[no_mangle]
pub static mut SECONDARY_HART_WAKE: [u32; 8] = [0; 8];

#[no_mangle]
pub extern "C" fn rust_main(
    hart_id: usize,
    _opaque: usize,
    jt_ptr: *const kernel_common::uboot::JumpTable,
) {
    // CRITICAL: Secondary harts (not Hart 1) must wait for boot to complete,
    // then grab the node from the global NODE static.
    // We do NOT use the Arc-via-opaque approach because:
    //   1. Arc::into_raw produces the same pointer for all clones (same allocation),
    //      causing Arc refcount corruption if any hart drops its copy.
    //   2. OpenSBI may not correctly relay a1 (opaque) to the target hart on all
    //      firmware versions.
    if hart_id != 1 {
        // Blind write 'S' (Secondary) upon entering rust_main
        kernel_common::uart::blind_write(b'S');

        // Wait for Hart 1 to fully complete its init AND release all global locks
        // (NODE, EXECUTOR) before secondary harts try to acquire them.
        let mut loop_cnt = 0;
        let flag_addr = &HART_DISPATCH_READY as *const _ as usize;
        while !HART_DISPATCH_READY.load(Ordering::SeqCst) {
            invalidate_cache_range(flag_addr, 8);
            core::hint::spin_loop();
            loop_cnt += 1;
            if loop_cnt % 1000000 == 0 {
                kernel_common::uart::blind_write(b'w');
            }
        }

        kernel_common::uart::blind_write(b'D'); // Done waiting


        // NOTE: We intentionally do NOT call invalidate_cache_range(bss) here.
        // cbo.inval requires menvcfg.CBIE to be set by M-mode. If OpenSBI doesn't
        // enable this for secondary harts on this SoC, the instruction traps silently.
        // The HART_DISPATCH_READY SeqCst barrier + Hart 1's clean_cache_range is
        // sufficient to guarantee BSS visibility across clusters.

        // secondary_hart_main grabs node+exec from globals AFTER trap::init().
        secondary_hart_main(hart_id);
    }

    kernel_common::uart::print(b"[boot] rust_main started\n");
    // IMPORTANT: Determine the boot hart first using a .data variable
    let prev_boot_id =
        BOOT_HART_ID.compare_exchange(999, hart_id, Ordering::SeqCst, Ordering::SeqCst);

    if prev_boot_id.is_ok() {
        kernel_common::watchdog::disable();
        let pmic_ok = kernel_common::pmic_wdt::disable();
        kernel_common::uart::print(b"[boot] pmic_wdt disable: ");
        kernel_common::uart::print(if pmic_ok {
            b"ok"
        } else {
            b"FAILED (I2C timeout)"
        });
        kernel_common::uart::print(b"\r\n");

        // We are the first hart to boot. We MUST zero the BSS section!
        // If we don't, all spinlocks and Option types will have random garbage from RAM,
        // causing infinite spins and watchdog resets!
        extern "C" {
            static mut _bss_start: u8;
            static mut _bss_end: u8;
        }
        unsafe {
            let bss_start = &raw mut _bss_start;
            let bss_end = &raw mut _bss_end;
            let len = bss_end as usize - bss_start as usize;
            core::ptr::write_bytes(bss_start, 0, len);
            clean_cache_range(bss_start as usize, len);
        }

        // Signal that BSS is ready — any hart that erroneously enters the boot
        // path (e.g. wrong hart_id delivered by OpenSBI) will not spin forever.
        BOOT_COMPLETE.store(true, Ordering::SeqCst);

        // Now that BSS is zeroed, we can safely write to UBOOT_JT (which lives in BSS)
        unsafe {
            kernel_common::uboot::init(jt_ptr);
        }
    } else {
        // Secondary harts must wait for the boot hart to finish zeroing BSS and setting up locks.
        // We can't spin on a BSS variable here, but secondary harts shouldn't reach here
        // until the boot hart wakes them up via SBI anyway.
        // Just in case U-Boot woke them up, spin.
        while !BOOT_COMPLETE.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }
    }

    if hart_id >= 8 {
        let _guard = uart::UART_LOCK.lock();
        uart::print_no_lock(b"[entry] ERROR: Invalid Hart ID\n");
        loop {}
    }

    {
        let _guard = uart::UART_LOCK.lock();
        uart::print_no_lock(b"[entry] Hart ");
        uart::putchar_no_lock(b'0' + hart_id as u8);
        uart::print_no_lock(b" entered rust_main\n");
    }

    kernel_common::uart::print(b"RM-");
    {
        let _guard = uart::UART_LOCK.lock();
        uart::putchar_no_lock(b'0' + hart_id as u8);
    }
    kernel_common::uart::print(b"\n");

    // Initialization continues...
    if prev_boot_id.is_ok() {
        // We are the first hart to boot!
        kernel_common::watchdog::disable(); // Try to disable early to prevent resets during init

        // Dynamically discover the U-Boot API address:
        // 1. Try to read the "api_address" environment variable
        // 2. Fall back to scanning the RAM area (0x7b000000 - 0x7f000000) for "UBootAPI" magic
        // 3. Fall back to the last known hardcoded address as a final resort
        let mut api_ptr = None;

        if let Some(env_ptr) = kernel_common::uboot::env_get("api_address") {
            kernel_common::uart::print(b"[boot] Found api_address in U-Boot env: ");
            // Print the environment string to console
            unsafe {
                let mut p = env_ptr;
                while *p != 0 {
                    kernel_common::uboot::putc(*p);
                    p = p.add(1);
                }
            }
            kernel_common::uart::print(b"\n");

            if let Some(parsed) = parse_hex_env(env_ptr) {
                if kernel_common::uboot::validate_api(parsed) {
                    api_ptr = Some(parsed);
                } else {
                    kernel_common::uart::print(b"[boot] env api_address fails validation!\n");
                }
            }
        }

        if api_ptr.is_none() {
            kernel_common::uart::print(b"[boot] Falling back to memory scanning...\n");
            api_ptr = scan_for_api_sig();
        }

        let final_api_ptr = match api_ptr {
            Some(addr) => {
                kernel_common::uart::print(b"[boot] Dynamically resolved U-Boot API at ");
                kernel_common::uart::print_hex(addr);
                kernel_common::uart::print(b"\n");
                addr
            }
            None => {
                let fallback = 0x7dfcae80_usize;
                kernel_common::uart::print(
                    b"[boot] WARNING: API discovery failed. Trying hardcoded fallback at ",
                );
                kernel_common::uart::print_hex(fallback);
                kernel_common::uart::print(b"\n");
                fallback
            }
        };

        unsafe {
            let sig_candidate = final_api_ptr as *const kernel_common::uboot::ApiSignature;
            if kernel_common::uboot::validate_api(final_api_ptr) {
                kernel_common::uboot::init_api(sig_candidate);
                kernel_common::uart::print(b"[boot] U-Boot API Version ");
                kernel_common::uart::print_usize((*sig_candidate).version as usize);
                kernel_common::uart::print(b" connected!\n");
            } else {
                kernel_common::uart::print(b"[boot] ERROR: U-Boot API Signature NOT valid!\n");
            }
        }
        boot_hart_main(hart_id);
    } else {
        loop {
            core::hint::spin_loop();
        }
    }
}

extern "C" {
    static _start: u8;
}

struct NodeWrapper(pub Arc<Node>);

unsafe impl Send for NodeWrapper {}
unsafe impl Sync for NodeWrapper {}

fn boot_hart_main(hart_id: usize) {
    extern "C" {
        static _bss_start: u8;
        static _bss_end: u8;
        static _stack_bottom: u8;
        static _stack_top: u8;
        static _heap_start: u8;
        static _heap_end: u8;
    }

    let heap_start = (unsafe { &_heap_start as *const u8 as usize } + 15) & !15;
    let heap_end = unsafe { &_heap_end as *const u8 as usize } & !15;
    let heap_len = heap_end - heap_start;

    if HEAP_MAGIC.load(Ordering::SeqCst) != MAGIC_VAL {
        unsafe {
            ALLOCATOR.0.lock().init(heap_start as *mut u8, heap_len);
        }
        HEAP_MAGIC.store(MAGIC_VAL, Ordering::SeqCst);
    }

    kernel_common::logger::init();

    if !TRAP_INIT_COMPLETE.swap(true, Ordering::SeqCst) {
        trap::init();
    } else {
        // Warm reboot detected. Force-unlock critical system locks and reset
        // handshake flags so secondary harts re-wait properly.
        unsafe {
            kernel_common::uart::UART_LOCK.force_unlock();
            ALLOCATOR.0.force_unlock();
            kernel_common::net::NET.force_unlock();
            NODE.force_unlock();
            kernel_common::async_rt::EXECUTOR.force_unlock();
            for i in 0..kernel_common::storage::SHARD_COUNT {
                kernel_common::storage::SHARDS[i].force_unlock();
            }
        }
        // Reset handshake flags for the new boot cycle.
        HART_DISPATCH_READY.store(false, Ordering::SeqCst);
        NODE_INIT_COMPLETE.store(false, Ordering::SeqCst);
        trap::init();
    }

    let seed = kernel_common::sbi::get_time();
    rsnano_nullable_random::set_seed(seed);

    static LEADER_ELECTED: AtomicBool = AtomicBool::new(false);
    if !LEADER_ELECTED.swap(true, Ordering::SeqCst) {
        let config_str = load_config_toml_str();
        let daemon_toml = match rsnano_node::config::DaemonToml::from_toml_str(config_str) {
            Ok(toml) => toml,
            Err(e) => {
                tracing::warn!(
                    "[boot] WARNING: SD config.toml parse error ({}), falling back to embedded config.toml",
                    e
                );
                rsnano_node::config::DaemonToml::from_toml_str(include_str!("../../config.toml"))
                    .expect("Failed to parse embedded config.toml")
            }
        };

        if let Some(level) = daemon_toml
            .bare_metal
            .as_ref()
            .and_then(|bm| bm.log_level.as_deref())
        {
            kernel_common::logger::set_log_level(level);
            tracing::info!("[boot] Log level set to '{}'", level);
        }

        tracing::info!(
            "[boot] RS-Nano Bare Metal Node starting (Hart {})",
            hart_id
        );
        tracing::debug!(
            "[boot] BSS: {:#x} - {:#x} | Stack: {:#x} - {:#x}",
            unsafe { &_bss_start as *const u8 as usize },
            unsafe { &_bss_end as *const u8 as usize },
            unsafe { &_stack_bottom as *const u8 as usize },
            unsafe { &_stack_top as *const u8 as usize }
        );
        tracing::info!(
            "[boot] Heap: {:#x} - {:#x} ({} MB)",
            heap_start,
            heap_end,
            heap_len / 1024 / 1024
        );

        kernel_common::oled::init();

        #[cfg(feature = "block-test")]
        let network = rsnano_core::Networks::NanoDevNetwork;
        #[cfg(not(feature = "block-test"))]
        let network = daemon_toml.network_or(rsnano_core::Networks::NanoLiveNetwork);
        tracing::info!("[boot] Using network: {}", network.as_str());

        let mut daemon_config = rsnano_node::config::DaemonConfig::new2(network, 8);
        daemon_config.rpc_enable = true;
        daemon_config.merge_toml(&daemon_toml);

        // Apply bare-metal network configuration before initializing U-Boot networking
        {
            let mut net_cfg = kernel_common::net::get_net_config();
            net_cfg.peering_port = daemon_config.node.network.listening_port;
            net_cfg.rpc_port = daemon_config.rpc.port;
            net_cfg.rpc_enable = daemon_config.rpc_enable;
            net_cfg.rpc_enable_control = daemon_config.rpc.enable_control;

            if let Some(bm) = &daemon_toml.bare_metal {
                if let Some(mac_str) = &bm.mac_address {
                    if let Some(mac) = parse_mac_str(mac_str) {
                        net_cfg.mac = mac;
                    }
                }
                if let Some(ip_str) = &bm.ipv4_address {
                    if let Ok(ip) = core::net::Ipv4Addr::from_str(ip_str) {
                        net_cfg.ipv4 = ip.octets();
                    }
                }
                if let Some(prefix) = bm.ipv4_prefix_len {
                    net_cfg.prefix_len = prefix;
                }
                if let Some(gw_str) = &bm.gateway {
                    if let Ok(gw) = core::net::Ipv4Addr::from_str(gw_str) {
                        net_cfg.gateway = gw.octets();
                    }
                }
                if let Some(dns_str) = &bm.dns_server {
                    if let Ok(dns) = core::net::Ipv4Addr::from_str(dns_str) {
                        net_cfg.dns_server = dns.octets();
                    }
                }
            }
            kernel_common::net::set_net_config(net_cfg);
        }

        tracing::debug!("[boot] net init...");
        let net_ctx = kernel_common::net::init_uboot_net();
        tracing::debug!("[boot] net init done");
        if let Some(ctx) = net_ctx {
            *kernel_common::net::NET.lock() = Some(ctx);
        }

        kernel_common::storage::init_uboot_blk();

        let wipe_requested = daemon_toml
            .bare_metal
            .as_ref()
            .and_then(|bm| bm.wipe_blocks)
            .unwrap_or(false);

        #[cfg(feature = "block-test")]
        let wipe_requested = true;

        if wipe_requested {
            tracing::warn!("[storage] Wiping all B-Tree shards to start from zero...");
            for i in 0..kernel_common::storage::SHARD_COUNT {
                if let Some(db) = kernel_common::storage::SHARDS[i].lock().as_mut() {
                    db.wipe();
                }
            }
            kernel_common::storage::flush_all_shards();
            tracing::info!("[storage] All B-Tree shards wiped and flushed to NVMe.");
        }

        let exec = kernel_common::async_rt::Executor::new();
        kernel_common::async_rt::set_global_executor(exec.clone());

        let mut config = daemon_config.node;
        config.bootstrap.max_requests = config.bootstrap.max_requests.min(4096);
        config.bootstrap.channel_limit = config.bootstrap.channel_limit.min(64);
        config.bootstrap.rate_limit = config.bootstrap.rate_limit.max(1500).min(10000);
        config.bootstrap.frontier_rate_limit = config.bootstrap.frontier_rate_limit.max(16).min(64);
        config.bootstrap.frontier_scan.cooldown = core::time::Duration::from_millis(250);
        config.bootstrap.request_timeout = config
            .bootstrap
            .request_timeout
            .min(core::time::Duration::from_secs(15));
        config.priority_bucket.reserved_elections = config.priority_bucket.reserved_elections.max(250);
        config.priority_bucket.max_elections = config.priority_bucket.max_elections.max(1500);
        config.optimistic_scheduler.optimistic_limit_percentage =
            config.optimistic_scheduler.optimistic_limit_percentage.max(50);
        let preconfigured_peers = config.preconfigured_peers.clone();

        let mut flags = rsnano_node::config::NodeFlags::new();
        flags.enable_pruning = daemon_toml
            .node
            .as_ref()
            .and_then(|n| n.experimental.as_ref())
            .and_then(|e| e.enable_pruning)
            .unwrap_or(false);
        if flags.enable_pruning && config.enable_voting {
            tracing::warn!(
                "[boot] Notice: disabling enable_voting because enable_pruning is enabled"
            );
            config.enable_voting = false;
        }
        flags.enable_voting = config.enable_voting;

        let event_handlers = alloc::sync::Arc::new(spin::Mutex::new(
            rsnano_node::CompositeNodeEventHandler::default(),
        ));
        let event_sink = {
            let eh = event_handlers.clone();
            alloc::sync::Arc::new(move |event| {
                eh.lock().handle(&event);
            })
        };

        let mut node = rsnano_node::NodeBuilder::new(network)
            .resolver(alloc::sync::Arc::new(BareMetalDnsResolver))
            .event_sink(event_sink)
            .flags(flags.clone())
            .config(config)
            .finish()
            .expect("Failed to initialize node");

        let websocket_server = rsnano_websocket_server::create_websocket_server(
            node.config.websocket_config.clone(),
            &node,
            &mut event_handlers.lock(),
        );

        tracing::debug!("[boot] node.start() starting...");
        node.start();
        if let Some(ws) = &websocket_server {
            use rsnano_websocket_server::WebsocketListenerExt;
            ws.start();
            tracing::info!(
                "[boot] websocket server started on port {}",
                ws.listening_port()
            );
        }
        tracing::info!("[boot] node.start() completed!");

        // Connect immediately and persist any IPv4 preconfigured_peers from config.toml
        #[cfg(not(feature = "block-test"))]
        for peer in &preconfigured_peers {
            if let Ok(ip) = core::net::Ipv4Addr::from_str(&peer.address) {
                tracing::debug!("[boot] adding peer {}:{}...", peer.address, peer.port);
                let addr_v6 = ip.to_ipv6_mapped();
                let socket_addr = core::net::SocketAddrV6::new(addr_v6, peer.port, 0, 0);

                node.ledger
                    .store
                    .peer
                    .put(socket_addr, rsnano_nullable_clock::SystemTime::now());

                let _ = node.peer_connector.connect_to(socket_addr);
            }
        }
        #[cfg(feature = "block-test")]
        let _ = &preconfigured_peers;

        let node_arc = alloc::sync::Arc::new(node);
        {
            *NODE.lock() = Some(NodeWrapper(node_arc.clone()));
        }

        kernel_common::storage::flush_all_shards();

        #[cfg(feature = "block-test")]
        run_deterministic_block_test(&node_arc, &exec);

        // Signal secondary harts that the node is ready BEFORE starting them,
        // so harts that boot quickly don't spin past the flag before it's set.
        NODE_INIT_COMPLETE.store(true, Ordering::SeqCst);

        // On the SpacemiT K1 multi-cluster design, RISC-V fence is NOT sufficient for
        // cross-cluster cache visibility. We must flush all shared state (statics, locks,
        // AND heap contents) from Cluster 0's cache to DRAM BEFORE waking Cluster 1 harts.
        //
        // CRITICAL: Arc<T> inner data (e.g. Arc<Mutex<BootstrapState>>) lives in the HEAP,
        // not in BSS. Without flushing the heap before secondary harts dereference Arc<T>,
        // Cluster 1 harts can read stale DRAM lines.
        let start_addr = unsafe { &_start as *const u8 as usize };
        clean_cache_range(start_addr, 3 * 1024 * 1024);

        {
            let heap = ALLOCATOR.0.lock();
            let heap_start = heap.bottom() as usize;
            let heap_used = heap.used();
            drop(heap);
            if heap_used > 0 {
                clean_cache_range(heap_start, heap_used);
            }
        }

        // Signal secondary harts: all state is ready; flush BSS and flag to DRAM.
        HART_DISPATCH_READY.store(true, Ordering::SeqCst);
        unsafe {
            let bss_start = &_bss_start as *const u8 as usize;
            let bss_end = &_bss_end as *const u8 as usize;
            let len = bss_end - bss_start;
            clean_cache_range(bss_start, len);
        }
        let flag_addr = &HART_DISPATCH_READY as *const _ as usize;
        clean_cache_range(flag_addr, 8);

        tracing::info!("[boot] Starting secondary harts...");

        for id in 0..8 {
            if id != hart_id {
                if id == 0 {
                    tracing::debug!(
                        "[boot] Skipping Hart 0 (known to hang OpenSBI on this SoC)"
                    );
                    continue;
                }

                let (initial_err, initial_status) = kernel_common::sbi::hart_get_status(id);
                tracing::debug!(
                    "[boot] Starting Hart {} (initial err={}, status={})...",
                    id,
                    initial_err,
                    initial_status
                );

                let (err, _) = kernel_common::sbi::hart_start(id, start_addr, 0);

                // Workaround for SpacemiT K1 OpenSBI:
                // The firmware might fail to send the wakeup IPI internally,
                // leaving the hart stuck in START_PENDING state.
                // We send a manual IPI to force it to wake up.
                kernel_common::sbi::send_ipi(1 << id, 0);

                // Wait up to ~1s for it to leave START_PENDING (status=2)
                let mut status;
                let mut err_status;
                let start_mtime = kernel_common::clint::mtime();
                loop {
                    let (es, st) = kernel_common::sbi::hart_get_status(id);
                    err_status = es;
                    status = st;
                    if status != 2 {
                        break;
                    }
                    if kernel_common::clint::mtime() - start_mtime > 100_000_000 {
                        break;
                    }
                    core::hint::spin_loop();
                }

                let wake_addr = unsafe { &SECONDARY_HART_WAKE[id] as *const u32 as usize };
                invalidate_cache_range(wake_addr, 4);
                let wake_val = unsafe { core::ptr::read_volatile(&SECONDARY_HART_WAKE[id]) };

                tracing::debug!(
                    "[boot] Hart {} start result: err={}, get_status_err={}, status={}, wake={}",
                    id,
                    err,
                    err_status,
                    status,
                    wake_val
                );
            }
        }
    } else {
        // Other harts wait for the leader to finish node setup
        while !NODE_INIT_COMPLETE.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }
    }

    let node_arc = {
        let _intr = kernel_common::uart::InterruptGuard::new();
        let lock = NODE.lock();
        lock.as_ref().expect("Node not initialized").0.clone()
    };

    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .as_ref()
        .expect("Executor not initialized")
        .clone();

    tracing::debug!("[boot] Entering task loop for Hart {}...", hart_id);

    dispatch_hart_task(hart_id, node_arc, exec);
}

fn secondary_hart_main(hart_id: usize) -> ! {
    // Initialize trap vector for this hart BEFORE touching any global locks.
    kernel_common::trap::init();

    // Hart 1 has flushed kernel, heap, and BSS to DRAM and released all global locks
    // before setting HART_DISPATCH_READY.
    let node_arc = {
        let lock = NODE.lock();
        lock.as_ref()
            .expect("[PANIC] Node not initialized on secondary hart")
            .0
            .clone()
    };
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .as_ref()
        .expect("[PANIC] Executor not initialized on secondary hart")
        .clone();

    // Diagnostic E: We acquired the globals and are about to start the task loop
    kernel_common::uart::blind_write(b'E');

    dispatch_hart_task(hart_id, node_arc, exec);
}

fn dispatch_hart_task(
    hart_id: usize,
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
) -> ! {
    match hart_id {
        1 => run_maintenance_and_network_loop(node_arc, exec, hart_id),
        2 => run_crypto_worker_1_loop(node_arc, exec, hart_id),
        3 => run_crypto_worker_2_loop(node_arc, exec, hart_id),
        4 => run_consensus_and_elections_loop(node_arc, exec, hart_id),
        5 => run_vote_processor_loop(node_arc, exec, hart_id),
        6 => run_ledger_commit_and_bootstrap_loop(node_arc, exec, hart_id),
        7 => run_storage_flush_and_monitor_loop(node_arc, exec, hart_id),
        _ => {
            tracing::warn!(
                "[boot] Hart {}: Reserved/unassigned, entering parking loop",
                hart_id
            );
            loop {
                core::hint::spin_loop();
            }
        }
    }
}

#[inline(always)]
fn adaptive_pause_backoff(worked: bool, idle_spins: &mut u32) {
    kernel_common::uboot::eth_check_tx_stall();
    if worked {
        *idle_spins = 1;
    } else {
        for _ in 0..*idle_spins {
            // Compiles to the RISC-V `pause` instruction (0x0100000F) via `+zihintpause`,
            // delaying subsequent instruction decode and suppressing interconnect thrashing.
            core::hint::spin_loop();
        }
        if *idle_spins < 64 {
            *idle_spins <<= 1;
        }
    }
}

fn run_consensus_and_elections_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 4: Consensus & Active Elections loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_aec_voter_tick = kernel_common::clint::mtime();
    let mut iterations = 0;
    let mut idle_spins = 1u32;
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        // 1. Drive AEC voter and ticker every 300ms (matching RSNano's aec_loop_interval)
        if mtime - last_aec_voter_tick > millis(300) {
            let mut aec_voter = node_arc.aec_voter.lock();
            let mut aec_ticker = node_arc.aec_ticker.lock();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            rsnano_core::utils::Tickable::tick(&mut *aec_voter, &cancel_token);
            rsnano_core::utils::Tickable::tick(&mut *aec_ticker, &cancel_token);
            last_aec_voter_tick = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 4 (Consensus) | Iters: {} | Active: {}",
                iterations,
                node_arc.active.read().len()
            );
            last_heartbeat = mtime;
            iterations = 0;
        }

        let mut worked = false;
        if node_arc.vote_generators.voting_enabled() {
            if node_arc.request_aggregator.run_once() {
                worked = true;
            }
            if node_arc.vote_generators.run_once() {
                worked = true;
            }
        }
        for _ in 0..4 {
            if exec.run_once(hart_id) {
                worked = true;
            } else {
                break;
            }
        }
        if !node_arc.signature_checker.is_empty_relaxed() {
            if node_arc.signature_checker.help_verify() > 0 {
                worked = true;
            }
        }

        adaptive_pause_backoff(worked, &mut idle_spins);
        iterations += 1;
    }
}

fn run_vote_processor_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 5: Vote Processor loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;
    let mut idle_spins = 1u32;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 5 (VoteProc) | Iters: {}",
                iterations
            );
            last_heartbeat = mtime;
            iterations = 0;
        }

        let mut worked = false;
        if node_arc.vote_cache_processor.run_once() {
            worked = true;
        }
        if node_arc.vote_processor.run_once() {
            worked = true;
        }
        if node_arc.config.enable_vote_rebroadcast && node_arc.vote_rebroadcaster.run_once() {
            worked = true;
        }
        for _ in 0..4 {
            if exec.run_once(hart_id) {
                worked = true;
            } else {
                break;
            }
        }
        if !node_arc.signature_checker.is_empty_relaxed() {
            if node_arc.signature_checker.help_verify() > 0 {
                worked = true;
            }
        }
        adaptive_pause_backoff(worked, &mut idle_spins);
        iterations += 1;
    }
}

fn run_crypto_worker_1_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 2: Crypto Worker 1 (SIMD Verify) loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;
    let mut verified_count = 0usize;
    let mut idle_spins = 1u32;
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 2 (Crypto-1) | Iters: {} | Verified: {}",
                iterations,
                verified_count
            );
            iterations = 0;
            last_heartbeat = mtime;
        }

        let mut worked = false;
        // 1. High-priority synchronous signature batches waiting on Hart 6
        while node_arc.signature_checker.has_sync_tasks() {
            let n = node_arc.signature_checker.help_verify();
            if n > 0 {
                worked = true;
                verified_count = verified_count.wrapping_add(n);
            } else {
                break;
            }
        }

        // 2. Drain inbound network messages into BlockProcessorQueue & bg_tasks without starving TCP
        if !node_arc.inbound_message_queue.is_empty_relaxed() {
            let hp1 = kernel_common::clint::mtime();
            for _ in 0..4 {
                if rsnano_node::transport::MessageProcessor::run_batch(&node_arc.message_processor) {
                    worked = true;
                } else {
                    break;
                }
            }
            HART1_PROF[1].fetch_add(kernel_common::clint::mtime() - hp1, Ordering::Relaxed);
        }

        // 3. Background signature pre-verification (interleaved with sync & message checks)
        if !node_arc.signature_checker.is_empty_relaxed() {
            for _ in 0..4 {
                let n = node_arc.signature_checker.help_verify();
                if n > 0 {
                    worked = true;
                    verified_count = verified_count.wrapping_add(n);
                } else {
                    break;
                }
            }
        }

        let _ = (&exec, hart_id);

        adaptive_pause_backoff(worked, &mut idle_spins);
        iterations += 1;
    }
}

fn run_crypto_worker_2_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 3: Crypto Worker 2 (SIMD Verify) loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;
    let mut verified_count = 0usize;
    let mut idle_spins = 1u32;
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 3 (Crypto-2) | Iters: {} | Verified: {}",
                iterations,
                verified_count
            );
            iterations = 0;
            last_heartbeat = mtime;
        }

        let mut worked = false;
        // 1. High-priority synchronous signature batches waiting on Hart 6
        while node_arc.signature_checker.has_sync_tasks() {
            let n = node_arc.signature_checker.help_verify();
            if n > 0 {
                worked = true;
                verified_count = verified_count.wrapping_add(n);
            } else {
                break;
            }
        }

        // 2. Drain inbound network messages into BlockProcessorQueue & bg_tasks without starving TCP
        if !node_arc.inbound_message_queue.is_empty_relaxed() {
            let hp1 = kernel_common::clint::mtime();
            for _ in 0..4 {
                if rsnano_node::transport::MessageProcessor::run_batch(&node_arc.message_processor) {
                    worked = true;
                } else {
                    break;
                }
            }
            HART1_PROF[1].fetch_add(kernel_common::clint::mtime() - hp1, Ordering::Relaxed);
        }

        // 3. Background signature pre-verification (interleaved with sync & message checks)
        if !node_arc.signature_checker.is_empty_relaxed() {
            for _ in 0..4 {
                let n = node_arc.signature_checker.help_verify();
                if n > 0 {
                    worked = true;
                    verified_count = verified_count.wrapping_add(n);
                } else {
                    break;
                }
            }
        }

        let _ = (&exec, hart_id);

        adaptive_pause_backoff(worked, &mut idle_spins);
        iterations += 1;
    }
}

fn is_http_request_complete(req: &str) -> bool {
    let Some((headers, body)) = req.split_once("\r\n\r\n").or_else(|| req.split_once("\n\n")) else {
        return false;
    };
    if headers.starts_with("GET ") {
        return true;
    }
    for line in headers.lines() {
        let lower = line.trim();
        if lower.len() >= 15 && lower.as_bytes()[..15].eq_ignore_ascii_case(b"content-length:") {
            if let Ok(expected_len) = lower[15..].trim().parse::<usize>() {
                return body.len() >= expected_len;
            }
        }
    }
    let trimmed = body.trim();
    !trimmed.is_empty() && trimmed.ends_with('}')
}

static RPC_ACTIVE_SINCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn handle_rpc_server(ctx: &mut kernel_common::net::NetworkContext, node: Arc<Node>) {
    let net_cfg = kernel_common::net::get_net_config();
    if !net_cfg.rpc_enable {
        return;
    }
    use smoltcp::socket::tcp::{Socket as SmolTcpSocket, State as SmolTcpState};
    let socket = ctx.sockets.get_mut::<SmolTcpSocket>(ctx.rpc_handle);
    let now = kernel_common::clint::mtime();
    match socket.state() {
        SmolTcpState::Closed => {
            RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
            let _ = socket.listen(net_cfg.rpc_port);
        }
        SmolTcpState::Listen => {
            RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
        }
        SmolTcpState::TimeWait | SmolTcpState::CloseWait | SmolTcpState::LastAck | SmolTcpState::Closing => {
            RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
            socket.abort();
            let _ = socket.listen(net_cfg.rpc_port);
        }
        SmolTcpState::SynReceived => {
            let since = RPC_ACTIVE_SINCE.load(Ordering::Relaxed);
            if since == 0 {
                RPC_ACTIVE_SINCE.store(now, Ordering::Relaxed);
            } else if now.wrapping_sub(since) > seconds(5) {
                RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
                socket.abort();
                let _ = socket.listen(net_cfg.rpc_port);
            }
        }
        SmolTcpState::Established => {
            let since = RPC_ACTIVE_SINCE.load(Ordering::Relaxed);
            if since == 0 {
                RPC_ACTIVE_SINCE.store(now, Ordering::Relaxed);
            } else if now.wrapping_sub(since) > seconds(5) {
                RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
                socket.abort();
                let _ = socket.listen(net_cfg.rpc_port);
                return;
            }

            if socket.can_recv() {
                let mut response: Option<alloc::string::String> = None;
                let enable_control = net_cfg.rpc_enable_control;
                let _ = socket.recv(|buffer| {
                    let request = core::str::from_utf8(buffer).unwrap_or("");
                    if !is_http_request_complete(request) && buffer.len() < 60_000 {
                        return (0, ());
                    }
                    let handler = RpcCommandHandler::new(node.clone(), enable_control);
                    let server = BareMetalRpcServer::new(handler);

                    if let Some(resp_str) = server.handle_request(request) {
                        response = Some(resp_str);
                    } else {
                        response = Some(alloc::string::String::from(
                            "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n",
                        ));
                    }
                    (buffer.len(), ())
                });
                if let Some(resp_str) = response {
                    RPC_ACTIVE_SINCE.store(0, Ordering::Relaxed);
                    let _ = socket.send_slice(resp_str.as_bytes());
                    socket.close();
                }
            }
        }
        _ => {}
    }
}

struct BareMetalDnsResolver;
impl rsnano_network::AsyncDnsResolver for BareMetalDnsResolver {
    fn resolve(
        &self,
        host: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<alloc::vec::Vec<core::net::IpAddr>>> + Send>>
    {
        if let Ok(ip) = core::net::IpAddr::from_str(host) {
            return Box::pin(async move { Ok(alloc::vec![ip]) });
        }
        let host_owned = String::from(host);
        Box::pin(async move {
            match kernel_common::net::resolve(&host_owned).await {
                Ok(addrs) => {
                    let mut ips = alloc::vec::Vec::new();
                    for addr in addrs {
                        match addr {
                            smoltcp::wire::IpAddress::Ipv4(a) => {
                                let b: [u8; 4] = unsafe { core::mem::transmute(a) };
                                ips.push(core::net::IpAddr::V4(core::net::Ipv4Addr::new(
                                    b[0], b[1], b[2], b[3],
                                )));
                            }
                            smoltcp::wire::IpAddress::Ipv6(a) => {
                                let b: [u8; 16] = unsafe { core::mem::transmute(a) };
                                ips.push(core::net::IpAddr::V6(core::net::Ipv6Addr::from(b)));
                            }
                        }
                    }
                    Ok(ips)
                }
                Err(_) => Err(anyhow::anyhow!("DNS resolution failed")),
            }
        })
    }
}

fn run_storage_flush_and_monitor_loop(
    node_mon: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 7: Storage Flush & Monitor loop enter");
    let mut last_tick = kernel_common::clint::mtime();
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_oled = kernel_common::clint::mtime();
    let mut last_cleanup = kernel_common::clint::mtime();
    let mut last_online_weight_tick = kernel_common::clint::mtime();
    let mut last_stored_count = node_mon.ledger.block_count();
    let mut last_confirmed_count = node_mon.ledger.confirmed_count();
    let mut last_oled_stored = last_stored_count;
    let mut shard_pages = [0usize; kernel_common::storage::SHARD_COUNT];
    let mut shard_free = [0usize; kernel_common::storage::SHARD_COUNT];
    let mut shard_cached = [0usize; kernel_common::storage::SHARD_COUNT];
    {
        let shard_max = kernel_common::storage::SHARD_SIZE_PAGES.load(Ordering::Relaxed);
        for i in 0..kernel_common::storage::SHARD_COUNT {
            if let Some(mut btree_guard) = kernel_common::storage::SHARDS[i].try_lock() {
                if let Some(btree) = btree_guard.as_mut() {
                    shard_pages[i] = btree.page_count();
                    shard_free[i] = btree.capacity_remaining(shard_max) + btree.free_page_count();
                    shard_cached[i] = btree.bpm.cached_page_count();
                }
            }
        }
    }
    let mut cached_db_kb: usize = shard_pages.iter().sum::<usize>() * 4;
    let mut iterations = 0;

    // Initial OLED status render right after boot completes
    {
        let peers = node_mon.network.try_read().map(|n| n.channels_info().total).unwrap_or(0);
        kernel_common::oled::update_status(
            peers,
            last_stored_count as usize,
            last_confirmed_count as usize,
            0,
            cached_db_kb,
        );
    }

    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_online_weight_tick > seconds(2) {
            if let Some(mut guard) = node_mon.online_weight_calculation.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                guard.tick(&cancel_token);
            }
            last_online_weight_tick = mtime;
        }
        let _ = &mut last_cleanup;

        // 1. Persistence is owned by the ledger writer (Hart 6): it checkpoints all shards
        // atomically through the WAL at transaction boundaries. Nothing may flush shards directly.

        // 2. Hardware OLED status refresh
        if mtime - last_oled > seconds(2) {
            let stored_now = node_mon.ledger.block_count();
            let confirmed_now = node_mon.ledger.confirmed_count();
            let oled_bps = stored_now.saturating_sub(last_oled_stored) / 2;
            last_oled_stored = stored_now;
            let peers = node_mon.network.try_read().map(|n| n.channels_info().total).unwrap_or(0);
            kernel_common::oled::update_status(
                peers,
                stored_now as usize,
                confirmed_now as usize,
                oled_bps as usize,
                cached_db_kb,
            );
            last_oled = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 7 (Storage+Mon) | Iters: {}",
                iterations
            );
            last_heartbeat = mtime;
            iterations = 0;
        }

        if mtime - last_tick > seconds(15) {
            let shard_max = kernel_common::storage::SHARD_SIZE_PAGES.load(Ordering::Relaxed);
            for i in 0..kernel_common::storage::SHARD_COUNT {
                if let Some(mut btree_guard) = kernel_common::storage::SHARDS[i].try_lock() {
                    if let Some(btree) = btree_guard.as_mut() {
                        shard_pages[i] = btree.page_count();
                        shard_free[i] = btree.capacity_remaining(shard_max) + btree.free_page_count();
                        shard_cached[i] = btree.bpm.cached_page_count();
                    }
                }
            }
            let total_pages: usize = shard_pages.iter().sum();
            let free_pages: usize = shard_free.iter().sum();
            let cached_pages: usize = shard_cached.iter().sum();
            let db_kb = total_pages * 4;
            let cache_mb = (cached_pages * 4) / 1024;
            cached_db_kb = db_kb;

            let used = ALLOCATOR.0.lock().used();
            let slab_mb = InterruptSafeAllocator::slab_cached_bytes() / 1024 / 1024;
            let stored_count = node_mon.ledger.block_count();
            let confirmed_count = node_mon.ledger.confirmed_count();

            let bps = stored_count.saturating_sub(last_stored_count) / 15;
            let _cps = confirmed_count.saturating_sub(last_confirmed_count) / 15;
            last_stored_count = stored_count;
            last_confirmed_count = confirmed_count;

            let channels_info = node_mon.network.try_read().map(|n| n.channels_info()).unwrap_or_default();
            let online_stake = node_mon.online_reps.try_lock().map(|r| r.online_weight()).unwrap_or_default();
            let stake_nano = (online_stake.number() / (10u128.pow(30))) as usize;

            if db_kb >= 1024 {
                tracing::info!(
                    "[monitor] Heap: {} MB (slab-cached {} MB) | Cache: {} MB | DB: {} MB ({} free) | Peers: {} ({} out) | Stake: {} | Rate: {} bps",
                    used / 1024 / 1024,
                    slab_mb,
                    cache_mb,
                    db_kb / 1024,
                    free_pages,
                    channels_info.total,
                    channels_info.outbound,
                    stake_nano,
                    bps
                );
            } else {
                tracing::info!(
                    "[monitor] Heap: {} MB (slab-cached {} MB) | Cache: {} MB | DB: {} KB ({} free) | Peers: {} ({} out) | Stake: {} | Rate: {} bps",
                    used / 1024 / 1024,
                    slab_mb,
                    cache_mb,
                    db_kb,
                    free_pages,
                    channels_info.total,
                    channels_info.outbound,
                    stake_nano,
                    bps
                );
            }

            if tracing::enabled!(tracing::Level::DEBUG) {
                log_block_prof_debug();
                {
                    let mp = &rsnano_node::transport::MSG_PROF;
                    let ms = |idx: usize| (mp[7 + idx].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
                    let cnt = |idx: usize| mp[idx].load(Ordering::Relaxed) as usize;
                    tracing::debug!(
                        "   [msgprof] ascack={}/{}ms publish={}/{}ms confack={}/{}ms confreq={}/{}ms keepalive={}/{}ms ascreq={}/{}ms other={}/{}ms",
                        cnt(0), ms(0),
                        cnt(1), ms(1),
                        cnt(2), ms(2),
                        cnt(3), ms(3),
                        cnt(4), ms(4),
                        cnt(5), ms(5),
                        cnt(6), ms(6),
                    );
                }
                {
                    let st = node_mon.bootstrapper.get_state();
                    if let Some(g) = st.try_lock() {
                        let (rq, chans, inflight, at_limit) = g.diag_summary();
                        tracing::debug!(
                            "   [bsets] priority={} blocked={} running_queries={} channels={} inflight={} at_limit={}",
                            g.candidate_accounts.priority_len(),
                            g.candidate_accounts.blocked_len(),
                            rq,
                            chans,
                            inflight,
                            at_limit
                        );
                    };
                }
                let stats_col = node_mon.stats();
                {
                    static CINFO_TICK: AtomicUsize = AtomicUsize::new(0);
                    if CINFO_TICK.fetch_add(1, Ordering::Relaxed) % 4 == 0 {
                        let json = node_mon.container_info().into_json();
                        tracing::debug!("   [cinfo] {}", json);
                    }
                }
                {
                    use core::fmt::Write;
                    let mut bbuf = String::new();
                    for (key, val) in stats_col.iter() {
                        if *val > 0 && key.stat.starts_with("boot") {
                            if !bbuf.is_empty() {
                                bbuf.push_str(" | ");
                            }
                            let _ = write!(bbuf, "{}.{}={}", key.stat, key.detail, val);
                        }
                    }
                    if !bbuf.is_empty() {
                        tracing::debug!("   [bstats] {}", bbuf);
                    }
                }
                {
                    static VSTATS_TICK: AtomicUsize = AtomicUsize::new(0);
                    if VSTATS_TICK.fetch_add(1, Ordering::Relaxed) % 4 == 0 {
                        use core::fmt::Write;
                        let mut vbuf = String::new();
                        for (key, val) in stats_col.iter() {
                            let st = key.stat;
                            if *val > 0
                                && (st.contains("vote")
                                    || st.contains("election")
                                    || st.contains("confirm")
                                    || st.contains("rep_crawler"))
                            {
                                if !vbuf.is_empty() {
                                    vbuf.push_str(" | ");
                                }
                                let _ = write!(vbuf, "{}.{}={}", st, key.detail, val);
                            }
                        }
                        if !vbuf.is_empty() {
                            tracing::debug!("   [vstats] {}", vbuf);
                        }
                    }
                }
                {
                    use core::fmt::Write;
                    let mut sbuf = String::new();
                    for (key, val) in stats_col.iter() {
                        if (key.stat == "block_processor_result"
                            || key.stat == "block_processor_source")
                            && *val > 0
                        {
                            if !sbuf.is_empty() {
                                sbuf.push_str(" | ");
                            }
                            let _ = write!(sbuf, "{}: {}", key.detail, val);
                        }
                    }
                    if !sbuf.is_empty() {
                        tracing::debug!("   [stats] {}", sbuf);
                    }
                }
            }

            node_mon.tick_monitor();
            DB_PAGES_FREE.store(free_pages, Ordering::Relaxed);
            last_tick = mtime;
        }

        let mut worked_exec = false;
        for _ in 0..32 {
            if exec.run_once(hart_id) {
                worked_exec = true;
            } else {
                break;
            }
        }

        if !worked_exec {
            if !node_mon.signature_checker.is_empty_relaxed()
                && node_mon.signature_checker.help_verify() > 0
            {
                iterations += 1;
                continue;
            }
            // Aggressive throttle when completely idle to free up system bus
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
        }
        iterations += 1;
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let hart_id: usize;
    unsafe {
        core::arch::asm!("mv {0}, tp", out(reg) hart_id);
    }
    loop {
        uart::print(b"[PANIC] hart=");
        uart::print_usize(hart_id);
        uart::print(b" system panicked");
        if let Some(loc) = info.location() {
            uart::print(b" at ");
            uart::print(loc.file().as_bytes());
            uart::print(b":");
            uart::print_usize(loc.line() as usize);
        }
        uart::print(b"\n");
        use core::fmt::Write;
        let _ = write!(kernel_common::uart::UartWriter, "Message: {}\n", info.message());
        let t0 = kernel_common::clint::mtime();
        while kernel_common::clint::mtime().wrapping_sub(t0) < seconds(15) {
            core::hint::spin_loop();
        }
    }
}

/// Logs block-processor and storage stage profiling at DEBUG level.
fn log_block_prof_debug() {
    let bp = &rsnano_node::block_processing::BLOCK_PROF;
    let bp_ms = |i: usize| (bp[i].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
    let bp_n = |i: usize| bp[i].load(Ordering::Relaxed) as usize;
    tracing::debug!(
        "[prof] sig_ms={} ledger_ms={} post_ms={} batches={} blocks={} verified={} send_ms={} loop_ms={}",
        bp_ms(0), bp_ms(1), bp_ms(2), bp_n(3), bp_n(4), bp_n(5), bp_ms(6), bp_ms(7)
    );

    let sp = &kernel_common::storage::bpm::STORAGE_PROF;
    let sp_ms = |i: usize| (sp[i].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
    let sp_n = |i: usize| sp[i].load(Ordering::Relaxed) as usize;
    tracing::debug!(
        "[sprof] fetch_n={} fetch_ms={} read_n={} read_ms={} write_n={} write_ms={} search_n={} search_ms={} insert_n={} insert_ms={}",
        sp_n(0), sp_ms(1), sp_n(2), sp_ms(3), sp_n(4), sp_ms(5), sp_n(6), sp_ms(7), sp_n(8), sp_ms(9)
    );
    tracing::debug!(
        "[bexists] lock_wait_ms={} search_ms={} calls={}",
        sp_ms(10), sp_ms(11), sp_n(12)
    );

    for h in 0..8 {
        let acq = kernel_common::storage::LOCK_ACQ[h].load(Ordering::Relaxed);
        if acq > 0 {
            let hold_ms = (kernel_common::storage::LOCK_HOLD[h].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
            let wait_ms = (kernel_common::storage::LOCK_WAIT[h].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
            tracing::debug!(
                "[shardlock] hart={} acquires={} hold_ms={} wait_ms={}",
                h, acq, hold_ms, wait_ms
            );
        }
    }
    if let Some(t) = kernel_common::storage::LONG_HOLDS.try_lock() {
        for e in t.iter().flatten() {
            tracing::debug!(
                "[longhold] hart={} {}:{} total_ms={} count={}",
                e.0,
                e.1.file(),
                e.1.line(),
                (e.2 * 1000 / CLINT_FREQ) as usize,
                e.3
            );
        }
    }

    let lp = &rsnano_ledger::LEDGER_PROF;
    let lp_ms = |i: usize| (lp[i].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize;
    tracing::debug!(
        "[lprof] create_validator_ms={} validate_ms={} insert_ms={} prev_block_ms={} source_exists_ms={} get_pending_ms={} hash_exists_ms={} get_account_ms={} receivable_exists_ms={} now_ms={}",
        lp_ms(0), lp_ms(1), lp_ms(2), lp_ms(3), lp_ms(4), lp_ms(5), lp_ms(6), lp_ms(7), lp_ms(8), lp_ms(9)
    );

    let ws = &kernel_common::storage::wal::WAL_STATS;
    tracing::debug!(
        "[wal] checkpoints={} wal_pages={} applied_pages={} ckpt_ms={}",
        ws[0].load(Ordering::Relaxed),
        ws[1].load(Ordering::Relaxed),
        ws[2].load(Ordering::Relaxed),
        (ws[3].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize
    );
}

fn run_ledger_commit_and_bootstrap_loop(
    node_arc: Arc<Node>,
    _exec: kernel_common::async_rt::Executor,
    _hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 6: Single-Writer Ledger Commit & Bootstrap loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_node_tick = kernel_common::clint::mtime();
    let mut last_scheduler_tick = kernel_common::clint::mtime();
    let mut last_checkpoint = kernel_common::clint::mtime();
    let mut iterations = 0;
    let mut idle_spins = 1u32;
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::heartbeat_hart6(mtime);
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            tracing::debug!(
                "[heartbeat] Hart 6 (Ledger+Boot) | Iters: {} | Total Blocks: {} | BPQ: {}",
                iterations,
                node_arc.ledger.block_count(),
                node_arc.block_processor.total_queue_len()
            );
            iterations = 0;
            last_heartbeat = mtime;
        }

        let mut worked = false;

        // 1. Single-writer block insertion & dependencies
        // Throttle new block commits when unconfirmed backlog exceeds 250,000 so
        // ConfirmingSet and ElectionSchedulers can drain the backlog without destructive rollbacks.
        if node_arc.ledger.backlog_count() < 250_000 {
            if node_arc.unchecked.buffer_count() > 0 {
                node_arc.unchecked.run_once();
                worked = true;
            }
            for _ in 0..8 {
                let mut batch_worked = false;
                if node_arc.block_processor.total_queue_len() > 0 {
                    node_arc.block_processor.run_once();
                    worked = true;
                    batch_worked = true;
                }
                if node_arc.unchecked.buffer_count() > 0 {
                    node_arc.unchecked.run_once();
                    worked = true;
                    batch_worked = true;
                }
                if node_arc.ledger.backlog_count() >= 100_000 && node_arc.confirming_set.len() >= 128 {
                    node_arc.confirming_set.run_once();
                }
                if !batch_worked || node_arc.ledger.backlog_count() >= 250_000 {
                    break;
                }
            }
        }
        if node_arc.block_processor.total_queue_len() == 0 || node_arc.ledger.backlog_count() >= 100_000 {
            node_arc.confirming_set.run_once();
        }
        node_arc.election_schedulers.priority.run_one();

        if mtime - last_scheduler_tick > millis(100) {
            if node_arc.active.read().len() < 4096 && node_arc.ledger.backlog_count() > 0 {
                if let Some(mut guard) = node_arc.backlog_scan.try_lock() {
                    guard.run_once();
                }
            }
            node_arc.election_schedulers.run_once();
            last_scheduler_tick = mtime;
        }

        // Atomic WAL checkpoint at a transaction boundary (this hart is the single ledger writer,
        // so the cut across the 16 shards is consistent). Time based, or when a buffer pool had to
        // grow because every frame was dirty. Defer periodic timer checkpoints up to 30s while
        // the block processor queue is actively draining a burst to avoid redundant NVMe writes.
        let bpq_busy = node_arc.block_processor.total_queue_len() > 0
            || !node_arc.inbound_message_queue.is_empty_relaxed();
        let checkpoint_interval = if bpq_busy { seconds(30) } else { seconds(10) };
        if mtime - last_checkpoint > checkpoint_interval
            || kernel_common::storage::bpm::NEED_CHECKPOINT.load(Ordering::Relaxed)
        {
            let _ = kernel_common::storage::wal::checkpoint();
            last_checkpoint = kernel_common::clint::mtime();
            worked = true;
        }

        // 3. Periodic node tick
        if mtime - last_node_tick > seconds(2) {
            node_arc.tick();
            last_node_tick = mtime;
            worked = true;
        }

        // 4. Only service bootstrap responder when single-writer ledger commit queues are idle
        if !worked
            && node_arc.block_processor.total_queue_len() == 0
            && node_arc.unchecked.buffer_count() == 0
            && node_arc.bootstrap_server.run_once()
        {
            worked = true;
        }

        iterations += 1;
        adaptive_pause_backoff(worked, &mut idle_spins);
    }
}

#[cfg(feature = "block-test")]
fn drain_async_events(exec: &kernel_common::async_rt::Executor) {
    for _ in 0..4 {
        for h in 1..8 {
            let len = exec.local_queues[h].lock().len();
            for _ in 0..len {
                exec.run_once(h);
            }
        }
    }
}

#[cfg(feature = "block-test")]
fn generate_deterministic_pow(
    root: &rsnano_core::Root,
    threshold: u64,
    nonce_state: &mut u64,
) -> (rsnano_core::WorkNonce, u64, u64) {
    use rsnano_core::{Difficulty, DifficultyV1, WorkNonce};
    let diff = DifficultyV1::default();
    let mut iters = 0u64;
    let start = kernel_common::clint::mtime_raw();
    loop {
        let nonces = [
            WorkNonce::from(nonce_state.wrapping_add(1)),
            WorkNonce::from(nonce_state.wrapping_add(2)),
            WorkNonce::from(nonce_state.wrapping_add(3)),
            WorkNonce::from(nonce_state.wrapping_add(4)),
        ];

        let difficulties = diff.get_difficulties_batch_4(root, nonces);
        iters += 4;

        for i in 0..4 {
            if difficulties[i] >= threshold {
                *nonce_state += 4;
                let elapsed = kernel_common::clint::mtime_raw().wrapping_sub(start);
                return (nonces[i], elapsed, iters);
            }
        }
        *nonce_state += 4;

        if iters % 16384 == 0 {
            kernel_common::watchdog::kick();
        }
    }
}

#[cfg(feature = "block-test")]
fn print_ticks_and_us(ticks: u64) {
    let ticks_per_us = (CLINT_FREQ / 1_000_000) as u128;
    let us_x100 = (ticks as u128 * 100) / ticks_per_us;
    let us_whole = (us_x100 / 100) as usize;
    let us_frac = (us_x100 % 100) as usize;
    uart::print_usize(us_whole);
    uart::print(b".");
    if us_frac < 10 {
        uart::print(b"0");
    }
    uart::print_usize(us_frac);
    uart::print(b" us (");
    uart::print_usize(ticks as usize);
    uart::print(b" ticks)");
}

#[cfg(feature = "block-test")]
fn print_bps(blocks: u64, ticks: u64) {
    if ticks == 0 {
        uart::print(b"inf");
        return;
    }
    let bps_x100 = (blocks as u128 * CLINT_FREQ as u128 * 100) / (ticks as u128);
    let whole = (bps_x100 / 100) as usize;
    let frac = (bps_x100 % 100) as usize;
    uart::print_usize(whole);
    uart::print(b".");
    if frac < 10 {
        uart::print(b"0");
    }
    uart::print_usize(frac);
    uart::print(b" blocks/sec");
}

#[cfg(feature = "block-test")]
fn run_deterministic_block_test(
    node: &Arc<Node>,
    exec: &kernel_common::async_rt::Executor,
) {
    kernel_common::watchdog::kick();
    uart::print(b"\n========================================================================\n");
    uart::print(b"[block-test] DETERMINISTIC BARE-METAL BLOCK GENERATION & PIPELINE TEST\n");
    uart::print(b"[block-test] Timer frequency: 24 MHz (24 ticks = 1 us)\n");
    uart::print(b"========================================================================\n");

    let st = rsnano_sha::blake2b::self_test();
    assert_eq!(st, 0, "[block-test] blake2b SIMD self-test failed");
    rsnano_sha::ed25519::init_radix16_precomp();

    let key = rsnano_core::DEV_GENESIS_KEY.clone();
    let account = key.public_key().as_account();
    let rep = key.public_key();
    let link: rsnano_core::Link = rsnano_core::Account::from(1).into();
    let details = rsnano_core::BlockDetails::new(rsnano_core::Epoch::Epoch2, true, false, false);
    let threshold = node.network_params.work.threshold(&details);

    let mut prev_hash = node.latest(&account);
    let mut balance = node.balance(&account);
    let mut nonce_state: u64 = 0;

    // -------------------------------------------------------------------------
    // Phase 1: Single Block End-to-End Deterministic Breakdown
    // -------------------------------------------------------------------------
    let (work, pow_ticks, pow_iters) =
        generate_deterministic_pow(&rsnano_core::Root::from(prev_hash), threshold, &mut nonce_state);
    balance = balance - rsnano_core::Amount::raw(1);

    // Stage 1: Block Creation (Blake2b state block hashing + Ed25519 signing)
    let t0 = kernel_common::clint::mtime_raw();
    let block: rsnano_core::Block = rsnano_core::StateBlockArgs {
        key: &key,
        previous: prev_hash,
        representative: rep,
        balance,
        link,
        work,
    }
    .into();
    let hash = block.hash();
    let t1 = kernel_common::clint::mtime_raw();

    // Stage 2: Entry PoW Validation (Blake2b difficulty threshold verification)
    let pow_valid = node.network_params.work.validate_entry_block(&block);
    assert!(pow_valid, "[block-test] Entry PoW validation failed");
    let t2 = kernel_common::clint::mtime_raw();

    // Stage 3: BlockProcessor + SignatureChecker (Ed25519 verify) + BlockValidator + BlockInserter (B-Tree)
    let ctx = Arc::new(rsnano_node::block_processing::BlockContext::new(
        block,
        rsnano_node::block_processing::BlockSource::Local,
        rsnano_network::ChannelId::LOOPBACK,
    ));
    let pushed = node.block_processor.push(ctx);
    assert!(pushed, "[block-test] BlockProcessor push failed");
    node.block_processor.run_once();
    assert!(
        node.block_exists(&hash),
        "[block-test] Block not in ledger after BlockProcessor"
    );
    let t3 = kernel_common::clint::mtime_raw();

    // Stage 4: LedgerEventProcessor + ElectionSchedulers + ActiveElections Activation
    drain_async_events(exec);
    node.election_schedulers.run_once();
    drain_async_events(exec);
    assert!(
        node.is_active_hash(&hash),
        "[block-test] Block election did not activate in AEC"
    );
    let t4 = kernel_common::clint::mtime_raw();

    // Stage 5: Consensus Final Vote (Blake2b + Ed25519 sign) + VoteProcessor + ConfirmingSet Cementation
    let vote = Arc::new(rsnano_core::Vote::new_final(&key, alloc::vec![hash]));
    let vote_queued = node.vote_processor_queue.enqueue(
        vote,
        None,
        rsnano_core::VoteSource::Live,
        None,
    );
    assert!(vote_queued, "[block-test] VoteProcessorQueue enqueue failed");
    node.vote_processor.run_once();
    drain_async_events(exec);
    node.confirming_set.run_once();
    drain_async_events(exec);
    assert!(
        node.block_confirmed(&hash),
        "[block-test] Block not cemented after ConfirmingSet"
    );
    let t5 = kernel_common::clint::mtime_raw();

    // Stage 6: Persistent Storage Flush (B-Tree shards -> storage device)
    kernel_common::storage::flush_all_shards();
    let t6 = kernel_common::clint::mtime_raw();

    prev_hash = hash;

    let s1 = t1.wrapping_sub(t0);
    let s2 = t2.wrapping_sub(t1);
    let s3 = t3.wrapping_sub(t2);
    let s4 = t4.wrapping_sub(t3);
    let s5 = t5.wrapping_sub(t4);
    let s6 = t6.wrapping_sub(t5);
    let total_in_mem = t5.wrapping_sub(t0);
    let total_flushed = t6.wrapping_sub(t0);

    uart::print(b"\n--- Phase 1: Single Block Full Lifecycle Breakdown ---\n");
    uart::print(b"  [setup, excluded from totals] PoW Generation (");
    uart::print_usize(pow_iters as usize);
    uart::print(b" nonces):        ");
    print_ticks_and_us(pow_ticks);
    uart::print(b"\n  Stage 1 (Create + Blake2b + Ed25519 Sign): ");
    print_ticks_and_us(s1);
    uart::print(b"\n  Stage 2 (Entry PoW Validation):            ");
    print_ticks_and_us(s2);
    uart::print(b"\n  Stage 3 (BlockProc + SigVerify + B-Tree):  ");
    print_ticks_and_us(s3);
    uart::print(b"\n  Stage 4 (EventProc + Election Activate):   ");
    print_ticks_and_us(s4);
    uart::print(b"\n  Stage 5 (Vote Sign/Verify + Cementation):  ");
    print_ticks_and_us(s5);
    uart::print(b"\n  Stage 6 (B-Tree Shards Storage Flush):     ");
    print_ticks_and_us(s6);
    uart::print(b"\n  ----------------------------------------------------\n");
    uart::print(b"  Total Pipeline (Stages 1-5, In-Memory):    ");
    print_ticks_and_us(total_in_mem);
    uart::print(b" -> ");
    print_bps(1, total_in_mem);
    uart::print(b"\n  Total Pipeline (Stages 1-6, With Flush):   ");
    print_ticks_and_us(total_flushed);
    uart::print(b" -> ");
    print_bps(1, total_flushed);
    uart::print(b"\n");

    // -------------------------------------------------------------------------
    // Phase 2: Multi-Block (64 Blocks) Deterministic Batch Pipeline Benchmark
    // -------------------------------------------------------------------------
    const BATCH_COUNT: u64 = 64;
    let mut batch_pow_ticks = 0u64;
    let mut batch_pow_iters = 0u64;

    // Setup: Precompute deterministic PoW nonces for all blocks in the chain
    let mut works = alloc::vec::Vec::with_capacity(BATCH_COUNT as usize);
    let mut setup_prev = prev_hash;
    let mut setup_bal = balance;
    for _ in 0..BATCH_COUNT {
        kernel_common::watchdog::kick();
        let (b_work, b_pow_t, b_pow_i) = generate_deterministic_pow(
            &rsnano_core::Root::from(setup_prev),
            threshold,
            &mut nonce_state,
        );
        batch_pow_ticks += b_pow_t;
        batch_pow_iters += b_pow_i;
        works.push(b_work);

        setup_bal = setup_bal - rsnano_core::Amount::raw(1);
        let blk: rsnano_core::Block = rsnano_core::StateBlockArgs {
            key: &key,
            previous: setup_prev,
            representative: rep,
            balance: setup_bal,
            link,
            work: b_work,
        }
        .into();
        setup_prev = blk.hash();
    }

    // Stage 1: Batch Block Creation (Construct + Blake2b Hash + Ed25519 Sign for all blocks)
    let mut blocks = alloc::vec::Vec::with_capacity(BATCH_COUNT as usize);
    let mut hashes = alloc::vec::Vec::with_capacity(BATCH_COUNT as usize);
    let s1_start = kernel_common::clint::mtime_raw();
    for i in 0..BATCH_COUNT as usize {
        balance = balance - rsnano_core::Amount::raw(1);
        let b_block: rsnano_core::Block = rsnano_core::StateBlockArgs {
            key: &key,
            previous: prev_hash,
            representative: rep,
            balance,
            link,
            work: works[i],
        }
        .into();
        let b_hash = b_block.hash();
        prev_hash = b_hash;
        hashes.push(b_hash);
        blocks.push(b_block);
    }
    let s1_end = kernel_common::clint::mtime_raw();
    let batch_s1 = s1_end.wrapping_sub(s1_start);

    // Stage 2: Batch Entry PoW Validation (Difficulty threshold verification for all blocks)
    let s2_start = kernel_common::clint::mtime_raw();
    for b in &blocks {
        let b_pow_ok = node.network_params.work.validate_entry_block(b);
        assert!(b_pow_ok, "[block-test] Batch entry PoW validation failed");
    }
    let s2_end = kernel_common::clint::mtime_raw();
    let batch_s2 = s2_end.wrapping_sub(s2_start);

    // Stage 3: BlockProcessor Ingestion (True Batch Processing & Ledger Insertion)
    let s3_start = kernel_common::clint::mtime_raw();
    for b in &blocks {
        let b_ctx = Arc::new(rsnano_node::block_processing::BlockContext::new(
            b.clone(),
            rsnano_node::block_processing::BlockSource::Local,
            rsnano_network::ChannelId::LOOPBACK,
        ));
        assert!(node.block_processor.push(b_ctx));
    }
    node.block_processor.run_once();
    let s3_end = kernel_common::clint::mtime_raw();
    let batch_s3 = s3_end.wrapping_sub(s3_start);

    for h in &hashes {
        assert!(node.block_exists(h), "[block-test] Batch block missing from ledger");
    }

    // Stage 4 & 5: Consensus (Election Schedulers, Bundled Final Vote, Quorum & Cementation)
    // Create a single bundled final vote containing all hashes in the batch (Nano protocol max: 255 hashes)
    let vote_t0 = kernel_common::clint::mtime_raw();
    let bundled_vote = Arc::new(rsnano_core::Vote::new_final(&key, hashes.clone()));
    let vote_t1 = kernel_common::clint::mtime_raw();
    let mut batch_s4 = 0u64;
    let mut batch_s5 = vote_t1.wrapping_sub(vote_t0);

    for i in 0..BATCH_COUNT as usize {
        kernel_common::watchdog::kick();

        let t4_0 = kernel_common::clint::mtime_raw();
        drain_async_events(exec);
        node.election_schedulers.run_once();
        drain_async_events(exec);
        assert!(node.is_active_hash(&hashes[i]));
        let t4_1 = kernel_common::clint::mtime_raw();
        batch_s4 += t4_1.wrapping_sub(t4_0);

        let t5_0 = kernel_common::clint::mtime_raw();
        assert!(node.vote_processor_queue.enqueue(
            bundled_vote.clone(),
            None,
            rsnano_core::VoteSource::Live,
            None,
        ));
        node.vote_processor.run_once();
        drain_async_events(exec);
        node.confirming_set.run_once();
        drain_async_events(exec);
        assert!(node.block_confirmed(&hashes[i]));
        let t5_1 = kernel_common::clint::mtime_raw();
        batch_s5 += t5_1.wrapping_sub(t5_0);
    }

    let f0 = kernel_common::clint::mtime_raw();
    kernel_common::storage::flush_all_shards();
    let f1 = kernel_common::clint::mtime_raw();
    let batch_s6 = f1.wrapping_sub(f0);

    let batch_in_mem = batch_s1 + batch_s2 + batch_s3 + batch_s4 + batch_s5;
    let batch_with_flush = batch_in_mem + batch_s6;

    assert_eq!(node.ledger.block_count(), (2 + BATCH_COUNT) as usize);
    assert_eq!(node.ledger.confirmed_count(), (2 + BATCH_COUNT) as usize);

    uart::print(b"\n--- Phase 2: 64-Block Deterministic Chain Summary (Per-Block Avg) ---\n");
    uart::print(b"  [setup, excluded from throughput] Avg PoW Generation (");
    uart::print_usize((batch_pow_iters / BATCH_COUNT) as usize);
    uart::print(b" nonces/blk):     ");
    print_ticks_and_us(batch_pow_ticks / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 1 (Create + Hash + Sign):        ");
    print_ticks_and_us(batch_s1 / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 2 (Entry PoW Validation):        ");
    print_ticks_and_us(batch_s2 / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 3 (BlockProc + Sig + B-Tree):    ");
    print_ticks_and_us(batch_s3 / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 4 (EventProc + Election Sched):  ");
    print_ticks_and_us(batch_s4 / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 5 (Vote + Quorum + Cement):      ");
    print_ticks_and_us(batch_s5 / BATCH_COUNT);
    uart::print(b"\n  Avg Stage 6 (Amortized Storage Flush):     ");
    print_ticks_and_us(batch_s6 / BATCH_COUNT);
    uart::print(b"\n  ----------------------------------------------------\n");
    uart::print(b"  Avg Per-Block Time (Stages 1-5 In-Memory): ");
    print_ticks_and_us(batch_in_mem / BATCH_COUNT);
    uart::print(b"\n  Avg Per-Block Time (Stages 1-6 Batched):   ");
    print_ticks_and_us(batch_with_flush / BATCH_COUNT);
    uart::print(b"\n  Deterministic Throughput (Stages 1-5):     ");
    print_bps(BATCH_COUNT, batch_in_mem);
    uart::print(b"\n  Deterministic Throughput (Stages 1-6):     ");
    print_bps(BATCH_COUNT, batch_with_flush);
    uart::print(b"\n  Ledger Verified: stored=");
    uart::print_usize(node.ledger.block_count() as usize);
    uart::print(b", confirmed=");
    uart::print_usize(node.ledger.confirmed_count() as usize);
    uart::print(b"\n========================================================================\n\n");

    let bps_whole = if batch_with_flush > 0 {
        ((BATCH_COUNT as u128 * CLINT_FREQ as u128) / (batch_with_flush as u128)) as usize
    } else {
        0
    };
    kernel_common::oled::set_test_bps(bps_whole);
    kernel_common::oled::update_status(
        0,
        node.ledger.block_count() as usize,
        node.ledger.confirmed_count() as usize,
        bps_whole,
        0,
    );
}

/// Hart 1 loop section time in CLINT ticks: [net poll + rpc, message processor, sig checker, executor].
static HART1_PROF: [core::sync::atomic::AtomicU64; 4] = [const { core::sync::atomic::AtomicU64::new(0) }; 4];

fn run_maintenance_and_network_loop(
    node_arc: Arc<Node>,
    _exec: kernel_common::async_rt::Executor,
    _hart_id: usize,
) -> ! {
    tracing::debug!("[boot] Hart 1: Maintenance & Network loop enter");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;

    let mut last_ping = kernel_common::clint::mtime();
    loop {
        let mtime = kernel_common::clint::mtime();

        // 0. Process delegated storage requests from other harts
        kernel_common::storage::process_delegated_requests();

        let mut retry_net = false;
        let mut net_active = false;
        let hp0 = kernel_common::clint::mtime();
        {
            let _intr = kernel_common::uart::InterruptGuard::new();
            kernel_common::net::HART1_WANTS_NET.store(true, Ordering::Release);
            let mut ctx = kernel_common::net::NET.lock();
            if let Some(ref mut ctx_val) = *ctx {
                net_active = ctx_val.poll(mtime);
                handle_rpc_server(ctx_val, node_arc.clone());

                if mtime - last_ping > seconds(5) {
                    let gw = kernel_common::net::get_net_config().gateway;
                    ctx_val.ping(kernel_common::smoltcp::wire::Ipv4Address::new(
                        gw[0], gw[1], gw[2], gw[3],
                    ));
                    last_ping = mtime;
                }
            } else {
                if mtime - last_ping > seconds(5) {
                    retry_net = true;
                    last_ping = mtime;
                }
            }
            drop(ctx);
            kernel_common::net::HART1_WANTS_NET.store(false, Ordering::Release);
        }

        HART1_PROF[0].fetch_add(kernel_common::clint::mtime() - hp0, Ordering::Relaxed);
        if retry_net {
            tracing::warn!("[net] Retrying network initialization...");
            if let Some(new_ctx) = kernel_common::net::init_uboot_net() {
                let _intr = kernel_common::uart::InterruptGuard::new();
                *kernel_common::net::NET.lock() = Some(new_ctx);
                tracing::info!("[net] Network initialized successfully!");
            }
        }

        // 2. Heartbeat & Watchdog
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            let processed = node_arc.message_processor.lock().total_processed();
            tracing::debug!(
                "[heartbeat] Hart 1 (Maint+Net) | Iters: {} | Msgs: {} | ms net={} msgproc={} sig={} exec={}",
                iterations,
                processed,
                (HART1_PROF[0].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize,
                (HART1_PROF[1].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize,
                (HART1_PROF[2].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize,
                (HART1_PROF[3].load(Ordering::Relaxed) * 1000 / CLINT_FREQ) as usize
            );
            last_heartbeat = mtime;
            iterations = 0;
        }

        iterations += 1;

        // Release NET.lock() for a brief window (50 us when active, 200 us when idle)
        // so Cluster 1 worker harts can acquire NET.lock() for socket reads/writes without starving.
        if !net_active && !node_arc.signature_checker.is_empty_relaxed() {
            let hs = kernel_common::clint::mtime();
            let _ = node_arc.signature_checker.help_verify();
            HART1_PROF[2].fetch_add(kernel_common::clint::mtime() - hs, Ordering::Relaxed);
        } else {
            let pause_ticks = if net_active {
                CLINT_FREQ / 20_000
            } else {
                CLINT_FREQ / 5_000
            };
            let pause_start = kernel_common::clint::mtime();
            while kernel_common::clint::mtime().wrapping_sub(pause_start) < pause_ticks {
                core::hint::spin_loop();
            }
        }
    }
}
