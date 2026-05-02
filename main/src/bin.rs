#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering, AtomicUsize};
use alloc::sync::Arc;
use alloc::string::String;
use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;

use kernel_common::{uart, trap};
use rsnano_node::Node;
use linked_list_allocator::LockedHeap;
use smoltcp::socket::tcp::Socket as TcpSocket;

#[global_allocator]
pub static ALLOCATOR: kernel_common::bump_alloc::BumpAllocator = kernel_common::bump_alloc::BumpAllocator::empty();

global_asm!(include_str!("entry.S"));

static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(999);
static BOOT_COMPLETE: AtomicBool = AtomicBool::new(false);
static NODE: spin::Mutex<Option<NodeWrapper>> = spin::Mutex::new(None);

#[no_mangle]
pub extern "C" fn rust_main(hart_id: usize) {
    if BOOT_HART_ID.compare_exchange(999, hart_id, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        boot_hart_main(hart_id);
    } else {
        secondary_hart_main(hart_id);
    }
}

struct NodeWrapper(Arc<Node>);

unsafe impl Send for NodeWrapper {}
unsafe impl Sync for NodeWrapper {}

fn boot_hart_main(hart_id: usize) {
    uart::print(b"[boot] RS-Nano Bare Metal Node starting (Hart ");
    uart::print_usize(hart_id);
    uart::print(b")\n");

    extern "C" {
        static _heap_start: u8;
        static _heap_end: u8;
    }
    let heap_start = unsafe { &_heap_start as *const u8 as usize };
    let heap_end = unsafe { &_heap_end as *const u8 as usize };
    let heap_len = heap_end - heap_start;
    unsafe {
        ALLOCATOR.init(heap_start, heap_len);
    }
    /*
    uart::print(b"[boot] BSS zeroed, heap ready (");
    uart::print_usize(heap_len / 1024 / 1024);
    uart::print(b" MB)\n");
    uart::print(b"[boot] Enabling trap handlers\n");
    */
    trap::init();
    /*
    uart::print(b"[boot] Waking up secondary cores via SBI HSM...\n");
    */
    for id in 0..8 {
        if id != hart_id {
            let _ = kernel_common::sbi::hart_start(id, 0x80200000, 0);
        }
    }
    kernel_common::logger::init();
 
    #[cfg(feature = "mmode")]
    let seed = kernel_common::clint::mtime();
    #[cfg(not(feature = "mmode"))]
    let seed = kernel_common::sbi::get_time();
    rsnano_nullable_random::set_seed(seed);
    tracing::info!("[node] --- DYNAMIC NODE START ---");
    
    let net_ctx = kernel_common::net::init_virtio_net();
    if let Some(ctx) = net_ctx {
        *kernel_common::net::NET.lock() = Some(ctx);
        // uart::print(b"[net] Global NET initialized.\n");
    }

    let storage_ctx = kernel_common::storage::init_virtio_blk();
    if let Some(storage) = storage_ctx {
        let bpm = kernel_common::storage::bpm::BufferPool::new(storage, 64);
        let btree = kernel_common::storage::btree::BTree::new(bpm);
        *kernel_common::storage::DB.lock() = Some(btree);
        // uart::print(b"[storage] Global DB initialized.\n");
    }

    let exec = kernel_common::async_rt::Executor::new();
    *kernel_common::async_rt::EXECUTOR.lock() = Some(exec);

    // uart::print(b"[nano] Initializing Nano Node...\n");
    let mut config = rsnano_node::config::NodeConfig::default_for(rsnano_core::Networks::NanoLiveNetwork, 8);
    config.network.peer_reachout = core::time::Duration::from_secs(1);
    config.preconfigured_peers.clear();
    config.preconfigured_peers.push(rsnano_core::utils::Peer::new(alloc::string::String::from("192.168.0.245"), 7075));
    config.preconfigured_peers.push(rsnano_core::utils::Peer::new(alloc::string::String::from("37.120.187.138"), 7075));

    // uart::print(b"[boot] Building Nano Node...\n");
    let node = rsnano_node::NodeBuilder::new(rsnano_core::Networks::NanoLiveNetwork)
        .data_path(".")
        .resolver(alloc::sync::Arc::new(BareMetalDnsResolver))
        .config(config)
        .finish()
        .expect("Failed to initialize node");

    // uart::print(b"[boot] Starting Nano Node...\n");
    let mut node = node;
    node.start();
    let node_arc = alloc::sync::Arc::new(node);
    {
        *NODE.lock() = Some(NodeWrapper(node_arc.clone()));
    }
    // uart::print(b"[boot] Nano Node started.\n");

    BOOT_COMPLETE.store(true, Ordering::SeqCst);
    
    {
        let mut db_guard = kernel_common::storage::DB.lock();
        if let Some(db) = db_guard.as_mut() {
            let _ = db.flush();
        }
    }

    let node_mon = node_arc.clone();
    kernel_common::async_rt::spawn(async move {
        loop {
            kernel_common::async_rt::sleep(core::time::Duration::from_secs(60)).await;
            node_mon.tick_monitor();
            let (used, total) = (ALLOCATOR.used(), ALLOCATOR.total());
            let blocks = node_mon.ledger.block_count();
            let accounts = node_mon.ledger.account_count();
            let channels = node_mon.network.read().count();
            let tasks = kernel_common::async_rt::EXECUTOR.lock().as_ref().map(|e| e.task_count()).unwrap_or(0);
            let queue = node_mon.block_processor_queue.total_queue_len();
            tracing::info!(
                "[monitor] Heap: {}/{} KB, Blocks: {}, Accounts: {}, Channels: {}, Tasks: {}, Queue: {}",
                used / 1024, total / 1024, blocks, accounts, channels, tasks, queue
            );
        }
    });

    /*
    uart::print(b"[boot] Entering main loop (Hart ");
    uart::print_usize(hart_id);
    uart::print(b")\n");
    */

    loop {
        #[cfg(feature = "mmode")]
        let mtime = kernel_common::clint::mtime();
        #[cfg(not(feature = "mmode"))]
        let mtime = kernel_common::clint::get_time_offset_ns() / 100 + kernel_common::clint::mtime();
        {
            if let Some(mut ctx) = kernel_common::net::NET.try_lock() {
                if let Some(ref mut ctx_val) = *ctx {
                    ctx_val.poll(mtime);
                }
            }
        }
        
        // Run background node tasks
        node_arc.block_processor.run_once();
        node_arc.confirming_set.run_once();
        node_arc.message_processor.lock().run_once();

        let exec = kernel_common::async_rt::EXECUTOR.lock().clone();
        if let Some(exec) = exec {
            exec.run_ready_tasks();
        }
        if let Some(mut ctx) = kernel_common::net::NET.try_lock() {
            if let Some(ref mut ctx_val) = *ctx {
                handle_rpc_server(ctx_val, &node_arc);
            }
        }
        core::hint::spin_loop();
    }
}

fn secondary_hart_main(_hart_id: usize) {
    loop {
        core::hint::spin_loop();
    }
}

fn handle_rpc_server(ctx: &mut kernel_common::net::NetworkContext, node: &Node) {
    let socket = ctx.sockets.get_mut::<TcpSocket>(ctx.rpc_handle);
    if !socket.is_open() { let _ = socket.listen(7076); }
    if socket.can_recv() {
        let mut response: Option<alloc::string::String> = None;
        let _ = socket.recv(|buffer| {
            if buffer.starts_with(b"GET /telemetry") {
                let blocks = node.ledger.block_count();
                let accounts = node.ledger.account_count();
                let resp_str = alloc::format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"blocks\":{}, \"accounts\":{}}}\n",
                    blocks, accounts
                );
                response = Some(resp_str);
                (buffer.len(), ())
            } else {
                response = Some(alloc::string::String::from("HTTP/1.1 404 Not Found\r\n\r\n"));
                (buffer.len(), ())
            }
        });
        if let Some(resp_str) = response {
            let _ = socket.send_slice(resp_str.as_bytes());
            socket.close();
        }
    }
}

struct BareMetalDnsResolver;
impl rsnano_network::AsyncDnsResolver for BareMetalDnsResolver {
    fn resolve(&self, host: &str) -> Pin<Box<dyn Future<Output = anyhow::Result<alloc::vec::Vec<core::net::IpAddr>>> + Send>> {
        // Try parsing as IP first to avoid DNS query
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
                                ips.push(core::net::IpAddr::V4(core::net::Ipv4Addr::new(b[0], b[1], b[2], b[3])));
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

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    uart::print(b"[PANIC] system panicked\n");
    if let Some(loc) = info.location() {
        uart::print(b" at ");
        uart::print(loc.file().as_bytes());
        uart::print(b":");
        uart::print_usize(loc.line() as usize);
    }
    uart::print(b"\n");
    // Print message if available
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        uart::print(b"Message: ");
        uart::print(s.as_bytes());
        uart::print(b"\n");
    }
    loop {}
}
