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
use rsnano_ledger::LedgerSet;
use rsnano_node::Node;
use rsnano_rpc_handler::RpcCommandHandler;
use rsnano_rpc_server::bare_metal::BareMetalRpcServer;
use smoltcp::socket::tcp::Socket as TcpSocket;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

global_asm!(include_str!("entry.S"));

static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(999);
static BOOT_COMPLETE: AtomicBool = AtomicBool::new(false);
static HARDWARE_INIT_COMPLETE: AtomicBool = AtomicBool::new(false);
static HEAP_MAGIC: AtomicUsize = AtomicUsize::new(0);
const MAGIC_VAL: usize = 0xACE1FEED;
static NODE: spin::Mutex<Option<NodeWrapper>> = spin::Mutex::new(None);

const CLINT_FREQ: u64 = 10_000_000;

#[inline]
fn seconds(s: u64) -> u64 {
    s * CLINT_FREQ
}

#[inline]
fn millis(ms: u64) -> u64 {
    ms * CLINT_FREQ / 1000
}

#[no_mangle]
pub extern "C" fn rust_main(hart_id: usize) {
    if hart_id >= 8 {
        let _guard = uart::UART_LOCK.lock();
        uart::print_no_lock(b"[entry] ERROR: Invalid Hart ID ");
        uart::print_usize_no_lock(hart_id);
        uart::print_no_lock(b"\n");
        loop {}
    }

    {
        let _guard = uart::UART_LOCK.lock();
        uart::print_no_lock(b"[entry] Hart ");
        uart::print_usize_no_lock(hart_id);
        uart::print_no_lock(b" entered rust_main\n");
    }

    kernel_common::uart::print(b"RM-");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b"\n");

    let prev_boot_id =
        BOOT_HART_ID.compare_exchange(999, hart_id, Ordering::SeqCst, Ordering::SeqCst);
    if prev_boot_id.is_ok()
        || prev_boot_id.as_ref().is_ok_and(|&id| id == hart_id)
        || prev_boot_id.as_ref().is_err_and(|&id| id == hart_id)
    {
        boot_hart_main(hart_id);
    } else {
        secondary_hart_main(hart_id);
    }
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
            ALLOCATOR.lock().init(heap_start as *mut u8, heap_len);
        }
        HEAP_MAGIC.store(MAGIC_VAL, Ordering::SeqCst);
    }

    if !HARDWARE_INIT_COMPLETE.load(Ordering::SeqCst) {
        HARDWARE_INIT_COMPLETE.store(true, Ordering::SeqCst);
        trap::init();
    } else {
        // Warm reboot detected. Force-unlock critical system locks to prevent deadlocks.
        unsafe {
            kernel_common::uart::UART_LOCK.force_unlock();
            ALLOCATOR.force_unlock();
            kernel_common::net::NET.force_unlock();
            NODE.force_unlock();
            kernel_common::async_rt::EXECUTOR.force_unlock();
            kernel_common::storage::DB.force_unlock();
        }
        trap::init();
    }

    kernel_common::logger::init();

    #[cfg(feature = "mmode")]
    let seed = kernel_common::clint::mtime();
    #[cfg(not(feature = "mmode"))]
    let seed = kernel_common::sbi::get_time();
    rsnano_nullable_random::set_seed(seed);
    tracing::info!("--- DYNAMIC NODE START ---");

    let net_ctx = kernel_common::net::init_virtio_net();
    if let Some(ctx) = net_ctx {
        *kernel_common::net::NET.lock() = Some(ctx);
    }

    let storage_ctx = kernel_common::storage::init_virtio_blk();
    if let Some(storage) = storage_ctx {
        let bpm = kernel_common::storage::bpm::BufferPool::new(storage, 64);
        let btree = kernel_common::storage::btree::BTree::new(bpm);
        *kernel_common::storage::DB.lock() = Some(btree);
    }

    let exec = kernel_common::async_rt::Executor::new();
    *kernel_common::async_rt::EXECUTOR.lock() = Some(exec);

    let mut config =
        rsnano_node::config::NodeConfig::default_for(rsnano_core::Networks::NanoLiveNetwork, 8);
    config.network.peer_reachout = core::time::Duration::from_secs(1);
    config.preconfigured_peers.clear();
    config
        .preconfigured_peers
        .push(rsnano_core::utils::Peer::new(
            alloc::string::String::from("192.168.0.245"),
            7075,
        ));
    config
        .preconfigured_peers
        .push(rsnano_core::utils::Peer::new(
            alloc::string::String::from("37.120.187.138"),
            7075,
        ));

    // --- Performance Tuning ---
    config.tcp.max_inbound_connections = 32;
    config.tcp.max_outbound_connections = 32;
    config.tcp.max_attempts = 32;

    config.network.max_inbound_connections = 32;
    config.network.max_outbound_connections = 32;

    config.vote_processor.max_pr_queue = 64;
    config.vote_processor.max_non_pr_queue = 16;
    config.block_processor.max_peer_queue = 128;
    config.message_processor.max_queue = 256;

    let (used, total) = {
        let heap = ALLOCATOR.lock();
        (heap.used(), heap.size())
    };
    uart::print(b"[boot] Pre-Ledger Heap: ");
    uart::print_usize(used / 1024);
    uart::print(b"/");
    uart::print_usize(total / 1024);
    uart::print(b" KB\n");

    uart::print(b"[boot] Creating Node (on heap)...\n");
    let mut node = rsnano_node::NodeBuilder::new(rsnano_core::Networks::NanoLiveNetwork)
            .data_path(".")
            .resolver(alloc::sync::Arc::new(BareMetalDnsResolver))
            .config(config)
            .finish()
            .expect("Failed to initialize node");

    node.start();

    // --- Genesis Diagnostics ---
    let genesis_hash = node.ledger.genesis().hash();
    let is_genesis_confirmed = node.ledger.confirmed().block_exists(&genesis_hash);
    let block_count = node.ledger.block_count();
    let account_count = node.ledger.account_count();
    let confirmed_count = node.ledger.confirmed_count();

    tracing::error!("LEDGER DIAGNOSTICS:");
    tracing::error!("  Genesis Hash: {}", genesis_hash);
    tracing::error!("  Genesis Confirmed: {}", is_genesis_confirmed);
    tracing::error!("  Block Count: {}", block_count);
    tracing::error!("  Account Count: {}", account_count);
    tracing::error!("  Confirmed Count: {}", confirmed_count);

    {
        tracing::debug!("  Accounts in Ledger:");
        let any = node.ledger.any();
        let it = any.accounts_range(rsnano_core::Account::zero()..);
        for (acc, info) in it {
            tracing::debug!(
                "    - {}: head={}, count={}",
                acc.encode_account(),
                info.head,
                info.block_count
            );
        }
    }

    let node_arc = alloc::sync::Arc::new(node);
    {
        *NODE.lock() = Some(NodeWrapper(node_arc.clone()));
    }

    // Start secondary harts ONLY after node is fully ready
    for id in 0..8 {
        if id != hart_id {
            let (err, _) = kernel_common::sbi::hart_start(id, 0x80200000, 0);
            if err != 0 && err != 18446744073709551610 {
                kernel_common::uart::print(b"[boot] ERROR starting Hart ");
                kernel_common::uart::print_usize(id);
                kernel_common::uart::print(b"\n");
            }
        }
    }

    BOOT_COMPLETE.store(true, Ordering::SeqCst);

    {
        let mut db_guard = kernel_common::storage::DB.lock();
        if let Some(db) = db_guard.as_mut() {
            let _ = db.flush();
        }
    }

    // Monitor and stats tasks are now handled by a dedicated Hart loop (Hart 7)
    // to ensure reliability and free up the async executor.

    kernel_common::uart::print(b"[boot] Entering task loop for Hart ");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b"...\n");

    dispatch_hart_task(hart_id, node_arc);
}
fn secondary_hart_main(hart_id: usize) {
    {
        let _guard = uart::UART_LOCK.lock();
        uart::print_no_lock(b"[entry] Hart ");
        uart::print_usize_no_lock(hart_id);
        uart::print_no_lock(b" entered secondary_hart_main\n");
    }

    // Wait for boot hart to finish basic hardware setup
    while !HARDWARE_INIT_COMPLETE.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }

    kernel_common::uart::print(b"[boot] Hart ");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b" starting (secondary_hart_main)...\n");

    while !BOOT_COMPLETE.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }

    kernel_common::uart::print(b"[boot] Hart ");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b" boot complete, initializing traps...\n");

    kernel_common::trap::init();

    kernel_common::uart::print(b"[boot] Hart ");
    kernel_common::uart::print_usize(hart_id);
    kernel_common::uart::print(b" traps init, getting node...\n");
    if hart_id == 1 {
        kernel_common::uart::print(b"H1-A\n");
    }
    if hart_id == 2 {
        kernel_common::uart::print(b"H2-A\n");
    }
    if hart_id == 3 {
        kernel_common::uart::print(b"H3-A\n");
    }

    let node_arc = {
        if hart_id == 1 {
            kernel_common::uart::print(b"H1-B\n");
        }
        if hart_id == 2 {
            kernel_common::uart::print(b"H2-B\n");
        }
        if hart_id == 3 {
            kernel_common::uart::print(b"H3-B\n");
        }
        let lock = NODE.lock();
        if hart_id == 1 {
            kernel_common::uart::print(b"H1-C\n");
        }
        if hart_id == 2 {
            kernel_common::uart::print(b"H2-C\n");
        }
        if hart_id == 3 {
            kernel_common::uart::print(b"H3-C\n");
        }
        lock.as_ref().expect("Node not initialized").0.clone()
    };
    if hart_id == 1 {
        kernel_common::uart::print(b"H1-D\n");
    }
    if hart_id == 2 {
        kernel_common::uart::print(b"H2-D\n");
    }
    if hart_id == 3 {
        kernel_common::uart::print(b"H3-D\n");
    }

    dispatch_hart_task(hart_id, node_arc);
}

fn dispatch_hart_task(hart_id: usize, node_arc: Arc<Node>) {
    match hart_id {
        0 => run_network_loop(node_arc),
        1 => run_maintenance_loop(node_arc),
        2 => run_block_processor_loop(node_arc),
        3 => run_consensus_loop(node_arc),
        4 => run_rpc_worker_loop(node_arc),
        5 => run_stats_worker_loop(node_arc),
        6 => run_async_worker_loop(node_arc),
        7 => run_monitor_loop(node_arc),
        _ => run_async_worker_loop(node_arc), // Help with async tasks on any extra harts
    }
}

fn run_network_loop(_node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 0: Network & RPC loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 0 (Network) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }

        {
            let mut ctx = kernel_common::net::NET.lock();
            if let Some(ref mut ctx_val) = *ctx {
                ctx_val.poll(mtime);
            }
        }

        iterations += 1;
        for _ in 0..64 {
            exec.run_once();
        }
        core::hint::spin_loop();
    }
}

fn run_rpc_worker_loop(node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 4: RPC Server loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();
        {
            let mut ctx = kernel_common::net::NET.lock();
            if let Some(ref mut ctx_val) = *ctx {
                handle_rpc_server(ctx_val, node_arc.clone());
            }
        }

        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 4 (RPC) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }

        iterations += 1;
        core::hint::spin_loop();
    }
}

fn run_stats_worker_loop(node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 5: Maintenance Ticker loop enter\n");
    let mut last_online_weight_tick = kernel_common::clint::mtime();
    let mut last_aec_tick = kernel_common::clint::mtime();
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        let mtime = kernel_common::clint::mtime();

        if mtime - last_aec_tick > seconds(1) {
            let mut guard = node_arc.aec_ticker.lock();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            rsnano_core::utils::Tickable::tick(&mut *guard, &cancel_token);
            last_aec_tick = mtime;
        }

        if mtime - last_online_weight_tick > seconds(2) {
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            node_arc
                .online_weight_calculation
                .lock()
                .tick(&cancel_token);
            last_online_weight_tick = mtime;
        }

        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 5 (Stats) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }

        iterations += 1;
        core::hint::spin_loop();
    }
}
fn run_maintenance_loop(node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 1: Maintenance & Async Executor loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_cleanup = kernel_common::clint::mtime();
    let mut last_flush = kernel_common::clint::mtime();
    let mut last_aec_tick = kernel_common::clint::mtime();
    let mut last_online_weight_tick = kernel_common::clint::mtime();
    let mut last_bootstrap_tick = kernel_common::clint::mtime();
    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_bootstrap_tick > millis(100) {
            node_arc.bootstrapper.run_once();
            node_arc.rep_crawler.run();
            last_bootstrap_tick = mtime;
        }

        if mtime - last_aec_tick > millis(300) {
            let mut guard = node_arc.aec_ticker.lock();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            rsnano_core::utils::Tickable::tick(&mut *guard, &cancel_token);
            last_aec_tick = mtime;
        }

        if mtime - last_heartbeat > seconds(60) {
            last_heartbeat = mtime;
        }
        if mtime - last_cleanup > seconds(5) {
            node_arc.dead_channel_cleanup.lock().clean_up();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            node_arc.peer_cache_connector.lock().tick(&cancel_token);
            last_cleanup = mtime;
        }

        if mtime - last_online_weight_tick > seconds(2) {
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            node_arc
                .online_weight_calculation
                .lock()
                .tick(&cancel_token);
            last_online_weight_tick = mtime;
        }

        if mtime - last_flush > seconds(5) {
            let mut db_guard = kernel_common::storage::DB.lock();
            if let Some(db) = db_guard.as_mut() {
                let _ = db.flush();
            }
            last_flush = mtime;
        }

        node_arc.tick();
        for _ in 0..16 {
            exec.run_once();
        }
        core::hint::spin_loop();
    }
}

fn run_block_processor_loop(node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 2: Block/Cache Processor loop enter\n");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        node_arc.block_processor.run_once();
        node_arc.confirming_set.run_once();
        node_arc.vote_cache_processor.run_once();
        node_arc.backlog_scan.lock().run_once();

        let mtime = kernel_common::clint::mtime();
        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 2 (BlockProc) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }
        iterations += 1;
        core::hint::spin_loop();
    }
}

fn run_consensus_loop(node_arc: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 3: Message/Vote Processor loop enter\n");
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    let mut last_heartbeat = kernel_common::clint::mtime();
    let mut last_throughput_print = kernel_common::clint::mtime();
    let mut votes_start = node_arc
        .vote_processor
        .total_processed
        .load(Ordering::Relaxed);
    let mut msgs_start = {
        let guard = node_arc.message_processor.lock();
        guard.total_processed()
    };
    let mut last_aec_voter_tick = kernel_common::clint::mtime();
    let mut iterations = 0;
    loop {
        node_arc.message_processor.lock().run_once();
        node_arc.vote_processor.run_once();

        let mtime = kernel_common::clint::mtime();
        if mtime - last_aec_voter_tick > millis(500) {
            let mut aec_voter = node_arc.aec_voter.lock();
            let mut aec_ticker = node_arc.aec_ticker.lock();
            let cancel_token = rsnano_core::utils::CancellationToken::new();
            rsnano_core::utils::Tickable::tick(&mut *aec_voter, &cancel_token);
            rsnano_core::utils::Tickable::tick(&mut *aec_ticker, &cancel_token);
            last_aec_voter_tick = mtime;
        }

        // Help with executor during consensus wait periods.
        // Spreading exec.run_once() across harts ensures low-latency response for async tasks.
        for _ in 0..16 {
            exec.run_once();
        }

        let mtime = kernel_common::clint::mtime();
        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 3 (Msg/Vote) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }

        if mtime - last_throughput_print > seconds(60) {
            let votes_now = node_arc
                .vote_processor
                .total_processed
                .load(Ordering::Relaxed);
            let msgs_now = {
                let guard = node_arc.message_processor.lock();
                guard.total_processed()
            };

            if votes_now != votes_start || msgs_now != msgs_start {
                kernel_common::uart::print(b"[hart3] Processed: ");
                kernel_common::uart::print_usize((votes_now - votes_start) as usize);
                kernel_common::uart::print(b" votes, ");
                kernel_common::uart::print_usize((msgs_now - msgs_start) as usize);
                kernel_common::uart::print(b" msgs in 5s\n");
            }

            votes_start = votes_now;
            msgs_start = msgs_now;
            last_throughput_print = mtime;
        }
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
        kernel_common::uart::print(b"[rpc] Connection accepted, processing request...\n");
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

fn run_async_worker_loop(node_arc: Arc<Node>) {
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    loop {
        for _ in 0..256 {
            exec.run_once();
        }
        node_arc.tick();
        core::hint::spin_loop();
    }
}

fn run_monitor_loop(node_mon: Arc<Node>) {
    kernel_common::uart::print(b"[boot] Hart 7: Monitor loop enter\n");
    let mut last_tick = kernel_common::clint::mtime();
    let mut last_heartbeat = kernel_common::clint::mtime();
    let exec = kernel_common::async_rt::EXECUTOR
        .lock()
        .clone()
        .expect("Executor not initialized");
    let mut iterations = 0;

    loop {
        let mtime = kernel_common::clint::mtime();
        if mtime - last_heartbeat > seconds(60) {
            kernel_common::uart::print(b"[heartbeat] Hart 7 (Monitor) | Iters: ");
            kernel_common::uart::print_usize(iterations);
            kernel_common::uart::print(b"\n");
            last_heartbeat = mtime;
            iterations = 0;
        }
        iterations += 1;

        if mtime - last_tick > seconds(5) {
            let (used, _) = {
                let heap = ALLOCATOR.lock();
                (heap.used(), heap.size())
            };
            kernel_common::uart::print(b"[monitor] Heap: ");
            kernel_common::uart::print_usize(used / 1024 / 1024);
            kernel_common::uart::print(b" MB | Tasks: ");
            kernel_common::uart::print_usize(exec.task_count());
            kernel_common::uart::print(b" | Hart7 alive\n");
            node_mon.tick_monitor();
            last_tick = mtime;
        }

        exec.run_once();
        core::hint::spin_loop();
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
