# barely-nano — Project Resume

> A bare-metal Nano protocol node written in `no_std` Rust, targeting the
> **OrangePi RV2** (SpacemiT K1 / Ky X1 SoC, 8× RISC-V X60 harts, 8 GB RAM, 1 TB NVMe SSD).

---

## 1. Mission

Migrate the RSNano reference node from a Linux/`std` environment to a fully
self-contained, OS-free binary that boots directly from U-Boot on RISC-V
hardware. Protocol semantics are preserved verbatim from RSNano; the migration
is structural, not a redesign.

**Priority order:** Protocol correctness → RSNano compatibility → Stability →
Memory efficiency → SSD endurance → CPU throughput.

---

## 2. High-Level Architecture

```
SD Card (MMC — Boot Media)
  └── SPL / U-Boot + boot.scr + barely-nano.bin + config.toml

Nano Protocol (rsnano_node, rsnano_ledger, rsnano_core, rsnano_messages)
         │
         ├──► RPC Server (port 7076) & WebSocket Server (port 7078)
         ▼
  Multi-Hart Runtime (Harts 1–7 dedicated roles + work-stealing async executor)
         │
    ┌────┴────────────────────────────────────────┐
    │  Network Stack       │  Consensus / Ledger  │
    │  (smoltcp 0.12)      │  (Single-Writer H6)  │
    └────┬─────────────────┴──────────┬───────────┘
         │                            │
    U-Boot Ethernet            16 Sharded B-Trees
    driver (raw Ethernet)             │
         │                     Buffer Pool Manager (5 GB in DRAM Bank 1)
         │                            │
         │                     Atomic Dual-Area Write-Ahead Log (WAL)
         │                            │
         └──────────────► U-Boot Syscall API (dev_read / dev_write)
                                      │
                        ───── NVMe SSD (1 TB — Ledger Storage) ─────
```

---

## 3. Boot Sequence

| Phase | Who | What happens |
|-------|-----|--------------|
| SPL / U-Boot | Bootloader | Boots from **SD Card (MMC)**, executes `boot.scr`, and loads `barely-nano.bin` and `config.toml` from the SD card into DRAM |
| `entry.S` | Hart 1 | Sets up per-hart stack, saves U-Boot registers, calls `rust_main` |
| `rust_main` | Hart 1 | Disables PMIC watchdog early, dynamically resolves U-Boot API signature (`env_get("api_address")` or RAM scan), initializes U-Boot Jump Table |
| `boot_hart_main` | Hart 1 | Initializes heap (`LockedHeap`), trap vector, logger, RNG seed, OLED display, parses `config.toml` (loaded from SD card), initializes `smoltcp` network stack, probes **NVMe SSD** for 16 B-Tree storage shards + WAL recovery, builds `Node`, starts RPC & WebSocket servers, connects preconfigured peers |
| Cache flush | Hart 1 | Executes `cbo.clean` over kernel image, active heap, and BSS to DRAM; sets `NODE_INIT_COMPLETE` and `HART_DISPATCH_READY` |
| Secondary harts (2–7) | Harts 2–7 | Started via `sbi::hart_start` + manual wakeup IPI; wait on `HART_DISPATCH_READY`, initialize per-hart trap vectors, acquire `Arc<Node>` + `Executor`, enter dedicated per-hart loops |

> **Why not pass `Arc<Node>` via SBI opaque register?** OpenSBI on SpacemiT K1 does not
> reliably relay `a1` to woken harts, and `Arc::into_raw` produces the same
> allocation pointer for all clones without incrementing the reference count per hart.
> Sharing via the global `Mutex<Option<NodeWrapper>>` after explicit cross-cluster cache
> cleaning guarantees safe reference counting and visibility across Cluster 0 and Cluster 1.

---

## 4. Multi-Hart Task Assignment

Each hart runs a dedicated tight loop (`main/src/bin.rs`) with adaptive `pause` backoff (`+zihintpause`):

| Hart | Cluster | Role |
|------|---------|------|
| **0** | Cluster 0 | Reserved / skipped (known OpenSBI start quirk on SpacemiT K1) |
| **1** | Cluster 0 | **Boot, Maintenance & Network (`Maint+Net`):** `smoltcp` interface polling, HTTP JSON-RPC server (port 7076), delegated NVMe block I/O execution for worker harts, gateway ICMP keepalive, watchdog kick |
| **2** | Cluster 0 | **Crypto Worker 1 (`Crypto-1`):** Lock-free SIMD Ed25519 batch verification (`signature_checker.help_verify()`) and inbound `MessageProcessor::run_batch()` |
| **3** | Cluster 0 | **Crypto Worker 2 (`Crypto-2`):** Mirrors Hart 2 — SIMD Ed25519 batch verification and `MessageProcessor::run_batch()` |
| **4** | Cluster 1 | **Consensus & Elections (`Consensus`):** Drives `aec_voter` and `aec_ticker` every 300 ms, polls async executor tasks (`exec.run_once(4)`) |
| **5** | Cluster 1 | **Vote Processor (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and polls async executor tasks (`exec.run_once(5)`) |
| **6** | Cluster 1 | **Single-Writer Ledger Commit & Bootstrap (`Ledger+Boot`):** Runs `block_processor`, `unchecked`, `confirming_set`, `election_schedulers`, `backlog_scan`, atomic WAL checkpoints (every 10 s or on BPM demand), periodic `node.tick()`, and `bootstrap_server` |
| **7** | Cluster 1 | **Monitor, OLED & Async Worker (`Storage+Mon`):** Runs `online_weight_calculation`, updates hardware I2C OLED display every 2 s, prints 15 s system/storage/lock telemetry, and polls async executor tasks (`exec.run_once(7)`) |

---

## 5. RISC-V Hardware Optimizations

### 5.1 Compile-Time CPU Extensions

Production build (`make build` / `make build-smode`):

```bash
RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause"
```

| Extension | Purpose |
|-----------|---------|
| `+v`, `+zvl256b` | RISC-V Vector 1.0 with 256-bit vector registers (RVV SIMD for Blake2b & Ed25519) |
| `+zbb` | Bit-Manipulation Base — `rori`, `clz`, `cpop`, `rev8` (Blake2b 64-bit rotations compile to single `rori`) |
| `+zba` | Address-Generation Bit-Manipulation — `sh1add`, `sh2add`, `sh3add` |
| `+zbs` | Single-Bit Manipulation — `bset`, `bclr`, `bext` |
| `+zbc` | Carry-less Multiplication |
| `+zihintpause` | `pause` instruction in spin/backoff loops — reduces interconnect contention |
| `+zicboz` | Cache-Block Zero (`cbo.zero`) |
| `+zicbom` | Cache-Block Management (`cbo.clean`, `cbo.flush`, `cbo.inval`) for cross-cluster coherency |
| `Zicbop` | Cache-line read prefetch hints (`prefetch.r` via inline `ori zero, rs1, 1`) in B-Tree binary search |

### 5.2 Blake2b & Ed25519 Cryptographic Acceleration (`nullables/sha`)

- **Blake2b:** Vectorized RVV 1.0 + Zbb implementation (`rsnano_sha::blake2b`), verified at boot via deterministic self-test. Supports 4-way batch PoW difficulty evaluation (`get_difficulties_batch_4`).
- **Ed25519:** Precomputed Radix-16 scalar multiplication table (`init_radix16_precomp`) and multi-hart batch verification distributed across Harts 2 and 3.
- **S-Mode Vector/FP Enablement:** `sstatus.FS` and `sstatus.VS` are initialized to `Dirty` (`11`) in `trap::init()` on every hart so vector instructions execute without trapping.

### 5.3 Cross-Cluster Cache Coherency

SpacemiT K1 splits harts across Cluster 0 (Harts 0–3) and Cluster 1 (Harts 4–7). Before waking secondary harts, Hart 1 cleans the kernel image, heap, and BSS to DRAM using 64-byte cache-line `cbo.clean` loops followed by `fence rw, rw`.

---

## 6. Storage Engine

### 6.1 Architecture Overview

Boot artifacts (`SPL`, `U-Boot`, `boot.scr`, `barely-nano.bin`, and `config.toml`) reside on the **SD Card (`MMC`)**, whereas the **1 TB NVMe SSD** is dedicated exclusively to Nano ledger storage. At startup, `init_uboot_blk()` enumerates U-Boot storage devices and selects the NVMe SSD (`if_type != 6 && if_type != 7`), ignoring the SD/MMC boot card (using it only as a fallback if no NVMe drive is attached).

The ledger operates directly on NVMe sectors without LMDB, RocksDB, or an OS filesystem:

```
Ledger Store (rsnano_ledger_store: block, account, pending, confirmation_height, rep_weight, peer, ...)
        │
  16 Prefix-Sharded B-Trees (`common/src/storage/btree.rs`)
        │
  Buffer Pool Manager (`common/src/storage/bpm.rs` — 5.0 GB in DRAM Bank 1)
        │
  Write-Ahead Log (`common/src/storage/wal.rs` — Dual 128 MB areas, atomic checkpoints)
        │
  StorageContext (`common/src/storage/mod.rs` — 32-bit DMA bounce buffer for Bank 1 frames)
        │
  U-Boot Block API (`dev_read` / `dev_write` → 1 TB NVMe SSD)
```

### 6.2 Disk Layout

- **SD Card (`MMC`):** Contains the U-Boot boot partition and FAT/ext filesystem where `make build` deploys `barely-nano.bin`, `config.toml`, and `boot.scr`.
- **1 TB NVMe SSD (Ledger Storage):**
  - **LBA `0 .. 524,287` (first 256 MB):** Unused safety margin (`STORAGE_BASE_LBA = 1,048_576` ensures that even if the node ever booted without an NVMe drive and fell back to the SD card, it would never overwrite U-Boot or boot files).
  - **LBA `524,288 .. 655,367` (256 MB offset):** Persistent Bloom filter header (`BLOOM_HEADER_LBA`) and 64 MB Bloom filter data (`BLOOM_DATA_LBA`).
  - **LBA `1,048,576+` (`4 KB` pages = `8 × 512 B` sectors):**
    - **Page 0:** Global shard header (`STORAGE_MAGIC = "SHARDSH3"`, `wal_epoch` bumped on every boot, `shard_size`, `shard_count`, `wal_base`, `wal_pages`).
    - **Shards `0..15` (`1 + i * (shard_size + 1)`):** Each shard has its own metadata page (`root_page_id`, `next_free_page`, `free_list_head`) followed by `shard_size` 4 KB B-Tree node pages.
    - **WAL Region (`65,536` pages = `256 MB` at `wal_base`):** Split into two alternating `128 MB` (`32,768`-page) areas for sequential batch logging.

### 6.3 Buffer Pool Manager (BPM) in DRAM Bank 1

File: [`common/src/storage/bpm.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/bpm.rs)

| Property | Value |
|----------|-------|
| Backing memory | Ky X1 DRAM Bank 1 (`HIGH_DRAM_BASE = 0x1_0000_0000`, 6 GB physical window) |
| Target resident frames / shard | `81,920` (`320 MB` per shard = **`5.00 GB` across 16 shards**) |
| Max frames / shard | `86,016` (`336 MB` per shard = **`5.25 GB` across 16 shards**) |
| Frame metadata | Compact 24-byte `FrameMeta` kept in Bank 0 L2-friendly RAM |
| Lookup structure | Open-addressed hash table (`page_id -> frame_id`) |
| Eviction policy | Clock (second-chance) algorithm over clean frames; **never writes dirty pages on eviction** |

Because dirty pages are never written ad-hoc during eviction, an interrupted write or power loss can never leave cross-shard state (such as `account` frontier vs. `block` table) inconsistent on disk.

### 6.4 Atomic Write-Ahead Log (WAL)

File: [`common/src/storage/wal.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wal.rs)

1. **Transaction-Consistent Cut:** Hart 6 (the single ledger writer) invokes `wal::checkpoint()` between block batches (every 10 seconds or when `NEED_CHECKPOINT` is signaled by the BPM).
2. **Snapshot & Sequential WAL Write:** Up to `MAX_CHECKPOINT_PAGES = 4,096` (`16 MB`, pre-allocated at boot) dirty pages across all 16 shards are copied into a contiguous buffer and written sequentially to the active WAL area as `[descriptor pages][data pages][commit page]` with a 64-bit multiplicative hash checksum and the current boot epoch.
3. **Sorted Home Application:** Once the commit page is durable on NVMe, the snapshotted pages are sorted by disk LBA and written to their home shard locations, then marked clean if not re-dirtied.
4. **Power-Loss Recovery:** On boot (`wal::init_and_recover`), the engine scans both WAL areas for valid batches matching the previous epoch and idempotently replays the newest committed batch before bumping the device epoch.

### 6.5 B-Tree Engine

File: [`common/src/storage/btree.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/btree.rs)

- **Page size:** `4,096 B` (`AlignedBlock`, `repr(align(64))`).
- **Node capacity:** `MAX_KEYS = 12`, `KEY_SIZE = 65 B` (1-byte table prefix + 64-byte key), `VALUE_SIZE = 256 B`.
- **Operations:** Iterative `search()` with `Zicbop` cache-line prefetching, proactive-split `insert()`, `delete()` with sibling borrow/merge and embedded free-page linked list, and stack-cached `RangeIterator`.
- **Sharding:** `16` shards selected by the top nibble of `key[1]`, preserving prefix ordering within each shard while allowing parallel read access across harts. Per-hart lock wait/hold times and long-hold call sites are tracked via `ShardMutex`.

---

## 7. Network, RPC, and WebSocket Stack

- **Driver:** `UbootNetDevice` — raw Ethernet RX/TX via U-Boot syscalls with TX stall detection.
- **TCP/IP:** `smoltcp` 0.12 in `no_std` mode, configured via `config.toml` (default IP `192.168.1.200/24`, gateway `192.168.1.1`).
- **Services & Ports:**
  - **Port `7075` (Peering):** Nano live network TCP listener & outbound peer connector (`96` concurrent peers).
  - **Port `7076` (HTTP JSON-RPC):** Full `no_std` migration of `rsnano_rpc_messages`, `rsnano_rpc_handler`, and `rsnano_rpc_server`.
  - **Port `7078` (WebSocket Server):** Full `no_std` migration of `rsnano_websocket_messages`, `rsnano_websocket_client`, and `rsnano_websocket_server` using `httparse` and `embedded-websocket`. Supports real-time subscriptions (`confirmation`, `vote`, `started_election`, `stopped_election`, `telemetry`, `bootstrap`, `new_unconfirmed_block`, etc.) with subscription acknowledgements, topic filtering, and `ping`/`pong`.

---

## 8. Async Runtime

File: `common/src/async_rt.rs`

A minimal multicore-safe `no_std` cooperative executor:

- **Per-hart queues:** 8 local queues (`VecDeque<Arc<Task>>`) plus an injector queue.
- **Worker affinity:** Newly spawned tasks are round-robined across worker harts `[4, 5, 7]`, leaving Hart 1 dedicated to networking/I/O, Harts 2–3 dedicated to crypto/message draining, and Hart 6 dedicated to single-writer ledger commits.
- **Work stealing:** Idle worker harts steal from other worker queues in `[4, 5, 7]` using `try_lock()`.
- **Timers:** Hardware `mtime` (24 MHz CLINT) drives `Sleep` futures without timer interrupt overhead.

---

## 9. Hardware Drivers

- **U-Boot API Bridge (`common/src/uboot.rs`):** Wraps U-Boot Jump Table and Syscall API (`dev_read`, `dev_write`, `env_get`, etc.) with inline assembly `gp` register preservation.
- **I2C OLED Display (`common/src/oled.rs`):** Bare-metal SSD1306/SH1106 128×64 driver supporting hardware `I2C4` (`0xd401_2800`) and GPIO bit-bang fallback across candidate header pins. Renders live peer count, stored blocks (`BLK`), confirmed blocks (`CNF`), throughput (`<bps>/S`), and database size (`K`/`M`/`G`).
- **PMIC Watchdog (`common/src/watchdog.rs` / `pmic_wdt.rs`):** Manages the SPM8821 PMIC watchdog over PWR I2C8 (`0xd401_d800`) and tracks Hart 6 liveness.
- **CLINT Timer (`common/src/clint.rs`):** 24 MHz `rdtime` counter (`24 ticks = 1 µs`).
- **Trap Handler (`common/src/trap.rs`):** S-mode trap vector saving all 31 GPRs, enabling `sstatus.FS | VS`, and reporting exception diagnostics (`scause`, `sepc`, `stval`, `ra`, hart ID) over UART.

---

## 10. Build System & Host Tools

Authoritative interface: [`Makefile`](file:///mnt/data/rust-workspace/barely-nano/Makefile)

| Command | Description |
|---------|-------------|
| `make build` / `make build-smode` | Builds release S-mode binary with RVV/Zbb/Zba/Zbs/Zbc/Zicbom/Zicboz/Zihintpause, generates `barely-nano.bin` and `boot.scr`, and deploys to SD card (`NANO_NODE`) if mounted |
| `make build-block-test` | Builds firmware with `block-test` feature for deterministic on-hardware single-block and 64-block pipeline benchmarking |
| `make run-tests` | Runs unit test suites on host (`x86_64`) across `rsnano_sha`, `rsnano_core`, `rsnano_ledger`, `rsnano_messages`, `rsnano_node`, `rsnano_rpc_messages`, `rsnano_rpc_handler`, `rsnano_websocket_messages`, `rsnano_websocket_client`, and `rsnano_websocket_server` |
| `make run-picocom` | Captures live 115200-baud UART output from `/dev/ttyUSB0` to `orpi.log` |
| `make stress` | Builds and runs `tools/nano-stress` against the bare-metal node |
| `./tools/ws_subscribe.py` | Zero-dependency Python CLI tool to subscribe to live WebSocket topics on port `7078` |

---

## 11. Current Status (as of 2026-11-01)

| Subsystem | Status |
|-----------|--------|
| Boot + multi-hart dispatch (Harts 1–7) | ✅ Production-stable |
| U-Boot API + SD `config.toml` loader | ✅ Operational |
| Cross-cluster cache coherency (`Zicbom`) | ✅ Operational |
| RVV 1.0 + Zbb Blake2b & Ed25519 (`rsnano_sha`) | ✅ Operational (~37.7M+ batch-verified signatures in 10.5h run) |
| 16-Shard B-Tree + 5 GB DRAM Bank 1 BPM | ✅ Operational |
| Atomic Dual-Area Write-Ahead Log (WAL) | ✅ Operational (`4,697` checkpoints / `92.9M` pages applied with 0 errors in 10.5h) |
| Live Network Peering & Realtime Sync | ✅ 96/96 peers connected, `73.48M` online stake, `127.1M+` blocks (`99.95%` confirmed) |
| Sustained Sync Throughput | ✅ **280–524 bps** (**~720 bps** average over 10.5h overnight run; `+27.28M` blocks synced overnight) |
| Memory Stability | ✅ Flat ~1,023–1,040 MB heap + 4.48–5.12 GB page cache over 10.5h continuous run |
| HTTP JSON-RPC Server (port 7076) | ✅ Complete `no_std` migration & unit-tested |
| WebSocket Server & Client (port 7078) | ✅ Complete `no_std` migration, unit-tested, and verified streaming live `confirmation`, `vote`, and `started_election` topics |
| SSD1306/SH1106 I2C OLED Display | ✅ Live status (`BLK`, `CNF`, `BPS`, `DB` in `K`/`M`/`G`) |
