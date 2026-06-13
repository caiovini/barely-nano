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

use kernel_common::{pmic_wdt, trap, uart};
use linked_list_allocator::LockedHeap;
use rsnano_core::utils::Tickable;
use rsnano_node::Node;
use rsnano_rpc_handler::RpcCommandHandler;
use rsnano_rpc_server::bare_metal::BareMetalRpcServer;
use smoltcp::socket::tcp::Socket as TcpSocket;

struct InterruptSafeAllocator(LockedHeap);
unsafe impl core::alloc::GlobalAlloc for InterruptSafeAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let _intr = kernel_common::uart::InterruptGuard::new();
        self.0.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let _intr = kernel_common::uart::InterruptGuard::new();
        self.0.dealloc(ptr, layout)
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
            core::arch::asm!(
                "cbo.inval ({0})",
                in(reg) current,
                options(nostack)
            );
        }
        current += block_size;
    }
    unsafe {
        core::arch::asm!("fence r, r", options(nostack));
    }
}


#[no_mangle]
pub static mut SECONDARY_HART_WAKE: [u32; 8] = [0; 8];

#[no_mangle]
pub extern "C" fn rust_main(
    hart_id: usize,
    opaque: usize,
    jt_ptr: *const kernel_common::uboot::JumpTable,
) {
    #[cfg(feature = "mmode")]
    unsafe {
        core::arch::asm!("csrs mstatus, {0}", in(reg) (3usize << 13) | (3usize << 9));
    }

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
        return;
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
            let bss_start = &mut _bss_start as *mut u8;
            let bss_end = &mut _bss_end as *mut u8;
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
    uart::print(b"[boot] RS-Nano Bare Metal Node starting (Hart ");
    uart::print_usize(hart_id);
    uart::print(b")\n");

    extern "C" {
        static _bss_start: u8;
        static _bss_end: u8;
        static _stack_bottom: u8;
        static _stack_top: u8;
        static _heap_start: u8;
        static _heap_end: u8;
    }

    uart::print(b"[boot] BSS: 0x");
    uart::print_usize_hex(unsafe { &_bss_start as *const u8 as usize });
    uart::print(b" - 0x");
    uart::print_usize_hex(unsafe { &_bss_end as *const u8 as usize });
    uart::print(b"\n");

    uart::print(b"[boot] Stack: 0x");
    uart::print_usize_hex(unsafe { &_stack_bottom as *const u8 as usize });
    uart::print(b" - 0x");
    uart::print_usize_hex(unsafe { &_stack_top as *const u8 as usize });
    uart::print(b"\n");

    let heap_start = (unsafe { &_heap_start as *const u8 as usize } + 15) & !15;
    let heap_end = unsafe { &_heap_end as *const u8 as usize } & !15;
    let heap_len = heap_end - heap_start;

    uart::print(b"[boot] Heap: 0x");
    uart::print_usize_hex(heap_start);
    uart::print(b" - 0x");
    uart::print_usize_hex(heap_end);
    uart::print(b" (");
    uart::print_usize(heap_len / 1024 / 1024);
    uart::print(b" MB)\n");

    if HEAP_MAGIC.load(Ordering::SeqCst) != MAGIC_VAL {
        unsafe {
            ALLOCATOR.0.lock().init(heap_start as *mut u8, heap_len);
        }
        HEAP_MAGIC.store(MAGIC_VAL, Ordering::SeqCst);
    }

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

    uart::print(b"[boot] logger init...\n");
    kernel_common::logger::init();
    uart::print(b"[boot] logger init done\n");

    #[cfg(feature = "mmode")]
    let seed = kernel_common::clint::mtime();
    #[cfg(not(feature = "mmode"))]
    let seed = {
        uart::print(b"[boot] getting seed...\n");
        let s = kernel_common::sbi::get_time();
        uart::print(b"[boot] getting seed done\n");
        s
    };
    uart::print(b"[boot] setting seed...\n");
    rsnano_nullable_random::set_seed(seed);
    uart::print(b"[boot] setting seed done\n");

    uart::print(b"[boot] tracing info...\n");
    tracing::info!("--- DYNAMIC NODE START ---");
    uart::print(b"[boot] tracing info done\n");

    static LEADER_ELECTED: AtomicBool = AtomicBool::new(false);
    if !LEADER_ELECTED.swap(true, Ordering::SeqCst) {
        uart::print(b"[boot] net init...\n");
        let net_ctx = kernel_common::net::init_uboot_net();
        uart::print(b"[boot] net init done\n");
        if let Some(ctx) = net_ctx {
            *kernel_common::net::NET.lock() = Some(ctx);
        }

        kernel_common::storage::init_uboot_blk();

        let exec = kernel_common::async_rt::Executor::new();
        kernel_common::async_rt::set_global_executor(exec);

        #[cfg(feature = "spammer")]
        let network = rsnano_core::Networks::NanoDevNetwork;
        #[cfg(not(feature = "spammer"))]
        let network = rsnano_core::Networks::NanoLiveNetwork;

        let mut config = rsnano_node::config::NodeConfig::default_for(network, 8);
        config.online_weight_minimum = rsnano_core::Amount::nano(130_000_000);
        config.network.peer_reachout = core::time::Duration::from_secs(1);
        config.enable_voting = true;
        config.enable_vote_processor = true;
        config.max_pruning_age_s = 2 * 24 * 60 * 60;
        config.allow_local_peers = true;
        config.representative_vote_weight_minimum = rsnano_core::Amount::zero();

        // More aggressive bootstrap parameters for native Orange Pi RV2 hardware
        config.bootstrap.rate_limit = 10000;
        config.bootstrap.database_rate_limit = 4096;
        config.bootstrap.frontier_rate_limit = 128;
        config.bootstrap.channel_limit = 64;
        config.bootstrap.max_requests = 16384;
        config.bootstrap.max_pending_frontier_responses = 128;

        // Larger block processing queue limits
        config.block_processor.max_peer_queue = 65536;
        config.block_processor.max_system_queue = 131072;
        config.block_processor.batch_size = 2048;

        // Larger vote processing queue limits
        config.vote_processor.max_pr_queue = 16384;
        config.vote_processor.max_non_pr_queue = 16384;
        config.vote_processor.batch_size = 4096;
        config.vote_processor.max_triggered = 65536;

        let mut flags = rsnano_node::config::NodeFlags::new();
        flags.enable_voting = true;
        flags.enable_pruning = false;

        let mut node = rsnano_node::NodeBuilder::new(network)
            .data_path(".")
            .resolver(alloc::sync::Arc::new(BareMetalDnsResolver))
            .flags(flags.clone())
            .config(config)
            .finish()
            .expect("Failed to initialize node");

        if flags.enable_pruning {
            kernel_common::uart::print(b"[boot] wiping store...\n");
            node.ledger.store.wipe();
            kernel_common::uart::print(b"[boot] wiping store done.\n");
        }

        kernel_common::uart::print(b"[boot] node.start() starting...\n");
        node.start();
        kernel_common::uart::print(b"[boot] node.start() completed!\n");

        // Helper to add a peer from an IPv4 string
        let add_ipv4_peer = |ip_str: &str, port: u16| {
            kernel_common::uart::print(b"[boot] adding peer ");
            for b in ip_str.as_bytes() {
                kernel_common::uart::putchar(*b);
            }
            kernel_common::uart::print(b"...\n");
            if let Ok(ip) = core::net::Ipv4Addr::from_str(ip_str) {
                let addr_v6 = ip.to_ipv6_mapped();
                let socket_addr = core::net::SocketAddrV6::new(addr_v6, port, 0, 0);

                // Add to persistent peer store so it gets retried by PeerCacheConnector
                node.ledger
                    .store
                    .peer
                    .put(socket_addr, rsnano_nullable_clock::SystemTime::now());

                let _ = node.peer_connector.connect_to(socket_addr);
            }
            kernel_common::uart::print(b"[boot] adding peer done\n");
        };

        // Add initial bootstrap peers (DNS and Hardcoded IPs)
        let _peering_host = "peering.nano.org";

        // Hardcoded IPs to bypass initial DNS dependency
        add_ipv4_peer("37.120.187.138", 7075); // Active public representative
        add_ipv4_peer("94.130.135.50", 7075); // Active public representative
        add_ipv4_peer("192.168.0.244", 7075); // Local synced node

        let node_arc = alloc::sync::Arc::new(node);
        {
            *NODE.lock() = Some(NodeWrapper(node_arc.clone()));
        }

        // Signal secondary harts that the node is ready BEFORE starting them,
        // so harts that boot quickly don't spin past the flag before it's set.
        NODE_INIT_COMPLETE.store(true, Ordering::SeqCst);

        kernel_common::uart::print(b"[boot] Starting secondary harts...\n");
        let start_addr = unsafe { &_start as *const u8 as usize };

        // Flush the entire kernel (code, data, bss, statics) to DRAM before waking
        // secondary harts on Cluster 1. Use 3MB to cover the full binary (~2.35MB).
        clean_cache_range(start_addr, 3 * 1024 * 1024);

        for id in 0..8 {
            if id != hart_id {
                if id == 0 {
                    kernel_common::uart::print(
                        b"[boot] Skipping Hart 0 (known to hang OpenSBI on this SoC)\n",
                    );
                    continue;
                }

                kernel_common::uart::print(b"[boot] Starting Hart ");
                kernel_common::uart::print_usize(id);
                kernel_common::uart::print(b"...\n");

                let (initial_err, initial_status) = kernel_common::sbi::hart_get_status(id);

                kernel_common::uart::print(b"[boot] Hart ");
                kernel_common::uart::print_usize(id);
                kernel_common::uart::print(b" initial status: err=");
                kernel_common::uart::print_usize(initial_err);
                kernel_common::uart::print(b", status=");
                kernel_common::uart::print_usize(initial_status);
                kernel_common::uart::print(b"\n");

                let start_addr_to_use = start_addr;

                let (err, _) = kernel_common::sbi::hart_start(id, start_addr_to_use, 0);

                // Workaround for SpacemiT K1 OpenSBI: 
                // The firmware might fail to send the wakeup IPI internally,
                // leaving the hart stuck in START_PENDING state.
                // We send a manual IPI to force it to wake up.
                kernel_common::sbi::send_ipi(1 << id, 0);

                // Wait up to ~1s for it to leave START_PENDING (status=2)
                let mut status = 2;
                let mut err_status = 0;
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

                kernel_common::uart::print(b"[boot] Hart ");
                kernel_common::uart::print_usize(id);
                kernel_common::uart::print(b" start result: err=");
                kernel_common::uart::print_usize(err);
                kernel_common::uart::print(b", get_status_err=");
                kernel_common::uart::print_usize(err_status);
                kernel_common::uart::print(b", status=");
                kernel_common::uart::print_usize(status);
                kernel_common::uart::print(b", wake=");
                kernel_common::uart::print_usize(wake_val as usize);
                kernel_common::uart::print(b"\n");
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

    kernel_common::storage::flush_all_shards();

    // Monitor and stats tasks are now handled by a dedicated Hart loop (Hart 7)
    // to ensure reliability and free up the async executor.

    kernel_common::uart::print(b"[boot] Entering task loop for Hart ");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b"...\n");

    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .as_ref()
        .expect("Executor not initialized")
        .clone();

    // On the SpacemiT K1 multi-cluster design, RISC-V fence is NOT sufficient for
    // cross-cluster cache visibility. We must flush all shared state (statics, locks,
    // AND heap contents) from Cluster 0's cache to DRAM so Cluster 1 harts can see it.
    // cbo.flush is always permitted in S-mode.
    //
    // CRITICAL: Arc<T> inner data (e.g. Arc<Mutex<BootstrapState>>) lives in the HEAP,
    // not in BSS. Secondary harts read the Arc pointer correctly from the flushed BSS/globals,
    // but then dereference into heap memory that is still cached only in Cluster 0's L2.
    // Without flushing the heap, secondary harts get a Load Access Fault when they
    // dereference the first level of indirection inside any Arc<T>.
    //
    // We flush the used portion of the heap. `LockedHeap` is a bump+free-list allocator,
    // so all live data is within [heap_start, heap_start + used_bytes].
    // We conservatively flush the entire allocated range to be safe.
    {
        let heap = ALLOCATOR.0.lock();
        let heap_start = heap.bottom() as usize;
        let heap_used = heap.used();
        drop(heap);
        if heap_used > 0 {
            // Round up to cache-line boundary.
            clean_cache_range(heap_start, heap_used);
        }
    }

    // Flush BSS (contains global statics: NODE, EXECUTOR, HART_DISPATCH_READY, etc.)
    unsafe {
        let bss_start = &_bss_start as *const u8 as usize;
        let bss_end = &_bss_end as *const u8 as usize;
        let len = bss_end - bss_start;
        clean_cache_range(bss_start, len);
    }

    // Signal secondary harts: all state is flushed to DRAM; go ahead.
    HART_DISPATCH_READY.store(true, Ordering::SeqCst);
    // Flush the flag itself last, so secondary harts see it only after all other
    // state above is already coherent in DRAM.
    let flag_addr = &HART_DISPATCH_READY as *const _ as usize;
    clean_cache_range(flag_addr, 8);

    dispatch_hart_task(hart_id, node_arc, exec);
}
fn secondary_hart_main(hart_id: usize) {
    // Initialize trap vector for this hart BEFORE touching any global locks.
    kernel_common::trap::init();

    // NOTE: Do NOT call invalidate_cache_range (cbo.inval) here.
    // cbo.inval in S-mode requires menvcfg.CBIE to be set by M-mode firmware.
    // OpenSBI on this SoC does NOT set CBIE, so the instruction raises an
    // illegal-instruction trap and kills the hart silently.
    //
    // BSS visibility is already guaranteed:
    //   1. Hart 1 flushed the entire BSS to DRAM via clean_cache_range (cbo.flush,
    //      always permitted in S-mode) before setting HART_DISPATCH_READY.
    //   2. Each secondary hart already called invalidate_cache_range on the
    //      HART_DISPATCH_READY flag word in its spin-wait loop, which forces it
    //      to re-fetch from DRAM and breaks any stale L1/L2 state.
    //   3. The SeqCst store/load pair on HART_DISPATCH_READY provides the
    //      required acquire/release barrier for the BSS contents.

    // Now that traps are initialized and BSS is invalidated,
    // Hart 1 has released all global locks and we can read them from DRAM.
    let node_arc = {
        let lock = NODE.lock();
        lock.as_ref().expect("[PANIC] Node not initialized on secondary hart").0.clone()
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
) {
    match hart_id {
        0 => run_heavy_io_loop(node_arc, exec, hart_id),
        1 => run_maintenance_and_network_loop(node_arc, exec, hart_id),
        2 => run_block_processor_loop(node_arc, exec, hart_id),
        3 => run_consensus_loop(node_arc, exec, hart_id),
        4 => run_rpc_worker_loop(node_arc, exec, hart_id),
        5 => run_stats_worker_loop(node_arc, exec, hart_id),
        6 => run_heavy_io_loop(node_arc, exec, hart_id),
        7 => run_monitor_loop(node_arc, exec, hart_id),
        _ => {
            kernel_common::uart::print(b"[boot] Hart ");
            kernel_common::uart::print_usize(hart_id);
            kernel_common::uart::print(b": No task assigned, entering parking loop\n");
            loop {
                core::hint::spin_loop();
            }
        }
    }
}

#[allow(dead_code)]
fn run_network_loop(node_arc: Arc<Node>, hart_id: usize) {
    tracing::info!("[boot] Hart 0: Network & RPC loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .as_ref()
        .unwrap()
        .clone();
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_heartbeat > seconds(1) {
            last_heartbeat = mtime;
            iterations = 0;
        }

        /*
                {
                    let _intr = kernel_common::uart::InterruptGuard::new();
                    let mut ctx = kernel_common::net::NET.lock();
                    if let Some(ref mut ctx_val) = *ctx {
                        ctx_val.poll(mtime);
                    }
                }
        */

        node_arc.message_processor.lock().run_once();
        if !exec.run_once(hart_id) {
            core::hint::spin_loop();
        }
        iterations += 1;
        core::hint::spin_loop();
    }
}

fn run_rpc_worker_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) {
    tracing::info!("[boot] Hart 4: RPC Server loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();
        {
            let _intr = kernel_common::uart::InterruptGuard::new();
            let mut ctx = kernel_common::net::NET.lock();
            if let Some(ref mut ctx_val) = *ctx {
                handle_rpc_server(ctx_val, node_arc.clone());
            }
        }

        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart 4 (RPC) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        node_arc.signature_checker.try_help_verify();
        if !exec.run_once(hart_id) {
            core::hint::spin_loop();
        }
        iterations += 1;

        // Pacing: Throttle Hart 4 RPC loop to resolve lock starvation of NET.lock
        for _ in 0..5000 {
            core::hint::spin_loop();
        }
    }
}

fn run_stats_worker_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) {
    kernel_common::uart::print(b"[boot] Hart 5: Vote Processor loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart 5 (VoteProc) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        node_arc.vote_processor.run_once();
        node_arc.signature_checker.try_help_verify();
        if !exec.run_once(hart_id) {
            core::hint::spin_loop();
        }
        iterations += 1;
        core::hint::spin_loop();
    }
}

#[allow(dead_code)]
fn run_maintenance_loop(node_arc: Arc<Node>, hart_id: usize) {
    kernel_common::uart::print(b"[boot] Hart 1: Maintenance & Async Executor loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_cleanup = kernel_common::clint::mtime();
    let mut last_aec_tick = kernel_common::clint::mtime();
    let mut last_online_weight_tick = kernel_common::clint::mtime();
    let _last_node_tick = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_aec_tick > millis(300) {
            if let Some(mut guard) = node_arc.aec_ticker.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                rsnano_core::utils::Tickable::tick(&mut *guard, &cancel_token);
            }
            last_aec_tick = mtime;
        }

        if mtime - last_heartbeat > seconds(1) {
            last_heartbeat = mtime;
            iterations = 0;
        }
        iterations += 1;
        if mtime - last_cleanup > seconds(5) {
            if let Some(guard) = node_arc.dead_channel_cleanup.try_lock() {
                guard.clean_up();
            }
            if let Some(mut guard) = node_arc.peer_cache_connector.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                guard.tick(&cancel_token);
            }
            last_cleanup = mtime;
        }

        if mtime - last_online_weight_tick > seconds(2) {
            if let Some(mut guard) = node_arc.online_weight_calculation.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                guard.tick(&cancel_token);
            }
            last_online_weight_tick = mtime;
        }

        for _ in 0..8 {
            if !exec.run_once(hart_id) {
                core::hint::spin_loop();
                break; // No more work for now
            }
        }
        core::hint::spin_loop();
    }
}

fn run_block_processor_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) {
    kernel_common::uart::print(b"[boot] Hart 2: Block Processor loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_flush = kernel_common::clint::mtime();
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(
                    b"[heartbeat] Hart 2 (BlockProc) | Total Blocks: ",
                );
                kernel_common::uart::print_usize_no_lock(node_arc.ledger.block_count());
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
            }
        }

        if node_arc.block_processor.total_queue_len() > 0 {
            node_arc.block_processor.run_once();

            if mtime - last_flush > seconds(5) {
                for i in 0..kernel_common::storage::SHARD_COUNT {
                    if let Some(mut btree_guard) = kernel_common::storage::SHARDS[i].try_lock() {
                        if let Some(btree) = btree_guard.as_mut() {
                            let _ = btree.flush();
                        }
                    }
                }
                last_flush = mtime;
            }
        }
        node_arc.signature_checker.try_help_verify();
        if !exec.run_once(hart_id) {
            core::hint::spin_loop();
        }
        core::hint::spin_loop();
    }
}

fn run_consensus_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) {
    kernel_common::uart::print(b"[boot] Hart 3: Message/Vote Processor loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_aec_voter_tick = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_aec_voter_tick > millis(100) {
            let mut aec_voter = node_arc.aec_voter.lock();
            let mut aec_ticker = node_arc.aec_ticker.lock();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            rsnano_core::utils::Tickable::tick(&mut *aec_voter, &cancel_token);
            rsnano_core::utils::Tickable::tick(&mut *aec_ticker, &cancel_token);
            last_aec_voter_tick = mtime;
        }
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart 3 (Consensus) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        // Help with executor during consensus wait periods.
        // Spreading exec.run_once() across harts ensures low-latency response for async tasks.
        for _ in 0..16 {
            if !exec.run_once(hart_id) {
                core::hint::spin_loop();
                break;
            }
        }
        node_arc.signature_checker.try_help_verify();
        iterations += 1;
        core::hint::spin_loop();
    }
}

fn handle_rpc_server(ctx: &mut kernel_common::net::NetworkContext, node: Arc<Node>) {
    let socket = ctx.sockets.get_mut::<TcpSocket>(ctx.rpc_handle);
    if !socket.is_open() {
        let _ = socket.listen(7076);
    }
    if socket.can_recv() {
        let mut response: Option<alloc::string::String> = None;
        let _ = socket.recv(|buffer| {
            let request = core::str::from_utf8(buffer).unwrap_or("");
            let handler = RpcCommandHandler::new(node.clone(), true);
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
            let _ = socket.send_slice(resp_str.as_bytes());
            socket.close();
        }
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

#[allow(dead_code)]
fn run_async_worker_loop(node_arc: Arc<Node>, hart_id: usize) {
    tracing::info!("[boot] Hart 6: Async Executor & Ledger Maintenance loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    loop {
        for _ in 0..128 {
            exec.run_once(hart_id);
        }
        node_arc.confirming_set.run_once();
        node_arc.vote_cache_processor.run_once();
        node_arc.backlog_scan.lock().run_once();
        node_arc.vote_processor.run_once();
        node_arc.signature_checker.try_help_verify();
        core::hint::spin_loop();
    }
}

fn run_monitor_loop(node_mon: Arc<Node>, exec: kernel_common::async_rt::Executor, hart_id: usize) {
    kernel_common::uart::print(b"[boot] Hart 7: Monitor loop enter\n");
    let mut last_tick = kernel_common::clint::mtime();
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_stored_count = 0;
    let mut last_confirmed_count = 0;
    let mut iterations = 0;

    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart 7 (Monitor) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        if mtime - last_tick > seconds(15) {
            let mut free_pages = 0;
            let mut total_pages = 0;
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                let (used, _) = {
                    let heap = ALLOCATOR.0.lock();
                    (heap.used(), heap.size())
                };
                kernel_common::uart::print_no_lock(b"[monitor] Heap: ");
                kernel_common::uart::print_usize_no_lock(used / 1024 / 1024);
                kernel_common::uart::print_no_lock(b" MB | DB: ");
                let shard_max = kernel_common::storage::SHARD_SIZE_PAGES.load(Ordering::Relaxed);
                for i in 0..kernel_common::storage::SHARD_COUNT {
                    if let Some(mut btree_guard) = kernel_common::storage::SHARDS[i].try_lock() {
                        if let Some(btree) = btree_guard.as_mut() {
                            total_pages += btree.page_count();
                            free_pages += btree.capacity_remaining(shard_max);
                            free_pages += btree.free_page_count();
                        }
                    }
                }
                let db_kb = total_pages * 4;
                let stored_count = node_mon.ledger.block_count();
                let confirmed_count = node_mon.ledger.confirmed_count();

                let bps = (stored_count - last_stored_count) / 15;
                let _cps = (confirmed_count - last_confirmed_count) / 15;
                last_stored_count = stored_count;
                last_confirmed_count = confirmed_count;

                let channels_info = node_mon.network.read().channels_info();
                let online_stake = node_mon.online_reps.lock().online_weight();

                kernel_common::uart::print_no_lock(b"[monitor] Heap: ");
                kernel_common::uart::print_usize_no_lock(used / 1024 / 1024);
                kernel_common::uart::print_no_lock(b" MB | DB: ");
                if db_kb >= 1024 {
                    kernel_common::uart::print_usize_no_lock(db_kb / 1024);
                    kernel_common::uart::print_no_lock(b" MB (");
                } else {
                    kernel_common::uart::print_usize_no_lock(db_kb);
                    kernel_common::uart::print_no_lock(b" KB (");
                }
                kernel_common::uart::print_usize_no_lock(free_pages);
                kernel_common::uart::print_no_lock(b" free) | Peers: ");
                kernel_common::uart::print_usize_no_lock(channels_info.total);
                kernel_common::uart::print_no_lock(b" (");
                kernel_common::uart::print_usize_no_lock(channels_info.outbound);
                kernel_common::uart::print_no_lock(b" out) | Stake: ");
                kernel_common::uart::print_usize_no_lock(
                    (online_stake.number() / (10u128.pow(30))) as usize,
                );
                kernel_common::uart::print_no_lock(b" | Rate: ");
                kernel_common::uart::print_usize_no_lock(bps as usize);
                kernel_common::uart::print_no_lock(b" bps\n");

                let stats_col = node_mon.stats();
                let mut first = true;
                for (key, val) in stats_col.iter() {
                    if (key.stat == "block_processor_result"
                        || key.stat == "block_processor_source")
                        && *val > 0
                    {
                        if first {
                            kernel_common::uart::print_no_lock(b"   [stats] ");
                            first = false;
                        } else {
                            kernel_common::uart::print_no_lock(b" | ");
                        }
                        kernel_common::uart::print_no_lock(key.detail.as_bytes());
                        kernel_common::uart::print_no_lock(b": ");
                        kernel_common::uart::print_usize_no_lock(*val as usize);
                    }
                }
                if !first {
                    kernel_common::uart::print_no_lock(b"\n");
                }
            }
            node_mon.tick_monitor();
            DB_PAGES_FREE.store(free_pages, Ordering::Relaxed);
            last_tick = mtime;
        }

        node_mon.vote_processor.run_once();
        if !exec.run_once(hart_id) {
            // Aggressive throttle for the monitor hart to free up system bus
            for _ in 0..5000 {
                core::hint::spin_loop();
            }
        }
        node_mon.signature_checker.try_help_verify();
        iterations += 1;
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let hart_id: usize;
    unsafe {
        core::arch::asm!("mv {0}, tp", out(reg) hart_id);
    }
    uart::print(b"[PANIC] hart=");
    uart::print_usize(hart_id);
    uart::print(b" system panicked\n");
    if let Some(loc) = info.location() {
        uart::print(b" at ");
        uart::print(loc.file().as_bytes());
        // uart::print(b".");
        uart::print_usize(loc.line() as usize);
    }
    uart::print(b"\n");
    loop {}
}

fn run_heavy_io_loop(node_arc: Arc<Node>, exec: kernel_common::async_rt::Executor, hart_id: usize) {
    kernel_common::uart::print(b"[boot] Hart 6: Heavy I/O & Bootstrap loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_flush = kernel_common::clint::mtime();
    let mut last_bootstrap_tick = kernel_common::clint::mtime();
    let mut last_node_tick = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_bootstrap_tick > millis(100) {
            node_arc.bootstrapper.run_once();
            last_bootstrap_tick = mtime;
        }

        if mtime - last_flush > seconds(60) {
            kernel_common::storage::flush_all_shards();
            last_flush = mtime;
        }

        if mtime - last_node_tick > millis(100) {
            node_arc.tick();
            last_node_tick = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart ");
                kernel_common::uart::print_usize_no_lock(hart_id);
                kernel_common::uart::print_no_lock(b" (HeavyIO) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        for _ in 0..16 {
            if !exec.run_once(hart_id) {
                core::hint::spin_loop();
                break;
            }
        }

        #[cfg(feature = "spammer")]
        {
            if DB_PAGES_FREE.load(Ordering::Relaxed) > 256 {
                node_arc.spammer.generate_batch(64);
            } else {
                // Throttle significantly when low on space
                for _ in 0..10000 {
                    core::hint::spin_loop();
                }
            }
        }

        iterations += 1;
        core::hint::spin_loop();
    }
}
fn run_maintenance_and_network_loop(
    node_arc: Arc<Node>,
    exec: kernel_common::async_rt::Executor,
    hart_id: usize,
) {
    kernel_common::uart::print(b"[boot] Hart 1: Maintenance & Network loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_watchdog = kernel_common::clint::mtime();
    let mut last_cleanup = kernel_common::clint::mtime();
    let mut last_aec_tick = kernel_common::clint::mtime();
    let mut last_online_weight_tick = kernel_common::clint::mtime();
    let mut iterations = 0;

    let mut last_ping = kernel_common::clint::mtime();
    loop {
        let mtime = kernel_common::clint::mtime();

        // 0. Process delegated storage requests from other harts
        kernel_common::storage::process_delegated_requests();

        {
            let _intr = kernel_common::uart::InterruptGuard::new();
            let mut ctx = kernel_common::net::NET.lock();
            if let Some(ref mut ctx_val) = *ctx {
                ctx_val.poll(mtime);

                if mtime - last_ping > seconds(5) {
                    ctx_val.ping(kernel_common::smoltcp::wire::Ipv4Address::new(
                        192, 168, 0, 1,
                    ));
                    ctx_val.ping(kernel_common::smoltcp::wire::Ipv4Address::new(
                        192, 168, 0, 244,
                    ));
                    last_ping = mtime;
                }
            }
        }
        node_arc.message_processor.lock().run_once();

        // 2. Maintenance Ticks
        if mtime - last_aec_tick > millis(300) {
            if let Some(mut guard) = node_arc.aec_ticker.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                rsnano_core::utils::Tickable::tick(&mut *guard, &cancel_token);
            }
            last_aec_tick = mtime;
        }

        if mtime - last_cleanup > seconds(5) {
            if let Some(guard) = node_arc.dead_channel_cleanup.try_lock() {
                guard.clean_up();
            }
            if let Some(mut guard) = node_arc.peer_cache_connector.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                guard.tick(&cancel_token);
            }
            last_cleanup = mtime;
        }

        if mtime - last_online_weight_tick > seconds(2) {
            if let Some(mut guard) = node_arc.online_weight_calculation.try_lock() {
                let cancel_token = rsnano_core::utils::CancellationToken::new();
                guard.tick(&cancel_token);
            }
            last_online_weight_tick = mtime;
        }

        // 3. Heartbeat
        if mtime - last_watchdog > seconds(1) {
            kernel_common::watchdog::kick();
            last_watchdog = mtime;
        }

        if mtime - last_heartbeat > seconds(10) {
            let processed = node_arc.message_processor.lock().total_processed();
            if let Some(_lock) = kernel_common::uart::UART_LOCK.try_lock() {
                kernel_common::uart::print_no_lock(b"[heartbeat] Hart 1 (Maint+Net) | Iters: ");
                kernel_common::uart::print_usize_no_lock(iterations);
                kernel_common::uart::print_no_lock(b" | Msgs: ");
                kernel_common::uart::print_usize_no_lock(processed as usize);
                kernel_common::uart::print_no_lock(b"\n");
                last_heartbeat = mtime;
                iterations = 0;
            }
        }

        // 4. Executor & Signature Checker
        node_arc.signature_checker.try_help_verify();
        for _ in 0..8 {
            if !exec.run_once(hart_id) {
                core::hint::spin_loop();
                break;
            }
        }
        iterations += 1;

        // Pacing: Throttle Hart 1 network/maintenance loop to resolve lock starvation of NET.lock
        for _ in 0..5000 {
            core::hint::spin_loop();
        }
    }
}
