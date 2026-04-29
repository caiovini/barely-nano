#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::panic::PanicInfo;
use kernel_common::{trap, uart};
use linked_list_allocator::LockedHeap;

// ---------------------------------------------------------------------------
// Global heap allocator
// ---------------------------------------------------------------------------

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// ---------------------------------------------------------------------------
// Linker-provided symbols
// ---------------------------------------------------------------------------

unsafe extern "C" {
    static _bss_start: u8;
    static _bss_end: u8;
    static _heap_start: u8;
    static _heap_end: u8;
}

// ---------------------------------------------------------------------------
// Assembly entry: set stack pointer, jump to Rust
// ---------------------------------------------------------------------------

global_asm!(
    ".section .text.start",
    ".global _start",
    "_start:",
    "    la    sp, _stack_top",    // set stack pointer
    "    csrci sstatus, 2",        // clear sstatus.SIE (bit 1) — csrci max imm = 31, bit1=2 OK
    "    li    t0, 0x20",          // STIE = bit 5 of sie = 32; must use register form
    "    csrc  sie, t0",           // clear sie.STIE
    "    j     kernel_main",
);

// ---------------------------------------------------------------------------
// Kernel main
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn kernel_main(_hart_id: usize, _dtb: usize) -> ! {
    // 1. Zero BSS
    unsafe {
        let start = &_bss_start as *const u8 as *mut u8;
        let end = &_bss_end as *const u8;
        let len = end.offset_from(start) as usize;
        core::ptr::write_bytes(start, 0, len);
    }

    // 2. Initialise heap
    unsafe {
        let start = &_heap_start as *const u8 as usize;
        let end = &_heap_end as *const u8 as usize;
        ALLOCATOR.lock().init(start as *mut u8, end - start);
    }

    uart::print(b"[boot] BSS zeroed, heap ready (4 MB)\n");
    uart::print(b"[boot] Enabling CLINT timer interrupts (1 s)\n");

    trap::init();

    let net_ctx = kernel_common::net::init_virtio_net();
    if let Some(ctx) = net_ctx {
        *kernel_common::net::NET.lock() = Some(ctx);
        uart::print(b"[net] Global NET initialized.\n");
    }

    let storage_ctx = kernel_common::storage::init_virtio_blk();
    if let Some(storage) = storage_ctx {
        let bpm = kernel_common::storage::bpm::BufferPool::new(storage, 10);
        uart::print(b"[bpm] Initialized Buffer Pool with 10 frames\n");

        let mut btree = kernel_common::storage::btree::BTree::new(bpm);
        uart::print(b"[btree] Initialized Paged B-Tree\n");

        uart::print(b"[btree] Inserting keys 1 to 100...\n");
        for i in 1..=100 {
            let mut key = [0u8; 65];
            key[0] = 255;
            let i_bytes = (i as u64).to_le_bytes();
            key[1..9].copy_from_slice(&i_bytes);

            let mut val = [0u8; 256];
            let val_bytes = (i * 10 as u64).to_le_bytes();
            val[0..8].copy_from_slice(&val_bytes);

            btree.insert(key, val);
        }

        uart::print(b"[btree] Searching for key 42...\n");
        let mut search_key = [0u8; 65];
        search_key[0] = 255;
        search_key[1..9].copy_from_slice(&42u64.to_le_bytes());

        if let Some(val) = btree.search(&search_key) {
            let found_val = u64::from_le_bytes(val[0..8].try_into().unwrap());
            if found_val == 420 {
                uart::print(b"[btree] SUCCESS: Found key 42 with value 420!\n");
            } else {
                uart::print(b"[btree] FAILURE: Incorrect value found\n");
            }
        } else {
            uart::print(b"[btree] FAILURE: Key 42 not found\n");
        }

        let mut fail_key = [0u8; 65];
        fail_key[0] = 255;
        fail_key[1..9].copy_from_slice(&999u64.to_le_bytes());
        if btree.search(&fail_key).is_none() {
            uart::print(b"[btree] SUCCESS: Key 999 correctly not found.\n");
        }

        *kernel_common::storage::DB.lock() = Some(btree);
        uart::print(b"[storage] Global DB initialized.\n");
    }

    uart::print(b"[crypto] Running Blake2b Hash test...\n");
    let test_data = b"nano_block_data";
    let hash = kernel_common::crypto::hash_blake2b(test_data);
    
    uart::print(b"[crypto] Blake2b(\"nano_block_data\") = ");
    for byte in &hash {
        let hex_chars = b"0123456789abcdef";
        let hi = (byte >> 4) as usize;
        let lo = (byte & 0x0F) as usize;
        let mut hex_buf = [hex_chars[hi], hex_chars[lo]];
        uart::print(&hex_buf);
    }
    uart::print(b"\n");

    uart::print(b"[crypto] Checking hardware extensions...\n");
    kernel_common::crypto::test_hw_extensions();

    let exec = kernel_common::async_rt::Executor::new();
    *kernel_common::async_rt::EXECUTOR.lock() = Some(exec);

    uart::print(b"[boot] Entering async executor loop\n");

    loop {
        if let Some(ref mut ctx) = *kernel_common::net::NET.lock() {
            #[cfg(feature = "mmode")]
            let mtime = kernel_common::clint::mtime();
            #[cfg(feature = "smode")]
            let mtime = kernel_common::clint::mtime(); 
            
            ctx.poll(mtime);
        }
        
        if let Some(exec) = kernel_common::async_rt::EXECUTOR.lock().as_ref() {
            exec.run_ready_tasks();
        }
        
        unsafe { core::arch::asm!("wfi", options(nostack)) };
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    uart::print(b"[PANIC]\n");
    loop {}
}
