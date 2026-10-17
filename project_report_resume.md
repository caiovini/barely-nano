# barely-nano — Project Resume

> A bare-metal Nano protocol node written in `no_std` Rust, targeting the
> **OrangePi RV2** (SpacemiT K1 SoC, 8× RISC-V X60 harts, 8 GB RAM, 1 TB NVMe SSD).

---

## 1. Mission

Migrate the RSNano reference node from a Linux/std environment to a fully
self-contained, OS-free binary that boots directly from U-Boot on RISC-V
hardware. Protocol semantics are preserved verbatim from RSNano; the migration
is structural, not a redesign.

**Priority order:** Protocol correctness → RSNano compatibility → Stability →
Memory efficiency → SSD endurance → CPU throughput.

---

## 2. High-Level Architecture

```
Nano Protocol (rsnano-node crates)
         │
         ▼
  Multi-Hart Executor  (8 harts, work-stealing)
         │
    ┌────┴────────────────────────────────────┐
    │  Network Stack   │  Consensus / Ledger  │
    │  (smoltcp)       │  (rsnano-node)       │
    └────┬─────────────┴────────┬─────────────┘
         │                      │
    U-Boot Ethernet         B-Tree Storage Engine
    driver (raw Ethernet)        │
         │                  Buffer Pool Manager (BPM)
         │                       │
    NVMe SSD via             U-Boot block API
    U-Boot Syscall API       (dev_read / dev_write)
         │                       │
    ─────────────────── NVMe SSD (1 TB) ─────────────────────
```

---

## 3. Boot Sequence

| Phase | Who | What happens |
|-------|-----|--------------|
| SPL / U-Boot | Bootloader | Loads `barely-nano.bin` at known address |
| `entry.S` | Hart 1 | Sets up stack per-hart, calls `rust_main` |
| `rust_main` | Hart 1 | Zeros BSS, inits U-Boot JT + API, inits heap |
| `boot_hart_main` | Hart 1 | PMIC WDT disable, trap vector, logger, seed, smoltcp, storage shards, node build, `node.start()` |
| Cache flush | Hart 1 | `cbo.flush` over entire heap + BSS, sets `HART_DISPATCH_READY` |
| Secondary harts (2–7) | harts | Wait on `HART_DISPATCH_READY` (SeqCst + `cbo.inval` on flag), init own trap vectors, grab `Arc<Node>` + `Executor` from globals, enter per-role task loop |

> **Why not pass Arc via opaque register?** OpenSBI on SpacemiT K1 does not
> reliably relay `a1` to woken harts, and `Arc::into_raw` produces the same
> allocation pointer for all clones, corrupting the refcount. The safe path is
> sharing the Arc through a global `Mutex<Option<NodeWrapper>>`.

---

## 4. Multi-Hart Task Assignment

Each hart runs a dedicated tight loop with no OS scheduler:

| Hart | Role |
|------|------|
| 0 | Heavy I/O (network polling + async executor) |
| 1 | Boot hart → Maintenance, network, AEC, peer cache, async executor |
| 2 | Block Processor + periodic BTree flush every 5 s |
| 3 | Consensus loop (AEC voter + AEC ticker @ 100 ms, async executor) |
| 4 | RPC server (port 7076, smoltcp TCP) |
| 5 | Vote Processor + signature verification helper |
| 6 | Heavy I/O (mirrors Hart 0) |
| 7 | Monitor / stats loop |

Every hart calls `signature_checker.try_help_verify()` where idle, spreading
ed25519 batch verification across all cores.

---

## 5. RISC-V Hardware Optimizations

### 5.1 Compile-Time CPU Extensions

Production build (`make build` / `make build-smode`):

```bash
RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom"
```

| Extension | Purpose |
|-----------|---------|
| `+v` | RISC-V Vector 1.0 (SIMD for hashing / crypto) |
| `+zbb` | Bit-Manipulation Base — `rori`, `clz`, `cpop`, `rev8` etc. Blake2b rotations → single `rori` instruction |
| `+zba` | Address-Gen Bit-Manipulation — `sh1add`, `sh2add`, `sh3add` |
| `+zbs` | Single-Bit Manipulation — `bset`, `bclr`, `bext` |
| `+zbc` | Carry-less Multiplication — CRC / GF operations |
| `+zihintpause` | `pause` hint → replaces `nop` spin loops, reduces power & bus pressure |
| `+zicboz` | Cache-Block Zero — `cbo.zero` for fast BSS/buffer clearing |
| `+zicbom` | Cache-Block Management — `cbo.clean`, `cbo.flush`, `cbo.inval` for explicit cross-cluster coherency |

### 5.2 Blake2b / Crypto Optimization

- All Blake2b calls go through `rsnano_sha::blake2b::hash`.
- The comment in `crypto/mod.rs` explicitly notes: *"every rotation compiles to a single Zbb `rori` instruction"*.
- The `simd` feature gate enables the vectorized code path at build time.
- `sstatus.FS` (bits 14:13) and `sstatus.VS` (bits 10:9) are set to `Dirty` (11) during trap init so that the CPU does not trap on FP/vector instructions.

### 5.3 Cache Coherency (Cross-Cluster)

SpacemiT K1 has two clusters (Cluster 0 = harts 0–3, Cluster 1 = harts 4–7)
without automatic CCI. Cache coherency is achieved manually:

```rust
// Zicbom: clean (write-back) a range to DRAM
fn clean_cache_range(addr: usize, len: usize) {
    // 64-byte cache-line aligned loop
    core::arch::asm!("cbo.clean ({0})", in(reg) current, ...);
    core::arch::asm!("fence rw, rw", ...);
}

// Zicbom: invalidate (discard) a range from L1/L2
fn invalidate_cache_range(addr: usize, len: usize) {
    core::arch::asm!("cbo.inval ({0})", in(reg) current, ...);
    core::arch::asm!("fence r, r", ...);
}
```

Used in:
- BSS zeroing after boot (Hart 1 cleans, secondary harts rely on the SeqCst barrier)
- Heap flush before waking secondary harts
- `HART_DISPATCH_READY` flag polling in secondary hart spin loops

> **cbo.inval note:** Requires `menvcfg.CBIE` set by OpenSBI. On this SoC it
> is NOT set, so secondary harts use only `cbo.clean` (always S-mode-permitted)
> and rely on the SeqCst barrier + boot-hart cache flush.

---

## 6. Storage Engine

### 6.1 Overview

No LMDB, RocksDB, or POSIX APIs. The ledger lives on NVMe via a custom
sector-addressable storage stack:

```
Ledger Store (rsnano-node API)
        │
  16 Sharded B-Trees   (parallel prefix-sharded key space)
        │
  Buffer Pool Manager  (128-frame LRU per shard)
        │
  StorageContext       (UBoot NVMe | RAM disk fallback)
        │
  U-Boot block API    (dev_read / dev_write → NVMe SSD)
```

### 6.2 Storage Layout on Disk

```
LBA 0 → 1 048 575        : U-Boot boot partition (reserved, never touched)
LBA 1 048 576+            : barely-nano ledger data
  Page 0                  : Global shard header (magic 0x5348415244534846, shard_size, shard_count)
  For each shard i (0..15):
    base_page = 1 + i * (shard_size + 1)
    Page base_page + 0    : B-Tree metadata page (magic 0xBEEFCB02, root_page_id, next_free_page, free_list_head)
    Pages base_page + 1…  : B-Tree nodes (4 KB each)
```

**Block size:** 4 096 bytes = 8 × 512-byte LBA sectors.

### 6.3 Buffer Pool Manager (BPM)

File: `common/src/storage/bpm.rs`

| Property | Value |
|----------|-------|
| Frame count per shard | 128 |
| Frame size | 4 096 bytes (`AlignedBlock`, `repr(align(64))`) |
| Eviction policy | LRU with clean-first preference |
| Page table | `BTreeMap<page_id, frame_id>` |
| Dirty tracking | per-frame `is_dirty` flag |
| Pin count | per-frame `pin_count: u32` |

**`PageGuard`** — RAII handle returned by `fetch_page()`. Calls `unpin()` on
drop, propagating the dirty flag automatically.

**LRU algorithm:**
1. Cache hit → `mark_mru()` (O(n) scan, acceptable for 128 frames).
2. Cache miss → scan LRU queue for unpinned clean frame first, then unpinned dirty frame.
3. Dirty victim → flushed to disk before reuse.
4. `flush_all()` → writes all dirty frames; called every 5 s from Hart 2 and at shutdown.

**Backends:**
- `StorageBackend::UBoot(DeviceInfo)` — real NVMe via U-Boot syscall API.
- `StorageBackend::RamDisk(Vec<u8>)` — 128 MB in-RAM fallback if no storage found.

### 6.4 B-Tree Engine

File: `common/src/storage/btree.rs`

| Constant | Value | Rationale |
|----------|-------|-----------|
| `BLOCK_SIZE` | 4 096 B | Matches NVMe sector size |
| `MAX_KEYS` | 12 | Keys + values + children fit in one 4 KB page |
| `MIN_KEYS` | 4 | Standard B-Tree minimum (MAX_KEYS / 2 − 1) |
| `KEY_SIZE` | 65 B | 1-byte table ID + 64-byte key |
| `VALUE_SIZE` | 256 B | Sufficient for all Nano ledger record types |

**Node layout on disk (4 KB page):**
```
Offset  0       : is_leaf (1 byte)
Offset  1–3     : num_keys (u16 LE + 1 pad byte)
Offset  4       : keys[0..MAX_KEYS]  (12 × 65 B = 780 B)
Offset  784     : values[0..MAX_KEYS] (12 × 256 B = 3072 B)
Offset  3856    : value_lengths[0..MAX_KEYS] (12 × 2 B = 24 B)
Offset  3880    : children[0..MAX_KEYS+1] (13 × 4 B = 52 B)
                  Total = 3932 B < 4096 B ✓
```

**Operations:**
- `search()` — iterative descent with binary search at each node. O(log n) page reads.
- `insert()` — proactive split (root split grows tree height). Handles in-place update for existing keys at both leaf and internal nodes.
- `delete()` — predecessor/successor replacement for internal keys; borrow-from-sibling or merge for underflowing nodes; page freed to free-list on merge.
- `iter(start_key)` → `RangeIterator` — stack-based in-order traversal; nodes cached between `next_kv()` calls to reduce BPM pressure.
- `count_prefixed(prefix)` — table-scoped counting; kicks watchdog every 1 000 entries to prevent PMIC WDT reset during large scans.

**Free page management:** Merged or deleted pages are prepended to a singly-linked free list embedded in the page data itself (first 4 bytes = next free page ID). `allocate_page()` checks this list first.

**Metadata persistence:** `save_meta()` writes to Page 0 via BPM (dirty-flagged), so it is flushed lazily with the rest of the pool.

### 6.5 Sharding

`SHARD_COUNT = 16` shards, selected by the top nibble of `key[1]` (the first byte after the table-ID byte). This gives prefix-ordered shards so range scans remain contiguous within a shard. Each shard has its own `Arc<Mutex<StorageContext>>` and `BufferPool`, allowing concurrent access from multiple harts without cross-shard contention.

---

## 7. Network Stack

- **Driver:** `UbootNetDevice` — raw Ethernet via U-Boot `dev_read` / `dev_write` syscalls. No kernel, no socket OS abstraction.
- **TCP/IP:** `smoltcp` 0.11 in no_std/no_os mode.
- **Static IP:** `192.168.0.200/24`, gateway `192.168.0.1`.
- **Sockets managed:**
  - Peering listener — port 7075 (Nano protocol TCP)
  - RPC listener — port 7076 (JSON-RPC)
  - ICMP socket — ping diagnostics
  - DNS socket — Cloudflare 1.1.1.1
  - UDP socket — general use
- **Async TCP:** `AsyncTcpStream` adapts smoltcp sockets to the custom `Executor`'s async model.
- **DNS resolver:** `BareMetalDnsResolver` tries literal IP parse first, then calls `kernel_common::net::resolve()` (async DNS via smoltcp DNS socket).

---

## 8. Async Runtime

File: `common/src/async_rt.rs`

A minimal `no_std`-compatible cooperative executor:

- **8 per-hart local queues** (`VecDeque<Arc<Task>>`).
- **1 global injector queue** for newly spawned tasks with no affinity.
- **Work stealing:** `pop_task()` checks local → injector → steals from other queues (back of victim queue, `try_lock` only to avoid deadlock).
- **Task states:** `IDLE`, `QUEUED`, `SUSPENDED`.
- **Waker:** `Arc<Task>` implements `Wake`; `wake_by_ref()` re-enqueues the task to its assigned hart's local queue.
- **`Sleep` future:** cooperative yield based on `rdtime` (CLINT); no preemptive timer interrupt needed.
- **`YieldNow` future:** single round-trip through the executor to let other tasks run.

---

## 9. Hardware Drivers

### 9.1 U-Boot Integration (`common/src/uboot.rs`)

Bridges Rust to U-Boot's C driver layer without a full OS:

- **Jump Table (JT):** `putc`, `puts`, `printf`, `malloc`/`free`, `udelay`, `get_timer`, `env_get`/`env_set`, I2C R/W.
- **Syscall API:** `dev_enum`, `dev_open`, `dev_close`, `dev_read`, `dev_write` — used for both NVMe and Ethernet.
- **GP register swap:** Every call into U-Boot saves/restores the `gp` (global pointer) register via inline assembly to prevent U-Boot's C globals from clobbering Rust's.
- **Dynamic API discovery:** Boot hart scans `0x7b000000`–`0x7f000000` for the `"UBootAPI"` magic string; falls back to env var `api_address`; then hardcoded address.

### 9.2 PMIC Watchdog (`common/src/pmic_wdt.rs`)

The SPM8821 PMIC has a hardware WDT (60 s timeout) independent of the SoC WDT:

- Accessed via PWR I2C bus 8 (MMIO `0xd401d800`).
- APBC clock gate for I2C8 at `0xd4015000 + 0x20` is enabled before use.
- `disable()` clears bit 3 of register `0x44` via raw I2C read-modify-write.
- `kick()` sets/clears the clear-status bit (bit 0) to reset the countdown.
- Called from Hart 1 and Hart 2 every second, and during long storage scans.

### 9.3 CLINT / Timer (`common/src/clint.rs`)

- `mtime()` / `mtime_raw()` — reads the RISC-V `time` CSR.
- `FREQ` — used for tick-to-duration conversion in `Sleep` futures and heartbeat timing.
- M-mode also uses `set_mtimecmp()` for the 10 Hz timer interrupt.

### 9.4 Trap Handler (`common/src/trap.rs`)

- Assembly trap vector for both M-mode and S-mode (conditional compilation).
- Saves/restores all 31 GPRs on the stack.
- Timer interrupt → increments `TICK_COUNT`, kicks watchdog on Hart 1, schedules next tick.
- Exception → prints hart ID, `xcause`, `sepc`, `ra` via lock-free UART; halts on fatal causes.
- `sstatus.FS | VS` set to `Dirty` on init → enables FP + vector without trapping.

---

## 10. Cryptography

- **Blake2b-256:** `rsnano_sha::blake2b::hash` — optimized implementation where every 64-bit rotation compiles to a single `rori` instruction (Zbb extension).
- **Ed25519:** reused from `ed25519-dalek` reference, no modifications to signature format, key derivation, or output.
- **Signature verification:** distributed across harts via `signature_checker.try_help_verify()` called in every hart's idle path.
- **Hardware detection:** `test_hw_extensions()` uses `#[cfg(target_feature)]` instead of reading `misa` (which traps in S-mode).

---

## 11. Memory Management

- **Heap:** `linked_list_allocator::LockedHeap` wrapped in `InterruptSafeAllocator` (disables interrupts during `alloc`/`dealloc` to prevent nested lock acquisition).
- **Heap range:** defined by linker symbols `_heap_start` / `_heap_end` (in `linker-smode.ld`).
- **`AlignedBlock`:** `repr(align(64))` — ensures DMA-friendly 64-byte cache-line alignment for all 4 KB storage blocks.
- **No std:** entire binary is `#![no_std] #![no_main]`; only `core` and `alloc` used.

---

## 12. Build System

```makefile
build:      build-smode          # Default: S-mode + all RISC-V extensions
build-smode:                     # Produces barely-nano.bin, auto-copies to SD if mounted
build-mmode:                     # QEMU M-mode, no extensions (for emulation testing)
build-spammer:                   # S-mode + spammer feature for B-Tree throughput testing
run-smode / run-spammer:         # QEMU with full extension flags: v,zbb,zba,zbs,zbc,zicbom,zicboz
```

**Deployment:** `llvm-objcopy -O binary` → `barely-nano.bin` → copied to SD card mount `/run/media/cvini/NANO_NODE/` automatically if present.

**Logging:** Live serial via `make run-picocom` → `picocom -b 115200 /dev/ttyUSB0 > orpi.log`.

---

## 13. Node Configuration Highlights

```rust
config.online_weight_minimum  = 130_000_000 NANO
config.enable_voting           = true
config.enable_vote_processor   = true
config.bootstrap.rate_limit    = 10_000
config.bootstrap.max_requests  = 16_384
config.block_processor.max_peer_queue   = 65_536
config.block_processor.batch_size       = 2_048
config.vote_processor.batch_size        = 4_096
config.vote_processor.max_triggered     = 65_536
```

Peers bootstrapped: `37.120.187.138:7075`, `94.130.135.50:7075`, `192.168.0.244:7075` (local synced node).

---

## 14. Current Status (as of 2026-06-14)

| Subsystem | Status |
|-----------|--------|
| Boot + multi-hart init | ✅ Stable (harts 1–7 running) |
| U-Boot API integration | ✅ Dynamic discovery working |
| PMIC WDT disable/kick | ✅ Operational |
| smoltcp networking | ✅ TCP connections established |
| Nano NodeIdHandshake | 🔧 `X` validation failures under investigation (latest conversation) |
| Network Realtime mode | 🔧 Peers transitioning, handshake stabilization ongoing |
| B-Tree + BPM storage | ✅ Operational; shards init correctly; flush every 5 s |
| Ledger block sync | 🔄 Bootstrap in progress |
| RPC server (7076) | ✅ Functional |
| Ed25519 / Blake2b | ✅ Protocol-compatible, Zbb-optimized |
| Cross-cluster coherency | ✅ cbo.flush + SeqCst barriers working |
