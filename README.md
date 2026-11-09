# Bare-Metal RISC-V Nano Node (barely-nano)

This project is a bare-metal `no_std` RISC-V Nano cryptocurrency node designed to run directly on the **Orange Pi RV2 (SpacemiT K1 / Ky X1 SoC)** with 8× X60 RISC-V harts, 8 GB RAM, and a 1 TB NVMe SSD.

It establishes a self-contained operating environment on top of OpenSBI and U-Boot (`CONFIG_API=y`), complete with a heap allocator, multi-core exception handling, hardware-accelerated cryptography, a 16-shard B-Tree + 5 GB DRAM Bank 1 Buffer Pool + atomic Write-Ahead Log (WAL) storage engine on NVMe, a full TCP/IP networking stack, an HTTP JSON-RPC server, and a real-time WebSocket server.

## Table of Contents

- [Features Implemented](#features-implemented)
  - [1. S-Mode Execution on OpenSBI & U-Boot](#1-s-mode-execution-on-opensbi--u-boot)
  - [2. Memory Management](#2-memory-management)
  - [3. Exception and Trap Handling](#3-exception-and-trap-handling)
  - [4. Hardware Watchdogs & Timers](#4-hardware-watchdogs--timers)
  - [5. Hardware Accelerated Cryptography & Optimizations](#5-hardware-accelerated-cryptography--optimizations)
  - [6. U-Boot Networking, RPC & WebSocket Stack](#6-u-boot-networking-rpc--websocket-stack)
  - [7. Multi-Hart SMP Architecture](#7-multi-hart-smp-architecture)
  - [8. I2C OLED Status Display (`common/src/oled.rs`)](#8-i2c-oled-status-display-commonsrcoledrs)
- [Storage Architecture: Bare-Metal NVMe B-Tree + WAL Engine](#storage-architecture-bare-metal-nvme-b-tree--wal-engine)
  - [Why the Bare-Metal NVMe Ledger is ~90 GB vs. ~123 GB in LMDB (& Architectural Advantages)](#why-the-bare-metal-nvme-ledger-is-90-gb-vs-123-gb-in-lmdb--architectural-advantages)
- [Representative & Wallet Key Security Architecture (`wallet_crypto`)](#representative--wallet-key-security-architecture-wallet_crypto)
  - [1. Physical & Cryptographic Isolation](#1-physical--cryptographic-isolation)
  - [2. Two Operator Modes: Unattended Auto-Voting vs. Passphrase Lock on Reboot](#2-two-operator-modes-unattended-auto-voting-vs-passphrase-lock-on-reboot)
  - [3. Configuring a Representative via RPC (Port `7076`)](#3-configuring-a-representative-via-rpc-port-7076)
  - [4. Dynamic Invoice QR & Watch-Only Stealth Derivation](#4-dynamic-invoice-qr--watch-only-stealth-derivation)
- [Hardware Requirements](#hardware-requirements)
- [Step-by-Step Setup: Toolchain, U-Boot & SD Card Preparation](#step-by-step-setup-toolchain-u-boot--sd-card-preparation)
  - [1. Install Host Prerequisites (RISC-V Tools, `mkimage` & Rust Target)](#1-install-host-prerequisites-risc-v-tools-mkimage--rust-target)
  - [2. Partition the MicroSD Card & Format to FAT32](#2-partition-the-microsd-card--format-to-fat32)
  - [3. Build Custom U-Boot (`u-boot-orangepi-k1`) & Flash to SD Card Sectors](#3-build-custom-u-boot-u-boot-orangepi-k1--flash-to-sd-card-sectors)
  - [4. Force Orange Pi RV2 to Boot & Load Drivers from the SD Card](#4-force-orange-pi-rv2-to-boot--load-drivers-from-the-sd-card)
  - [5. Build Firmware & Copy `barely-nano.bin`, `boot.scr`, and `config.toml` to the SD Card](#5-build-firmware--copy-barely-nanobin-bootscr-and-configtoml-to-the-sd-card)
- [Build & Deployment System](#build--deployment-system)
  - [Build & Test Targets](#build--test-targets)
- [Deployment & Configuration (`config.toml`)](#deployment--configuration-configtoml)
  - [Live SD Card Configuration (No Recompilation Needed)](#live-sd-card-configuration-no-recompilation-needed)
  - [Default `config.toml`](#default-configtoml)
  - [WebSocket Subscriptions (`tools/ws_subscribe.py`)](#websocket-subscriptions-toolsws_subscribepy)
  - [Ledger Pruning](#ledger-pruning)
  - [Representative Discovery on Bare-Metal](#representative-discovery-on-bare-metal)
- [Fast Ledger Bootstrap: Direct LMDB to NVMe Migration (`tools/lmdb-to-nvme`)](#fast-ledger-bootstrap-direct-lmdb-to-nvme-migration-toolslmdb-to-nvme)
  - [1. How the Two-Pass Streaming Converter Works](#1-how-the-two-pass-streaming-converter-works)
  - [2. Step-by-Step Usage (`tools/lmdb_to_nvme.sh`)](#2-step-by-step-usage-toolslmdb_to_nvmesh)
- [Direct NVMe Ledger Snapshot & Restore (`tools/nvme-snapshot`)](#direct-nvme-ledger-snapshot--restore-toolsnvme-snapshot)
  - [1. How Sparse-Shard Snapshot Streaming & WAL Replay Works](#1-how-sparse-shard-snapshot-streaming--wal-replay-works)
  - [2. `BNSNAPV1` Raw Snapshot File Layout](#2-bnsnapv1-raw-snapshot-file-layout)
  - [3. Step-by-Step Usage (`tools/nvme_snapshot.sh`)](#3-step-by-step-usage-toolsnvme_snapshotsh)
- [Network Stress Testing (`tools/nano-stress`)](#network-stress-testing-toolsnano-stress)
- [Workspace Structure](#workspace-structure)

## Features Implemented

### 1. S-Mode Execution on OpenSBI & U-Boot
The kernel runs in **Supervisor Mode (S-mode)** launched from U-Boot on top of OpenSBI firmware, utilizing SBI `ecall`s for multi-hart management (HSM) and IPIs, and U-Boot's Standalone Jump Table & Syscall API (`CONFIG_API=y`) for raw Ethernet and NVMe block device I/O.
- **Boot Media (SD Card / MMC):** Holds `SPL`, `U-Boot`, `boot.scr`, `barely-nano.bin`, and `config.toml`.
- **Ledger Media (1 TB NVMe SSD):** Automatically discovered via U-Boot block device enumeration (skipping MMC/SD `if_type` 6 & 7) and dedicated exclusively to persistent Nano ledger storage.

### 2. Memory Management
- **Boot sequence:** Written in custom assembly (`main/src/entry.S`) to set up stack pointers for all 8 harts, capture U-Boot's Jump Table pointer, zero BSS, and jump into Rust.
- **Heap Allocator (`DRAM Bank 0`):** Uses `linked_list_allocator` wrapped in `InterruptSafeAllocator` to provide a global dynamic memory allocator (`ALLOCATOR`) configured in `linker-smode.ld` with a **~1.45 GB heap** and **256 MB stack** (32 MB per hart) mapped starting at `0x11000000`.
- **High DRAM Sector Cache (`DRAM Bank 1`):** Directly utilizes the Ky X1 upper physical memory window (`0x1_0000_0000 .. 0x2_5000_0000`) to host a **5.00 GB B-Tree Buffer Pool** (`81,920` × 4 KB frames per shard across 16 shards).

### 3. Exception and Trap Handling
- **Trap Vector:** Custom S-mode assembly trap vector (`trap_vector`) saves and restores all 31 GPRs upon interrupts and exceptions and enables `sstatus.FS` and `sstatus.VS` for floating-point and RVV vector instructions.
- **Trap Dispatcher:** Evaluates the `scause` CSR to differentiate between asynchronous interrupts and synchronous exceptions.
- **Diagnostics:** Exceptions automatically print the hart ID, `scause`, `sepc`, `stval`, and `ra` registers over SpacemiT K1 UART0 (`0xD401_7000`).

### 4. Hardware Watchdogs & Timers
- **CLINT Timer:** Reads the 24 MHz hardware `time` CSR (`rdtime`) for cooperative async scheduling, profiling, and timestamps.
- **Watchdog Management:** Disables and periodically kicks both the SpacemiT K1 SoC watchdog (`0xD408_0000`) and the SPM8821 PMIC watchdog over PWR I2C bus 8 (`0xD401_D800`).

### 5. Hardware Accelerated Cryptography & Optimizations
To achieve high transaction throughput and maximize CPU efficiency, the node leverages the SpacemiT X60 RISC-V extension set (`+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause`):
- **Vector Extension (`V`, `Zvl256b`):** 256-bit SIMD processing (`nullables/sha`) for vectorized Blake2b compression, 4-way batch PoW difficulty evaluation, and Ed25519 verification.
- **Bit-Manipulation (`Zbb`, `Zba`, `Zbs`, `Zbc`):** Scalar bit-manipulation, single-cycle shifted address generation (`sh1add`, `sh2add`, `sh3add`) for B-Tree slot indexing, single-bit operations, and carry-less multiplication (compiling 64-bit Blake2b rotations to single `rori` instructions).
- **Cache-Block Prefetch (`Zicbop`):** Inline `prefetch.r` and `prefetch.w` hints ([`ledger_store/src/btree/arena.rs`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/btree/arena.rs)) during B-Tree binary search, child page descent, and leaf mutation, plus branchless 8-byte big-endian `u64` prefix comparisons ([`NodePage::search_slot`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/btree/node.rs#L156-L202)) to eliminate branch misprediction stalls on the in-order X60 pipeline.
- **Cache-Block Zero (`Zicboz`):** Fast 64-byte cache-line zeroing (`cbo.zero`).
- **Pause Hint (`Zihintpause`):** Adaptive `pause` backoff in spinloops to reduce interconnect contention.
- **Cache-Block Management (`Zicbom`):** Explicit cache maintenance (`cbo.clean`, `cbo.inval`) for cross-cluster coherency between Cluster 0 (Harts 0–3) and Cluster 1 (Harts 4–7).

### 6. U-Boot Networking, RPC & WebSocket Stack
- **Hardware Probing:** Discovers and initializes the board's Ethernet controller via the U-Boot Standalone API (`UbootNetDevice`) with burst RX draining (up to 64 frames per poll) and TX stall recovery.
- **TCP/IP Stack (`smoltcp` 0.12):** Full-featured `no_std` TCP/IP/DNS/ICMP stack integrated with the cooperative async executor.
- **Nano Services:**
  - **Port `7075` (Peering):** Live Nano P2P protocol peering and bootstrap (`96` concurrent peers).
  - **Port `7076` (HTTP JSON-RPC):** Full `no_std` Nano RPC command handler (`rsnano_rpc_messages`, `rsnano_rpc_handler`, `rsnano_rpc_server`).
  - **Port `7078` (WebSocket Server):** Full `no_std` WebSocket server and client (`rsnano_websocket_messages`, `rsnano_websocket_client`, `rsnano_websocket_server`) streaming live node events (`confirmation`, `vote`, `started_election`, `stopped_election`, `telemetry`, `bootstrap`, `new_unconfirmed_block`, etc.).
  - **Port `7080` (Live Web Dashboard & `/api/metrics`):** Built-in HTTP telemetry dashboard ([`main/src/dashboard.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/dashboard.rs)) serving a real-time web UI and `/api/metrics` JSON endpoint for hardware profiling and WAL/B-Tree monitoring.

### 7. Multi-Hart SMP Architecture
All 8 X60 harts on the SpacemiT K1 are pinned by physical CPU cluster (`512 KB` L2 cache per 4-hart cluster) in [`main/src/bin.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs) to maximize L2 cache locality and parallelize B-Tree + WAL storage across two harts:

- **Cluster 0 (Shared 512 KB L2 — Ingress, Signature Verification & Vote Ingestion):**
  - **Hart 0 (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 1 (`Maint+Net`):** Boot initialization, `smoltcp` network polling, HTTP JSON-RPC (`7076`), WebSocket (`7078`), Live Web Dashboard (`7080`), gateway ICMP keepalive, and watchdog kicking.
  - **Hart 2 (`Crypto-1`):** Dedicated lock-free SIMD Ed25519 signature verification worker (`signature_checker.help_verify()`) and inbound `MessageProcessor::run_batch()` drainer.
  - **Hart 3 (`Crypto-2`):** Dedicated lock-free SIMD Ed25519 signature verification worker (`signature_checker.help_verify()`) and inbound `MessageProcessor::run_batch()` drainer.
- **Cluster 1 (Shared 512 KB L2 — Consensus & Parallel Sharded Ledger Storage):**
  - **Hart 4 (`Consensus`):** Drives `aec_voter` and `aec_ticker` (300 ms ticks), `election_schedulers`, `backlog_scan`, `bootstrap_server`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 5 (`Ledger-B`):** Parallel Shards `8–15` B-Tree mutations ([`Ledger::try_run_ledger_b_job`](file:///mnt/data/rust-workspace/barely-nano/ledger/src/ledger.rs#L173-L195)), WAL segments `8–15` ([`wal::try_run_wal_b_job`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wal.rs#L118-L157)), and `unchecked` dependency queue drain.
  - **Hart 6 (`Ledger-A`):** Primary `block_processor` batch coordinator, Shards `0–7` B-Tree mutations ([`Ledger::flush_shard_half`](file:///mnt/data/rust-workspace/barely-nano/ledger/src/ledger.rs#L712-L819)), WAL segments `0–7`, and `confirming_set` cementation.
  - **Hart 7 (`Storage+Mon`):** Online weight calculation, NVMe Free-List GC/reclamation (`ledger_pruning`), 2-second I2C OLED status updates, 15-second telemetry/profiling reports, and cooperative async executor tasks (`WORKER_HARTS`).

### 8. I2C OLED Status Display (`common/src/oled.rs`)
- **Zero-Allocation `no_std` Driver:** Supports 4-pin I2C **SSD1306** and **SH1106** 128×64 OLED displays (at `0x3C` or `0x3D`) over hardware `I2C4` (`0xD401_2800`), with automatic GPIO I2C fallback if `SCK`/`SDA` are swapped or wired to `I2C3` (Pins 8 & 10).
- **Differential Refresh on Hart 7:** Refreshes every 2 seconds on Hart 7, transmitting only text rows that have changed.

#### Orange Pi RV2 26-Pin Header Wiring (`I2C4`)

| Display Pin | Orange Pi RV2 Physical Pin | Signal / SoC Pad | Notes |
| :--- | :--- | :--- | :--- |
| **VDD** (or VCC) | **Pin 1** | `3.3V` | **Use 3.3V only** (Orange Pi RV2 GPIO/I2C pads are 3.3V logic) |
| **SDA** | **Pin 3** | `I2C4_SDA` (`GPIO_52`) | Data line |
| **SCK** (or SCL) | **Pin 5** | `I2C4_SCL` (`GPIO_51`) | Clock line |
| **GND** | **Pin 6** (or Pin 9) | `GND` | Ground |

```text
    Display VDD ──► 3.3V [ 1]  [ 2] 5V
    Display SDA ──►  SDA [ 3]  [ 4] 5V
    Display SCK ──►  SCL [ 5]  [ 6] GND ◄── Display GND
                    PWM9 [ 7]  [ 8] I2C3_SCL (GPIO_47)
                     GND [ 9]  [10] I2C3_SDA (GPIO_48)
                         ...    ...
                         [25]  [26]
```

#### Display Telemetry Layout
Uses a high-contrast 8×16 bold font (4 lines × 15 characters):
- **Line 1:** `NANO P:<peers> *` — Connected peer count and toggling `.`/`*` heartbeat indicator
- **Line 2:** `BLK: <count>` — Total blocks stored in the ledger
- **Line 3:** `CNF: <count>` — Total confirmed (cemented) blocks
- **Line 4:** `<bps>/S DB:<size>` — Block processing rate (blocks/sec) and NVMe B-Tree database size (`K`/`M`/`G`)

## Storage Architecture: Bare-Metal NVMe B-Tree + WAL Engine

Without an OS filesystem or traditional database (LMDB/RocksDB), `barely-nano` implements a crash-resilient **16-Shard B-Tree + 5 GB DRAM Bank 1 Buffer Pool + Atomic Write-Ahead Log (WAL)** directly on the 1 TB NVMe SSD:

1. **Physical Layer (`StorageBackend::UBoot`):** Transfers raw 4 KB pages (`8 × 512 B` sectors, batched up to 64 pages / 256 KB per command via zero-copy 64-bit NVMe PRP DMA) over U-Boot's block syscall interface directly from High DRAM Bank 1 (`0x1_0000_0000+`) to the NVMe SSD. (`STORAGE_BASE_LBA = 1,048,576` leaves a 512 MB safety margin at the start of the drive, with a 64 MB persistent Bloom filter at `BLOOM_HEADER_LBA = 524,288`.)
2. **5 GB High-DRAM Buffer Pool Manager (`common/src/storage/bpm.rs`):** Manages `81,920` × 4 KB frames per shard (`320 MB/shard` = `5.00 GB` total) in Ky X1 DRAM Bank 1 (`0x1_0000_0000`), paired with compact 24-byte `FrameMeta` entries in Bank 0 L2-friendly memory, an open-addressed page hash table, and a **Clock (second-chance)** eviction policy. Dirty frames are **never written during eviction**, preventing partial cross-shard updates from reaching disk.
3. **Atomic Dual-Area Write-Ahead Log (`common/src/storage/wal.rs`):** Reserves `256 MB` (`2 × 128 MB` alternating areas) on NVMe. Between block batches on Cluster 1, `wal::checkpoint()` snapshots dirty pages across Shards `0..7` (Hart 6) and Shards `8..15` (Hart 5) in parallel, writes `[descriptor pages][data pages][commit page]` sequentially with a 64-bit checksum and boot epoch, and then writes the pages to their home shard locations sorted by LBA. On boot, `wal::recover()` idempotently replays the latest committed batch.
4. **16-Shard B-Tree Engine (`ledger_store/src/btree/` & `common/src/storage/btree.rs`):** Prefix-partitioned across 16 shards by the high nibble of `key[1]` (`0x00..0x7F` on Hart 6 `Ledger-A`, `0x80..0xFF` on Hart 5 `Ledger-B`), supporting `Zicbop`-prefetched (`prefetch.r`/`prefetch.w`) branchless 8-byte `u64` prefix binary search, proactive-split inserts, sibling borrow/merge deletes with embedded free-list page reclamation, and stack-cached range iterators.
5. **Isolated Wallet B-Tree & Atomic Redo Journal (`WALLET_SHARD` at `WALLET_STORAGE_LBA = 786,432`):** Wallet and representative keys live in a dedicated B-Tree partition at the `384 MB` NVMe mark (`LBA 786,432 .. 819,712`, `16 MB` B-Tree + `256 KB` checksummed redo journal), strictly below `STORAGE_BASE_LBA` (`512 MB`) and enforced by compile-time assertions and runtime LBA bounds guards so wallet mutations can never overwrite or contend with a synced ledger.

### Why the Bare-Metal NVMe Ledger is ~90 GB vs. ~123 GB in LMDB (& Architectural Advantages)

On the exact same unpruned Nano mainnet ledger (**220.89M blocks**, **37.44M accounts**, **218.53M confirmed**), the standard LMDB `data.ldb` file occupies **~122.2 GiB (`131.2 GB` / `32,029,037` × 4 KB pages)**, whereas `barely-nano`'s 16-shard NVMe B-Tree occupies **~90.4 GiB (`92,615 MB` / `23,709,465` × 4 KB pages)**—a **~26% (~31.8 GiB) storage reduction** before even enabling pruning:

| Metric | Standard Linux Node (LMDB `data.ldb`) | `barely-nano` Bare-Metal NVMe B-Tree | Advantage |
| :--- | :--- | :--- | :--- |
| **Unpruned Ledger Size (220.9M blocks)** | **~122.2 GiB (`131.2 GB`)** (`32,029,037` pages) | **~90.4 GiB (`92.6 GB`)** (`23,709,465` pages) | **~31.8 GiB (~26%) smaller** on disk |
| **Pruned Ledger Size (`--prune`)** | ~45–55 GiB (requires offline `mdb_copy -c` to reclaim file size) | **~30–32 GiB** (reclaims freed 4 KB pages in-place via shard free-list) | **~75% smaller** than unpruned LMDB; zero offline compaction needed |
| **Leaf Page Fill Factor** | **~67–69%** average (`50%` immediately after random 256-bit hash page splits) | **~88%** (`3,584 / 4,096 B` packed slotted pages with `0` dead bytes) | Eliminates **~23 GiB** of internal B+Tree split slack space |
| **Copy-on-Write (COW) & Freelist Overhead** | **~8.7 GiB** (`~2.26M` pages in branch nodes, un-reclaimed COW pages & `FREE_DBI` lists) | **0 B COW bloat** (in-place page updates + fixed `256 MB` dual-area WAL) | Eliminates multi-version page duplication and freelist fragmentation |
| **Incremental Growth During Live Sync** | ~400–600 bytes/block (COW leaf + ancestor branch path writes) | **~67 bytes/block net** (`+61 MB` across `+904k` blocks & `+167k` accounts) | New keys fill pre-allocated 12% slotted-page headroom before splitting |
| **SSD Write Amplification & Filesystem Overhead** | High (every commit writes COW leaf-to-root path + ext4/XFS journal + `msync`) | **Minimal** (dirty pages stay in 5 GB DRAM Bank 1; flushed in LBA-sorted 256 KB zero-copy PRP DMA batches) | Drastically lower NVMe wear and higher sustained IOPS |
| **RAM & Cache Predictability (8 GB Board)** | Unbounded `mmap` RSS + OS page-cache thrashing + Linux OOM risk | **Fixed `5,120 MB` DRAM Bank 1 Sector Cache + flat `~1,053 MB` Heap** | Zero OS page faults, zero swap, deterministic memory ceiling |
| **Negative Block Lookup Cost** | Full B+Tree root-to-leaf traversal (`3–4` page reads per check) | **$O(1)$ in-RAM `64 MB` `BlockBloom` filter** (persisted on NVMe) | Instant duplicate/existence filtering before touching B-Tree shards |
| **CPU Scheduling & Execution Model** | Linux CFS thread migration, syscalls, interrupts, and multi-thread DB lock contention | **Hard-pinned 8-Hart Cluster 0 / Cluster 1 SMP** (2× SIMD Crypto harts on Cluster 0, 2× Parallel Sharded Ledger harts on Cluster 1) | **21.0k bps sustained E2E (`22.4k–28.3k bps` active pipeline)**; zero context-switch or syscall overhead |

#### Why the Storage Size Difference Occurs
1. **No Copy-on-Write (COW) or Freelist DB Bloat (~8.7 GiB saved):** LMDB never modifies pages in place; every transaction allocates new 4 KB pages for the modified leaf and every parent branch page up to the root, recording old page numbers in `FREE_DBI`. In a 220.9M-block `data.ldb` (`32,029,037` pages), only `29,760,946` pages contain active leaf data, leaving **over 2.26 million pages (~8.65 GiB)** consumed by historical COW snapshots, freelist tracking, and deeper branch trees. `barely-nano` updates slotted pages in place in its 5 GB DRAM buffer pool and commits atomically via a fixed `256 MB` dual-area WAL.
2. **Higher Slotted-Page Packing Density (~23.1 GiB saved):** Because Nano block hashes and account public keys are uniformly distributed 256-bit values, random insertions into LMDB split 4 KB leaf pages in half (`50%` occupancy after split, averaging `~68%` across the file—wasting ~1.3 KB per page). `barely-nano`'s slotted-page B-Tree packs entries to **~88% occupancy** (`3,584 / 4,096` bytes) during [`lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) migration while leaving ~512 bytes of free headroom per page. During live network syncing after boot, incoming blocks fill that existing page headroom first—resulting in only **`+61 MB` of NVMe growth across `+904,574` blocks and `+167,046` accounts (~67 bytes/block)**.
3. **In-Place Page Reclamation Without Offline Compaction:** When blocks or pending entries are deleted (or pruned), LMDB's `data.ldb` file never shrinks on disk unless taken offline and rewritten with `mdb_copy -c`. In `barely-nano`, each of the 16 B-Tree shards maintains an embedded $O(1)$ free-page list (`free_list_head`) that immediately recycles emptied 4 KB pages on the fly.

## Representative & Wallet Key Security Architecture (`wallet_crypto`)

To protect representative private keys against physical NVMe SSD theft, power loss, and unauthorized extraction without sacrificing bare-metal voting throughput, `barely-nano` combines **Ky X1 SoC hardware eFuse binding** with **iterated `Blake2b-256` key derivation (`KDF`)** in [`common/src/storage/wallet_crypto.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wallet_crypto.rs) and [`ledger_store/src/wallet_store.rs`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/wallet_store.rs):

### 1. Physical & Cryptographic Isolation
- **Strict NVMe Partition Separation:** Wallet storage uses [`WALLET_SHARD`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs#L164) at `WALLET_STORAGE_LBA = 786,432` (`384 MB` offset) with its own 64-page redo journal. It never touches the 16 ledger [`SHARDS`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs#L161) or the ledger WAL (`>= 512 MB`), and survives full ledger wipes (`wipe_blocks = true`).
- **No Plaintext Keys or Passwords on Disk:** Neither the wallet `seed`, any account `priv_key`, nor the wallet `password` is ever written in plaintext to the NVMe SSD.
- **Hardware-Bound Key Derivation (`Ky X1 eFuse + Salt + Password`):**
  1. At boot, [`hardware_wallet_key()`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wallet_crypto.rs#L99) derives a 256-bit SoC hardware root key from the **SpacemiT K1 / Ky X1 4K-bit on-chip eFuse banks** (`0xD401_F000`), Chip ID Unit (`0xD428_2C00`), EMAC0 factory MAC registers (`0xCAC8_0040`), and U-Boot's eFuse-derived `serial#` / `ethaddr` / optional `wallet_hw_key` environment variables using trap-safe S-mode MMIO probes ([`trap::probe_read_u32`](file:///mnt/data/rust-workspace/barely-nano/common/src/trap.rs#L207)).
  2. Each wallet generates a unique 256-bit random `salt` and derives its 256-bit wrapping key via **4,096 rounds of iterated `Blake2b-256`**:
     $$\text{WrappingKey} = \text{Blake2b-256}^{4096}\bigl(\text{KyX1eFuseKey} \parallel \text{WalletSalt} \parallel \text{Password}\bigr)$$
  3. The wallet `seed` and each account `priv_key` are encrypted with domain-separated `Blake2b-256` keystream pads (`crypt_seed`, `crypt_priv_key`) and authenticated by a 256-bit `check_tag` (`compute_check_tag`). On unlock, every decrypted private key is also verified against its stored `PublicKey` (`priv_key.public_key() == pub_key`).
- **Zero Runtime Voting Overhead:** Decryption runs only once when the wallet is unlocked (at boot or on `wallet_enter`, taking `< 1.5 ms`). Once unlocked, Hart 4 ([`VoteGenerator`](file:///mnt/data/rust-workspace/barely-nano/node/src/consensus/vote_generation/vote_generator.rs)) signs votes directly from RAM with 0% cryptographic overhead.

### 2. Two Operator Modes: Unattended Auto-Voting vs. Passphrase Lock on Reboot

| Mode | Configuration | Behavior on Reboot / Power Loss | Protection if NVMe SSD is Stolen |
| :--- | :--- | :--- | :--- |
| **Mode A: Unattended Auto-Voting** *(Default)* | Default empty password (`""`) | Node automatically derives the wrapping key from the OrangePi RV2's **Ky X1 SoC eFuse**, unseals the representative key into RAM during `Wallets::initialize()`, runs `compute_reps()`, and **starts voting immediately with zero human input**. | **Encrypted on disk** — another machine lacks the OrangePi RV2's internal Ky X1 eFuse key and cannot verify `check_tag` or decrypt the keys. |
| **Mode B: Passphrase Required at Reboot** *(Optional)* | Operator sets a password via `wallet_rekey` | Node boots and syncs the ledger normally, but the wallet stays **locked with zero plaintext private keys in RAM** until the operator sends `wallet_enter` over RPC. | **Dual-factor encrypted** — requires **both** the physical OrangePi RV2 SoC eFuse **and** the operator's passphrase. |

### 3. Configuring a Representative via RPC (Port `7076`)

Because `key_create` is a stateless utility RPC that only generates a keypair in memory without saving it, use `wallet_create` + `wallet_add` (or `account_create`) to persist a representative key to the encrypted NVMe wallet store:

```bash
# 1. (Optional) Generate a new keypair via RPC (or use an existing representative private key)
curl -s -d '{"action": "key_create"}' http://192.168.1.200:7076

# 2. Create a persistent wallet in the isolated WALLET_SHARD on NVMe
curl -s -d '{"action": "wallet_create"}' http://192.168.1.200:7076
# -> Returns: {"wallet": "<WALLET_ID>"}

# 3. Import the representative private key into the wallet (persisted & eFuse-encrypted immediately)
curl -s -d '{"action": "wallet_add", "wallet": "<WALLET_ID>", "key": "<PRIVATE_KEY_HEX>"}' http://192.168.1.200:7076
# -> Returns: {"account": "nano_..."}
```

#### Optional: Enable Mode B (Require Passphrase Unlock After Reboot)
```bash
# Set a passphrase (re-encrypts wallet seed & keys under Ky X1 eFuse + passphrase; never stores passphrase)
curl -s -d '{"action": "wallet_rekey", "wallet": "<WALLET_ID>", "password": "MyStrongRepPassword"}' http://192.168.1.200:7076

# After a node reboot, unlock the wallet in RAM to resume representative voting:
curl -s -d '{"action": "wallet_enter", "wallet": "<WALLET_ID>", "password": "MyStrongRepPassword"}' http://192.168.1.200:7076

# To switch back to Mode A (unattended auto-voting on reboot), rekey with an empty password while unlocked:
curl -s -d '{"action": "wallet_rekey", "wallet": "<WALLET_ID>", "password": ""}' http://192.168.1.200:7076
```

### 4. Dynamic Invoice QR & Watch-Only Stealth Derivation

For Point-of-Sale (POS) terminals and merchant appliances, `barely-nano` implements **Watch-Only Stealth Address Derivation + $O(1)$ Stateless Ephemeral Invoice Generation** ([`nullables/sha/src/ed25519.rs`](file:///mnt/data/rust-workspace/barely-nano/nullables/sha/src/ed25519.rs), [`core/src/private_key.rs`](file:///mnt/data/rust-workspace/barely-nano/core/src/private_key.rs), [`node/src/wallets/wallet.rs`](file:///mnt/data/rust-workspace/barely-nano/node/src/wallets/wallet.rs)):

- **Zero Theft Surface on the POS Node:** The countertop node holds **only** a 256-bit Scan Key $v$ (`wallet.seed`) and the merchant's Cold Spend Public Key $S$ (`spend_public_key`). The Cold Master Spend Private Key $s$ is **never** loaded onto the node.
- **Additive Ed25519 Blinding:** For each invoice index $i$ (with domain tag $\text{DomainTag} =$ `"BARELY_NANO_STEALTH_V1"`), the node computes a deterministic blinding scalar:
  $$c_i = \text{Blake2b-512}\bigl(\text{DomainTag} \parallel v \parallel i_{\text{be}}\bigr) \bmod L$$
  and derives a one-time receiving account $P_i = S + c_i \cdot B$ using precomputed Radix-16 basepoint tables (`RADIX16_PRECOMP`). To the customer's wallet and the Nano network, $P_i$ is a standard `nano_1...` address.
- **$O(1)$ Memory & NVMe Storage (`"stateless": "true"`):** Passing `"stateless": "true"` to `account_create` updates **only** the 4-byte `deterministic_index` counter in [`PersistedWalletMeta`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/wallet_store.rs#L42-L58) (`WALLET_SHARD`) without inserting per-invoice entries into `Wallet::keys` (`HashMap`) or allocating new NVMe pages—allowing millions of dynamic invoice QR codes in strictly $O(1)$ RAM and disk space.
- **Cold Vault Scalar Reconstruction:** An air-gapped Cold Vault holding both the Master Spend Private Key $s$ and Scan Key $v$ reconstructs the canonical one-time Ed25519 scalar key $p_i = (s_{\text{clamped}} + c_i) \pmod L$ via [`PrivateKey::from_stealth`](file:///mnt/data/rust-workspace/barely-nano/core/src/private_key.rs#L61-L72) / [`Wallets::insert_stealth_key_at`](file:///mnt/data/rust-workspace/barely-nano/node/src/wallets/wallets.rs#L497-L539) and signs standard Nano `StateBlock`s via [`sign_scalar`](file:///mnt/data/rust-workspace/barely-nano/nullables/sha/src/ed25519.rs#L1639-L1659).

#### Step 1: Provision the Watch-Only POS Sentinel Wallet
Configure a wallet on the OrangePi RV2 with the Scan Key $v$ (`seed`) and the Cold Vault's Spend Public Key $S$ (`spend_public_key`):

```bash
# Create a Watch-Only Stealth POS wallet (or configure an existing wallet via wallet_change_seed)
curl -s -d '{
  "action": "wallet_create",
  "seed": "<64_HEX_SCAN_KEY_V>",
  "spend_public_key": "nano_1coldspendpubkey..."
}' http://192.168.1.200:7076
# -> Returns: {"wallet": "<POS_WALLET_ID>", "accounts": ["nano_1stealthindex0..."]}
```

*(Alternatively, update an existing wallet using `wallet_change_seed` with `"seed"` and `"spend_public_key"`.)*

#### Step 2: Generate Dynamic Invoice QR Addresses at Checkout ($O(1)$ Stateless Mode)
For each customer checkout, request a fresh one-time `nano_...` address with `"stateless": "true"`:

```bash
# Derive the next one-time stealth invoice address (O(1) RAM & NVMe; increments deterministic_index only)
curl -s -d '{
  "action": "account_create",
  "wallet": "<POS_WALLET_ID>",
  "stateless": "true"
}' http://192.168.1.200:7076
# -> Returns: {"account": "nano_1onetimeinvoice..."}
```

You can also re-derive a specific historical invoice address at index $i$ without advancing the counter beyond $i+1$:

```bash
curl -s -d '{
  "action": "account_create",
  "wallet": "<POS_WALLET_ID>",
  "index": "42",
  "stateless": "true"
}' http://192.168.1.200:7076
```

## Hardware Requirements

### Required Hardware

| Component | Minimum Specification | Purpose |
| :--- | :--- | :--- |
| **Single-Board Computer** | **Orange Pi RV2 (8 GB RAM)** | SpacemiT K1 / Ky X1 8-core RISC-V X60 SoC (`2 GB` Bank 0 + `6 GB` Bank 1 DRAM required for the 1.45 GB heap and 5 GB B-Tree Buffer Pool) |
| **MicroSD Card** | **$\ge 2\text{ GB}$** | Boot media holding custom U-Boot raw sectors (`bootinfo_sd.bin`, `FSBL.bin`, `u-boot-opensbi.itb`) and a **FAT32** partition (`NANO_NODE`) with `boot.scr`, `barely-nano.bin`, and `config.toml` |
| **NVMe SSD** | **$\ge 128\text{ GB}$ M.2 2280** (up to 1 TB) | Must be installed in **NVMe 1 (the M.2 2280 slot underneath the board)**. Dedicated exclusively to raw B-Tree + WAL ledger storage (`128 GB` minimum for pruned mode; `256 GB–1 TB` recommended for full unpruned live ledger) |
| **Ethernet Cable** | **RJ45 Cat5e/Cat6 connected to Ethernet Port 1** | Must be plugged into **Ethernet Port 1** (U-Boot only initializes Ethernet Port 1) |
| **USB-to-TTL UART Adapter** | **3.3V Logic (`115200 8N1`)**, e.g. **DSD TECH SH-U09C5** | Connects to the 3-pin debug UART0 header (`TX`, `RX`, `GND`) to monitor serial logs (`orpi.log`) and configure U-Boot to boot from the SD card |

> [!WARNING]
> **Hardware Port Restrictions (U-Boot Driver Limitation):**
> The U-Boot device tree and driver initialization on the Orange Pi RV2 only load and probe **Ethernet Port 1** and **NVMe 1 (the M.2 2280 slot located underneath the board)**:
> - **NVMe 1 (`M.2 2280` under the board):** You **must** install the M.2 2280 NVMe SSD in the slot on the bottom of the board. Connecting an SSD to the other M.2 slot will **not** be detected by U-Boot (`nvme scan`).
> - **Ethernet Port 1:** You **must** connect your LAN cable to **Ethernet Port 1**. Connecting to the second RJ45 Ethernet port will **not** work.

### Optional (Recommended) Accessories

| Accessory | Specification | Purpose |
| :--- | :--- | :--- |
| **Enclosure** | **Orange Pi RV2 Case** | Protects the board and M.2 NVMe SSD and mounts the cooling fan |
| **Cooling Fan** | **30×30×7 mm Fan** (5V / 2-pin) | Keeps the 8-core Ky X1 SoC and NVMe SSD cool during continuous 24/7 ledger sync and signature verification |
| **Status Display** | **128×64 I2C OLED Display** (4-pin **SSD1306** or **SH1106**) | Connects to `I2C4` (Pins 1, 3, 5, 6) to display real-time peer count, block count (`BLK`), confirmed blocks (`CNF`), sync rate (`BPS`), and database size (`K`/`M`/`G`) |

#### Debug UART Wiring (DSD TECH SH-U09C5 → Orange Pi RV2 Debug UART0)

> [!IMPORTANT]
> Set the jumper on the **DSD TECH SH-U09C5** (or equivalent USB-to-TTL adapter) to **3.3V** before connecting it to the Orange Pi RV2. Do **not** connect the `VCC` pin if the board is powered via USB-C.

| USB-to-TTL Adapter Pin | Orange Pi RV2 Debug UART Header | Notes |
| :--- | :--- | :--- |
| **GND** | **GND** | Common ground |
| **RXD** | **TX** | Adapter RX connects to Board TX |
| **TXD** | **RX** | Adapter TX connects to Board RX |

---

## Step-by-Step Setup: Toolchain, U-Boot & SD Card Preparation

### 1. Install Host Prerequisites (RISC-V Tools, `mkimage` & Rust Target)

On an Ubuntu/Debian x86_64 host, install the RISC-V GNU cross-compiler toolchain, U-Boot `mkimage` utility (`u-boot-tools`), device-tree compiler, FAT32 formatting tools, and serial console monitor:

```bash
sudo apt update
sudo apt install -y \
    gcc-riscv64-linux-gnu \
    binutils-riscv64-linux-gnu \
    u-boot-tools \
    device-tree-compiler \
    build-essential \
    bison \
    flex \
    libssl-dev \
    python3 \
    dosfstools \
    fdisk \
    picocom
```

Install the bare-metal RISC-V target and `llvm-objcopy` component via `rustup`:

```bash
rustup target add riscv64gc-unknown-none-elf
rustup component add llvm-tools-preview
```

---

### 2. Partition the MicroSD Card & Format to FAT32

The SpacemiT K1 BootROM and U-Boot SPL expect raw bootloader binaries at fixed sectors at the beginning of the SD card (`sectors 0..4521`, i.e. the first `~2.3 MiB`), followed by Partition 1 (`mmc 0:1`) formatted as **FAT32**:

| SD Card Region | Byte Offset | Sector Offset (512 B) | Contents |
| :--- | :--- | :--- | :--- |
| **BootInfo Header** | `0x000000` (0 B) | **Sector `0`** (80 bytes) | `bootinfo_sd.bin` (BootROM header pointing to SPL at `0x20000`; preserves MBR partition table at offset `446..511`) |
| **DDR Training Data** | `0x010000` (64 KB) | **Sector `128`** | Cached DDR training parameters written by SPL |
| **FSBL (U-Boot SPL)** | `0x020000` (128 KB) | **Sector `256`** | `FSBL.bin` (~193 KB First Stage Boot Loader) |
| **U-Boot Environment** | `0x060000` (384 KB) | **Sector `768`** | U-Boot saved environment (`16 KB`) |
| **OpenSBI + U-Boot FIT** | `0x0D0000` (832 KB) | **Sector `1664`** (`0x680`) | `u-boot-opensbi.itb` (~1.46 MB FIT image containing `fw_dynamic.bin`, `u-boot-nodtb.bin`, and `x1_orangepi-rv2.dtb`) |
| **Partition 1 (`NANO_NODE`)** | `0x400000` (4 MiB+) | **Sector `8192` → End** | **FAT32** filesystem containing `boot.scr`, `barely-nano.bin`, and `config.toml` |

> [!CAUTION]
> Replace `/dev/sdX` below with your actual MicroSD card block device (check with `lsblk`). Never specify your host NVMe drive.

```bash
# 1. Define your SD card device (e.g. /dev/sdb)
export SD_DEV=/dev/sdX

# 2. Unmount any existing partitions on the SD card
sudo umount ${SD_DEV}* 2>/dev/null || true

# 3. Create a clean MBR (dos) partition table with Partition 1 starting at sector 8192 (4 MiB)
#    Type '0c' sets the partition ID to W95 FAT32 (LBA)
sudo sfdisk "${SD_DEV}" << 'EOF'
label: dos
start=8192, type=0c, bootable
EOF

# 4. Format Partition 1 as FAT32 with volume label NANO_NODE
#    (Note: use ${SD_DEV}p1 if your device is /dev/mmcblk0)
sudo mkfs.vfat -F 32 -n NANO_NODE "${SD_DEV}1"
```

---

### 3. Build Custom U-Boot (`u-boot-orangepi-k1`) & Flash to SD Card Sectors

The modified U-Boot tree for the Orange Pi RV2 is maintained in a dedicated repository ([`https://github.com/caiovini/u-boot-orangepi-k1.git`](https://github.com/caiovini/u-boot-orangepi-k1.git)) configured with:
- **`CONFIG_API=y`** (`configs/x1_defconfig`) — Exposes the U-Boot Standalone syscall & Jump Table API required by `barely-nano`.
- **S-Mode `menvcfg` Cache & Prefetch Enablement** (`arch/riscv/cpu/generic/dram.c`) — Sets `CBIE=11`, `CBCFE=1`, `CBZE=1` so `Zicbom` (`cbo.clean`/`cbo.inval`), `Zicboz` (`cbo.zero`), and `Zicbop` (`prefetch.r`/`prefetch.w`) execute natively in S-mode without trapping.
- **Zero-Copy 64-Bit NVMe PRP DMA & `256 KB` Transfers** (`drivers/nvme/nvme.c`) — Supports direct 64-bit physical PRP DMA from High DRAM Bank 1 (`0x1_0000_0000+`), raises `max_transfer_shift` to `18` (`512` sectors / `256 KB`), and replaces `udelay(1)` completion polling with sub-microsecond `pause` loops.
- **Expanded Ethernet DMA Rings (`2048` RX / `2048` TX) & Burst RX Polling** (`drivers/net/ky/x1_emac.c`, `api/api_net.c`) — Drains up to 64 frames per `eth_rx()` call and prevents packet drops under high P2P block/vote burst load.
- **OpenSBI CCI Cache Coherency (`fw_dynamic.bin`)** — Enables hardware L2 snoop coherency across Cluster 0 (Harts 0–3) and Cluster 1 (Harts 4–7).
- **Prebuilt Binaries (`prebuilt/`)** — Includes ready-to-flash `bootinfo_sd.bin`, `FSBL.bin`, and `u-boot-opensbi.itb` if you want to skip compiling U-Boot from source.

You can initialize the submodule, build U-Boot from source, and flash all three raw sectors automatically using [`build_u-boot.sh`](file:///mnt/data/rust-workspace/barely-nano/build_u-boot.sh):

```bash
# Build u-boot-orangepi-k1 and flash bootinfo_sd.bin, FSBL.bin, and u-boot-opensbi.itb to ${SD_DEV}
./build_u-boot.sh "${SD_DEV}" --flash
```

Or run the build and flash steps manually:

```bash
git submodule update --init u-boot-orangepi-k1
cd u-boot-orangepi-k1

# (Optional) Recompile U-Boot SPL, u-boot-nodtb.bin, DTBs, FSBL.bin, and bootinfo_sd.bin from source:
make ARCH=riscv CROSS_COMPILE=riscv64-linux-gnu- clean
make ARCH=riscv CROSS_COMPILE=riscv64-linux-gnu- x1_defconfig
make ARCH=riscv CROSS_COMPILE=riscv64-linux-gnu- -j$(nproc)

# Package OpenSBI (fw_dynamic.bin) + U-Boot (u-boot-nodtb.bin) + DTB (x1_orangepi-rv2.dtb)
# into u-boot-opensbi.itb using uboot-opensbi.its
./tools/mkimage -f uboot-opensbi.its -r u-boot-opensbi.itb

cd ..
```

Flash `bootinfo_sd.bin`, `FSBL.bin`, and `u-boot-opensbi.itb` to the raw sectors of the SD card before Partition 1 (using either the newly built files or `u-boot-orangepi-k1/prebuilt/`):

```bash
# Write 80-byte BootInfo header at sector 0 (conv=notrunc preserves the MBR partition table at byte 446)
sudo dd if=u-boot-orangepi-k1/prebuilt/bootinfo_sd.bin of="${SD_DEV}" bs=512 count=1 conv=notrunc

# Write FSBL (U-Boot SPL) at sector 256 (128 KB offset)
sudo dd if=u-boot-orangepi-k1/prebuilt/FSBL.bin of="${SD_DEV}" bs=512 seek=256 conv=notrunc

# Write OpenSBI + U-Boot FIT image at sector 1664 (0x680 / 832 KB offset)
sudo dd if=u-boot-orangepi-k1/prebuilt/u-boot-opensbi.itb of="${SD_DEV}" bs=512 seek=1664 conv=notrunc

sync
```

---

### 4. Force Orange Pi RV2 to Boot & Load Drivers from the SD Card

There are two critical requirements for booting `barely-nano` on the Orange Pi RV2:

#### A. Force the SoC to Boot Custom U-Boot from the MicroSD Card (Not Factory SPI NOR)
The Orange Pi RV2 ships with a factory U-Boot in onboard SPI NOR flash that **lacks `CONFIG_API=y`**.
1. Connect your **DSD TECH SH-U09C5** USB-to-TTL converter to the 3-pin debug UART header and open the serial console on your host:
   ```bash
   sudo picocom -b 115200 /dev/ttyUSB0
   ```
2. Insert the prepared MicroSD card and power on the board.
3. If the board boots into the factory SPI NOR U-Boot (or has stale environment variables), press **Enter / Space / Ctrl+C** during the U-Boot countdown to drop into the `=>` prompt and force SD card boot priority:
   ```text
   => env default -a
   => setenv boot_targets "mmc0"
   => saveenv
   => reset
   ```
   *(Note: If your board revision's BootROM prioritizes SPI NOR over SD card, erasing the SPI NOR bootloader header from the U-Boot prompt with `sf probe; sf erase 0 0x100000; reset` forces the BootROM to always load `bootinfo_sd.bin` → `FSBL.bin` → `u-boot-opensbi.itb` directly from the MicroSD card.)*

#### B. Force U-Boot to Probe Ethernet Port 1 & NVMe 1 Drivers via `boot.scr`
U-Boot uses lazy Driver Model (`DM`) probing—hardware controllers remain unclocked and uninitialized unless a U-Boot command actively touches them before jumping to the standalone binary. Furthermore, U-Boot on the Orange Pi RV2 only loads drivers for **Ethernet Port 1** and **NVMe 1 (the M.2 2280 slot underneath the board)**. [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) (compiled into `boot.scr` on the SD card) automatically forces U-Boot to initialize both drivers before starting `barely-nano.bin`:

```text
# 1. Force Ethernet Port 1 (x1_emac) to initialize its MAC, PHY, and DMA rings
ethaddr
ping 10.0.92.1

# 2. Force PCIe controller and NVMe 1 (M.2 2280 slot under the board) to initialize
nvme scan

# 3. Load config.toml from SD card FAT32 partition (mmc 0:1) into RAM at 0x10F00000
mw.b 0x10F00000 0 0x10000
setenv config_size 0
if fatload mmc 0:1 0x10F00000 config.toml; then
    setenv config_addr 0x10F00000
    setenv config_size ${filesize}
fi

# 4. Load barely-nano.bin from SD card FAT32 partition into RAM at 0x11000000 and jump to S-mode
if fatload mmc 0:1 0x11000000 barely-nano.bin; then
    go 0x11000000
fi
```

---

### 5. Build Firmware & Copy `barely-nano.bin`, `boot.scr`, and `config.toml` to the SD Card

1. Mount the `NANO_NODE` FAT32 partition on your host (most Linux desktops auto-mount it at `/media/$USER/NANO_NODE` or `/run/media/$USER/NANO_NODE`; or mount manually):
   ```bash
   sudo mkdir -p /media/$USER/NANO_NODE
   sudo mount "${SD_DEV}1" /media/$USER/NANO_NODE
   ```
2. Edit [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) to match your local LAN subnet (`ipv4_address`, `gateway`, `dns_server`, `mac_address`).
3. Build the firmware and `boot.scr` using `make build`:
   ```bash
   make build
   ```
   - `make build` compiles [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) → `boot.scr` using `mkimage`, compiles `barely-nano` in release mode for `riscv64gc-unknown-none-elf`, extracts `barely-nano.bin` via `llvm-objcopy`, and automatically copies `barely-nano.bin`, `boot.scr`, and `config.toml` if `/media/cvini/NANO_NODE` or `/run/media/cvini/NANO_NODE` is mounted.
4. If your SD card is mounted at a different path, copy the three files manually and unmount cleanly:
   ```bash
   sudo cp barely-nano.bin boot.scr config.toml /media/$USER/NANO_NODE/
   sync
   sudo umount /media/$USER/NANO_NODE
   ```
5. Insert the MicroSD card into the Orange Pi RV2, ensure the NVMe SSD is installed in the M.2 slot, start serial logging (`make run-picocom`), and power on the board.

---

## Build & Deployment System

- [`Makefile`](file:///mnt/data/rust-workspace/barely-nano/Makefile) is the authoritative build interface.
- Cross-compiles for `riscv64gc-unknown-none-elf` with `-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause`.
- Compiles [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) into `boot.scr` via `mkimage`, converts the ELF into `barely-nano.bin`, and automatically deploys `barely-nano.bin`, `config.toml`, and `boot.scr` to the SD card (`/media/cvini/NANO_NODE` or `/run/media/cvini/NANO_NODE`) when mounted.

### Build & Test Targets

| Target | Feature flag | Purpose |
|---|---|---|
| `make build` / `make build-k1` / `make build-smode` | `simd,k1` | **Production build (SpacemiT K1 8 GB)** — builds `barely-nano.bin` and `boot.scr` (5.00 GB DRAM Bank 1 cache) and deploys to SD card if mounted |
| `make build-k3` | `simd,k3` | **SpacemiT K3 (32 GB LPDDR5) build** — builds `barely-nano.bin` with a 25.00 GB DRAM Bank 1 Buffer Pool (see [`spacemit_k3_migration.md`](file:///mnt/data/rust-workspace/barely-nano/spacemit_k3_migration.md)) |
| `make build-block-test` / `make build-spammer` | `block-test,simd` | **Deterministic hardware benchmark** — measures single-block and 64-block pipeline latency and BPS on hardware |
| `make run-tests` | — | Runs all host-side unit test suites (`crypto`, `core`, `ledger`, `messages`, `node`, `rpc_messages`, `rpc_handler`, `websocket_messages`, `websocket_client`, `websocket_server`) |
| `make run-crypto-tests` | `simd` | Runs cryptographic (`rsnano_sha`) unit tests |
| `make run-core-tests` | — | Runs `rsnano_core` unit tests |
| `make run-ledger-tests` | — | Runs `rsnano_ledger` unit tests |
| `make run-messages-tests` | — | Runs `rsnano_messages` unit tests |
| `make run-node-tests` | — | Runs `rsnano_node` unit tests |
| `make run-rpc-messages-tests` | — | Runs `rsnano_rpc_messages` unit tests |
| `make run-rpc-handler-tests` | — | Runs `rsnano_rpc_handler` unit tests |
| `make run-websocket-messages-tests` | — | Runs `rsnano_websocket_messages` unit tests |
| `make run-websocket-client-tests` | — | Runs `rsnano_websocket_client` unit tests |
| `make run-websocket-server-tests` | — | Runs `rsnano_websocket_server` unit tests |
| `make build-lmdb-to-nvme` | — | Builds the host-side [`tools/lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme) binary (`x86_64`) for migrating a Nano `data.ldb` snapshot directly to NVMe sectors |
| `make build-nvme-snapshot` | — | Builds the host-side [`tools/nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) binary (`x86_64`) for dumping a compact raw ledger snapshot from a synced NVMe SSD (`dump`), inspecting it (`info`), and restoring it to an NVMe SSD (`restore`) |
| `make build-stress` | — | Builds the host-side `tools/nano-stress` binary (`x86_64`) |
| `make run-stress` / `make stress` | — | **Network stress test** — streams blocks to the node on port `7075` |
| `make run-picocom` | — | Captures serial console output from `/dev/ttyUSB0` to `orpi.log` |
| `make clean` | — | Cleans Cargo build artifacts |

## Deployment & Configuration (`config.toml`)

Node, RPC, WebSocket, and bare-metal network parameters are configured declaratively in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml), following the Nano / RSNano TOML schema.

### Live SD Card Configuration (No Recompilation Needed)
1. **U-Boot `fatload` (`0x10F00000`):** On boot, [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) (`boot.scr`) zeroes a 64 KB buffer at physical RAM address `0x10F00000` (1 MB below the kernel load address `0x11000000`), runs `fatload mmc 0:1 0x10F00000 config.toml`, and records `config_addr` and `config_size` in the U-Boot environment.
2. **Kernel Boot Parsing:** [`load_config_toml_str()`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs#L120-L166) reads `config.toml` directly from `0x10F00000` and merges it into `DaemonConfig` (`NodeConfig`, `NodeRpcConfig`, `WebsocketConfig`, and `BareMetalNetConfig`).
3. **Compile-Time Fallback:** If `config.toml` is missing from the SD card or fails to parse, the kernel automatically falls back to the copy embedded at compile time (`include_str!("../../config.toml")`).

### Default `config.toml`

```toml
[bare_metal]
network = "live"
wipe_blocks = false
mac_address = "c0:74:2b:f9:15:ec"
ipv4_address = "192.168.1.200"
ipv4_prefix_len = 24
gateway = "192.168.1.1"
dns_server = "192.168.1.1"

[node]
peering_port = 7075
enable_voting = true
enable_bounded_backlog = false
allow_local_peers = true
online_weight_minimum = "130000000000000000000000000000000000000"
representative_vote_weight_minimum = "0"
preconfigured_peers = [
    "peering.nano.org:7075",
    "192.168.1.151:7075",
    "37.120.187.138:7075",
    "66.96.83.211:7075",
    "31.171.245.217:7075",
    "139.59.31.249:7075",
]

[node.bootstrap]
enable = true
enable_priorities = true
enable_dependency_walker = true
enable_frontier_scan = true
rate_limit = 1500
database_rate_limit = 256
frontier_rate_limit = 48
channel_limit = 16
max_requests = 1024
request_timeout = 5000
block_processor_threshold = 16384

[node.block_processor]
max_peer_queue = 65536
max_system_queue = 131072
batch_size = 256

[node.vote_processor]
max_pr_queue = 16384
max_non_pr_queue = 16384
batch_size = 4096
max_triggered = 65536

[node.experimental]
enable_pruning = false
max_pruning_age = 172800
max_pruning_depth = 0
pruning_exempt_accounts = []

[node.websocket]
enable = true
address = "::ffff:0.0.0.0"
port = 7078

[rpc]
enable = true
port = 7076
enable_control = true
```

### WebSocket Subscriptions (`tools/ws_subscribe.py`)

When `[node.websocket]` is enabled (default port `7078`), you can subscribe to live node topics using [`tools/ws_subscribe.py`](file:///mnt/data/rust-workspace/barely-nano/tools/ws_subscribe.py):

```bash
# Stream block confirmations continuously
./tools/ws_subscribe.py

# Subscribe to confirmation and vote topics and exit after 5 events
./tools/ws_subscribe.py -t confirmation -t vote -n 5

# Subscribe to started_election events
./tools/ws_subscribe.py -t started_election -n 5

# Filter confirmations with JSON options
./tools/ws_subscribe.py -t confirmation --options '{"confirmation_type":"active_quorum","include_election_info":true}'
```

### Ledger Pruning

Pruning allows the node to delete old, confirmed blocks to save disk space. In this bare-metal environment, the node uses its **B-Tree with Free-List reclamation** so deleted pages are physically reused on the NVMe SSD.

Set `enable_pruning = true` under `[node.experimental]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml):

```toml
[node.experimental]
enable_pruning = true
max_pruning_age = 172800 # Retain 48 hours of history (in seconds)
max_pruning_depth = 0    # Maximum blocks per account (0 = unlimited depth, age-only)
pruning_exempt_accounts = [
    "nano_1merchantcoldvault...",
    "nano_1merchantsupplierpayout..."
]
```

#### Retaining Full Account History for Merchant & Stealth Invoice Accounts on a Pruned Node
When pruning runs ([`LedgerPruning::ledger_pruning`](file:///mnt/data/rust-workspace/barely-nano/node/src/pruning.rs#L65-L110)), [`LedgerPruning::build_exempt_accounts`](file:///mnt/data/rust-workspace/barely-nano/node/src/pruning.rs#L54-L63) and [`Wallets::collect_pruning_exempt_accounts`](file:///mnt/data/rust-workspace/barely-nano/node/src/wallets/wallets.rs#L673-L716) automatically protect the merchant's own transaction history from ever being pruned while still pruning the rest of the global network:
1. **Explicitly Configured Accounts (`pruning_exempt_accounts`):** Any `nano_...` address listed in `pruning_exempt_accounts` under `[node.experimental]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) is permanently exempt from pruning—ideal for offline Cold Vault, treasury, or accounting addresses whose keys are never loaded onto the node.
2. **Local Wallet & Watch-Only Accounts:** Every deterministic, ad-hoc, stealth-scalar, and watch-only account stored in any local wallet (plus any configured Cold Spend Public Key `spend_public_key`) is automatically exempt from pruning, even if the wallet is currently locked in RAM.
3. **Stateless Stealth Invoice Accounts (`0..deterministic_index`):** During each pruning pass, the node re-derives the wallet's stateless ephemeral/stealth invoice addresses (`0..deterministic_index`) on the fly, checks [`store.confirmation_height`](file:///mnt/data/rust-workspace/barely-nano/node/src/wallets/wallets.rs#L708), and exempts every paid invoice account so its complete `account_history` (`open`, partial payments, and sweep `send` blocks) is preserved indefinitely on NVMe without keeping unpaid invoice addresses in RAM.

### Representative Discovery on Bare-Metal

In a bare-metal environment with an incomplete ledger during initial sync, public representatives may appear to have 0 weight locally until their account chains are synced. Setting `representative_vote_weight_minimum = "0"` under `[node]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) allows observing all voting representatives during initial sync.

## Fast Ledger Bootstrap: Direct LMDB to NVMe Migration (`tools/lmdb-to-nvme`)

Syncing 220M+ blocks from scratch over P2P bootstrap can take many hours. If you have a standard C++ `nano_node` or `rsnano` LMDB ledger snapshot (`data.ldb`), you can attach the 1 TB NVMe SSD to a Linux workstation (e.g. via a USB-C NVMe enclosure at `/dev/sda`) and use [`tools/lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme) ([`tools/lmdb_to_nvme.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb_to_nvme.sh)) to convert and write the entire ledger directly onto the raw NVMe sectors in minutes.

### 1. How the Two-Pass Streaming Converter Works

Standard Nano nodes store their ledger in an LMDB file (`data.ldb`) with separate named sub-databases (`accounts`, `blocks`, `confirmation_height`, `pending`, `rep_weights`, `online_weight`, `peers`, `version`), whereas `barely-nano` uses a bare-metal 16-shard Slotted-Page B-Tree (`53 GB` per shard, `4 KB` pages, 1-byte `table_id` key prefix) plus a `64 MB` on-disk `BlockBloom` filter.

Instead of performing random 4 KB B-Tree insertions across 220M+ records (which would cause severe SSD write amplification), [`tools/lmdb-to-nvme/src/main.rs`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) executes a **2-pass parallel streaming pipeline**:

1. **Startup Self-Test (`verify_btree_builder_self_test`):** Before touching any disk sectors, the tool constructs an in-memory multi-level B-Tree of 5,000 entries and verifies that every page passes `barely-nano`'s exact [`!page_is_full`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage.rs#L1127-L1136) invariant and binary-search lookup semantics.
2. **Pass 1 — Parallel Sequential `data.ldb` Leaf Scan:** Memory-maps `data.ldb` (`PROT_READ`, `MAP_PRIVATE`, `MADV_SEQUENTIAL`) and scans all 4 KB LMDB pages across all CPU cores (~6 GiB/s). Valid leaf pages (`mp_flags == 0x02`, `mp_pgno == pgno`) are parsed and each record is classified by its exact `(key_len, val_len)` and inner sub-tree root page numbers into:
   - `accounts` (`TABLE_ID = 1`, 32B key $\rightarrow$ 129B value)
   - `blocks` (`TABLE_ID = 2`, 32B key $\rightarrow$ 224B compact `StateBlock` value (`32 B` `successor` + `192 B` `StateBlock` with embedded 64-byte `Sideband`) or legacy block + sideband value matching [`Block::deserialize_with_sideband`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L334-L361))
   - `confirmation_height` (`TABLE_ID = 3`, 32B key $\rightarrow$ 40B value)
   - `pruned` (`TABLE_ID = 5`, 32B key $\rightarrow$ 0B value, populated when `--prune` is enabled or if the source `data.ldb` contains pre-existing pruned hashes)
   - `peers` (`TABLE_ID = 6`, 18B key $\rightarrow$ 8B value)
   - `pending` (`TABLE_ID = 7`, 64B key $\rightarrow$ 49B value)
   - `online_weight` (`TABLE_ID = 9`, 8B key $\rightarrow$ 16B value)
   - `rep_weights` (`TABLE_ID = 12`, 32B key $\rightarrow$ 16B value)
3. **Optional Full-History Pruning (`--prune` / `--prune-depth` / `--prune-age`):**
   - When `--prune` is passed, right after Pass 1 the tool builds a 4,096-bucket in-memory [`ConfHeightIndex`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs#L878-L999) in parallel across all 16 shards (~1.5s).
   - During Pass 2, each block's `(account, height, timestamp, successor)` is inspected directly from its raw LMDB bytes in $O(1)$ time via [`should_prune_block`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs#L1003-L1059).
   - The Genesis block, any account head block (`successor == 0`), any unconfirmed block (`height > confirmed_height`), and the top `--prune-depth` confirmed blocks (`height > confirmed_height - max_depth`, default `1` = confirmed frontier) are retained in `TABLE_BLOCKS (2)`.
   - All older confirmed blocks (`1 ..= confirmed_height - max_depth`) are omitted from `TABLE_BLOCKS (2)` and streamed in sorted order into `TABLE_PRUNED (5)` (`0`-byte value), reducing total NVMe B-Tree storage by **~65%** while recording exact `(account_count, block_count, confirmed_count, pruned_count)` in `CACHCNTS`.
4. **Pass 2 — Per-Shard 256-Bucket Parallel Radix Sort & Bottom-Up B-Tree Construction:**
   - Processes all 16 shards (`shard_idx = (key[1] >> 4)`), running Shard `4` last (`b'C' >> 4 == 4`) so that `TABLE_VERSION = 11` receives the exact deduplicated global `CACHCNTS` (`account_count`, `block_count`, `confirmed_count`, `pruned_count`).
   - Within each `(shard, table)`, records are partitioned by their next key byte into 256 strictly ordered sub-buckets, sorted in parallel across all CPU threads, and deduplicated so only the newest LMDB Copy-On-Write record for each key survives.
   - [`StreamingBTreeBuilder`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs#L228-L353) packs sorted records into 4 KB leaf and internal pages at an **~88% fill factor** (`PAGE_TARGET_USED = 3584` bytes, `dead_bytes = 0`, leaving headroom so runtime mutations never trigger an unsplit `page_is_full`) and flushes them in **16 MB sequential chunks** to `/dev/sda`.
5. **64 MB `BlockBloom` Filter & Global Storage Header:**
   - Every unique 32-byte block hash (both full and pruned) sets its bit positions in the 64 MB `BlockBloom` filter written to `BLOOM_DATA_LBA = 524,296` and `BLOOM_HEADER_LBA = 524,288` (`MAGIC = 0x424C4F4D56320006`), enabling $O(1)$ negative block lookups immediately on boot.
   - Zeroes both WAL area headers (`wal_base` and `wal_base + wal_pages / 2`) for every shard and writes the master `STORAGE_MAGIC = 0x5348415244534834` (`"SHARDSH4"`) header to `STORAGE_BASE_LBA = 1,048,576` (`512 MB` offset) last.
   - **Hardware Wallet Preservation:** The isolated hardware wallet region at `WALLET_STORAGE_LBA = 786,432` (`384 MB` offset) is never touched, preserving any existing encrypted representative or wallet keys on the NVMe drive.

### 2. Step-by-Step Usage (`tools/lmdb_to_nvme.sh`)

1. Place your downloaded Nano LMDB snapshot at `database/data.ldb` (or pass any path as the first argument).
2. Connect the Orange Pi RV2's 1 TB NVMe SSD to your host machine via a USB-C NVMe adapter and verify its block device path (e.g. `/dev/sda`) using `lsblk`.
3. **(Optional) Dry Run:** Validate the `data.ldb` file and inspect page classification (and pruning index statistics when `--prune` is passed) without writing to any block device:
   ```bash
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --dry-run
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --prune --dry-run
   ```
4. **Flash Ledger to NVMe (Full or Pruned):** Run the wrapper script (it automatically builds the release binary via `make build-lmdb-to-nvme` and elevates via `sudo` if `/dev/sda` requires root write permissions):
   ```bash
   # Option A: Migrate full unpruned ledger
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda

   # Option B: Prune history on the fly (retains confirmed frontier + unconfirmed blocks per account)
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --prune

   # Option C: Prune history keeping the latest 5 confirmed blocks per account (or blocks newer than 48h)
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --prune --prune-depth 5
   ./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --prune --prune-age 172800
   ```
5. Ensure `wipe_blocks = false` under `[bare_metal]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml), re-insert the NVMe SSD into the Orange Pi RV2, and boot the board. The node will detect `STORAGE_MAGIC` (`"SHARDSH4"`) and `BLOOM_MAGIC` (`"BLOMV2"`), load the exact block/cemented/account/pruned counts from `CACHCNTS` (automatically enabling `enable_pruning` if `pruned_count > 0`), and immediately join live consensus at the snapshot frontier.

## Direct NVMe Ledger Snapshot & Restore (`tools/nvme-snapshot`)

Once your Orange Pi RV2 has finished syncing the live Nano ledger on its NVMe SSD, you can attach the NVMe SSD to a Linux host over USB-C (e.g. `/dev/sda`) and use [`tools/nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) ([`tools/nvme_snapshot.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme_snapshot.sh)) to export a **compact, checksummed raw ledger snapshot file (`BNSNAPV1`)** or restore a snapshot file directly onto any target NVMe SSD in minutes.

### 1. How Sparse-Shard Snapshot Streaming & WAL Replay Works

On a 1 TB NVMe drive, the 16 B-Tree shards are spaced ~60 GB apart across the 1 TB LBA address space (`shard_size = 15,615,908` pages per shard), even though the allocated B-Tree pages (`0 .. next_free_page`) occupy only **~90.4 GiB total** (`~5.65 GiB` per shard) for an unpruned ledger or **~30 GiB total** for a pruned ledger.

Instead of `dd`-ing 1,000 GB of mostly unallocated sectors, [`tools/nvme-snapshot/src/main.rs`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot/src/main.rs) operates directly on `barely-nano`'s native storage structures:

1. **Non-Destructive In-Memory WAL Replay on Dump (`collect_wal_overlay`):**
   - Opens `/dev/sda` strictly **read-only (`O_RDONLY`)** during `dump` and `info`.
   - Inspects both 128 MB WAL areas (`wal_base` and `wal_base + wal_pages / 2`) using `barely-nano`'s exact `WAL_DESC` (`0x57414C5F44455343`), `WAL_CMIT` (`0x57414C5F434D4954`), and 64-bit checksum verification (`common/src/storage/wal.rs`).
   - If the board was powered off while a committed WAL batch had not yet been checkpointed to its home shard pages, those pages are overlaid in memory while streaming the snapshot—guaranteeing a **100% transactionally consistent snapshot** without modifying a single sector on the source NVMe.
2. **Sparse-Shard 16 MB Sequential Streaming (`BNSNAPV1` Format):**
   - Reads the 4 KB `STORAGE_MAGIC` (`"SHARDSH4"`) header at `STORAGE_BASE_LBA = 1,048,576` (`512 MB` mark), extracts `CACHCNTS` (`account_count`, `block_count`, `confirmed_count`, `pruned_count`) from Shard 4's B-Tree, and reads each shard's `MetaData` page (`0xBEEFCB03`).
   - Streams only the allocated pages `0 .. next_free_page` from each of the 16 shards (plus the `64 MB` `BlockBloom` filter at `BLOOM_HEADER_LBA = 524,288`) in **16 MB sequential chunks**, packing them contiguously into the output `.snap` file (~90.4 GiB unpruned vs. 1,000 GB raw disk).
3. **End-to-End 64-Bit Integrity Checksums:**
   - Computes a 64-bit checksum over the 64 MB `BlockBloom` region, each of the 16 B-Tree shard payloads, the optional 16.25 MB wallet partition, and the 4 KB `BNSNAPV1` file header itself.
   - During `restore`, every region's checksum and `BTREE_META_MAGIC` (`0xBEEFCB03`) are verified before committing `STORAGE_MAGIC` (`"SHARDSH4"`) to the target NVMe.
4. **Cross-Drive Capacity Portability on Restore:**
   - When restoring onto a different-sized NVMe SSD (e.g. restoring a snapshot taken from a 1 TB NVMe onto a 256 GB or 512 GB NVMe), `restore` queries the target drive's sector count via `BLKGETSIZE64`, computes the target drive's `shard_size`, `wal_base`, and `wal_pages`, verifies that every shard's `next_free_page` fits within the target `shard_size`, and writes each shard `0..16` to its proper target `base_page = 1 + i * (target_shard_size + 1)`.
5. **Hardware Wallet Isolation (`--include-wallets` Optional):**
   - By default, both `dump` and `restore` **skip** the isolated hardware wallet partition at `WALLET_STORAGE_LBA = 786,432` (`384 MB` mark), so restoring a ledger snapshot onto a node never overwrites its eFuse-encrypted representative or wallet keys. Pass `--include-wallets` to include or restore the 16.25 MB wallet B-Tree + redo journal when cloning an entire node onto the same board.

### 2. `BNSNAPV1` Raw Snapshot File Layout

Every `.snap` file generated by `nvme-snapshot dump` uses a deterministic, page-aligned (`4 KB`) binary container (`SNAP_MAGIC = 0x3156_5041_4E53_4E42`, `"BNSNAPV1"`):

| File Offset | Size | Region | Description |
| :--- | :--- | :--- | :--- |
| `0x0000_0000` | `4 KB` (`1` page) | **`BNSNAPV1` Header** | Magic (`"BNSNAPV1"`), flags (`FLAG_HAS_BLOOM`, `FLAG_HAS_WALLETS`, `FLAG_PRUNED`), timestamp, source `shard_size` & `wal_epoch`, cached ledger counts (`accounts`, `blocks`, `confirmed`, `pruned`), 64-bit region checksums, and 16 × 32-byte [`ShardDescriptor`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot/src/main.rs#L67-L74) entries (`root_page_id`, `next_free_page`, `first_free_page`, `file_offset_bytes`, `checksum`) |
| `0x0000_1000` | `64 MB + 4 KB` (`16,385` pages) | **`BlockBloom` Filter** | Raw copy of `BLOOM_HEADER_LBA` (`524,288`) + `BLOOM_DATA_LBA` (`524,296`, `64 MB` bitset) for instant $O(1)$ negative block lookups on boot |
| *(Optional)* | `16.25 MB` (`4,161` pages) | **Wallet Partition** | Present only when `--include-wallets` is passed; holds `WALLET_STORAGE_LBA` (`786,432`, `4,096` B-Tree pages + `1` header + `64` journal pages) |
| Contiguous | `next_free_page × 4 KB` per shard | **Shards `0..15` B-Tree Pages** | Only the allocated pages (`0 .. next_free_page`) of each of the 16 B-Tree shards (~90.4 GiB total unpruned / ~30 GiB pruned), with any committed WAL pages already overlaid in place |

### 3. Step-by-Step Usage (`tools/nvme_snapshot.sh`)

1. Power off the Orange Pi RV2, move the NVMe SSD into a USB-C NVMe enclosure on your Linux host, and check its device path with `lsblk` (e.g. `/dev/sda`).
2. **Inspect the NVMe SSD (`info`):** View the live ledger counts (`accounts`, `blocks`, `confirmed`, `pruned`), WAL status, and allocated page counts per shard without modifying the drive:
   ```bash
   ./tools/nvme_snapshot.sh info /dev/sda
   ```
3. **Create a Raw Ledger Snapshot File from NVMe (`dump`):** Stream the synced NVMe ledger into a compact `.snap` file (automatically builds `nvme-snapshot` via `make build-nvme-snapshot` and requests `sudo` if `/dev/sda` requires root read access):
   ```bash
   # Optional: Dry-run validation to preview snapshot size without writing a file
   ./tools/nvme_snapshot.sh dump /dev/sda database/nano_ledger.snap --dry-run

   # Dump the synced ledger from /dev/sda to database/nano_ledger.snap
   ./tools/nvme_snapshot.sh dump /dev/sda database/nano_ledger.snap

   # Optional: Include the 16.25 MB hardware-bound wallet partition in the snapshot
   ./tools/nvme_snapshot.sh dump /dev/sda database/nano_ledger.snap --include-wallets
   ```
4. **Inspect the Generated Snapshot File (`info`):**
   ```bash
   ./tools/nvme_snapshot.sh info database/nano_ledger.snap
   ```
5. **Write / Restore the Raw Snapshot File Back to an NVMe SSD (`restore`):**
   ```bash
   # Optional: Verify all 64-bit shard checksums and target NVMe capacity without writing to /dev/sda
   ./tools/nvme_snapshot.sh restore database/nano_ledger.snap /dev/sda --dry-run

   # Write the raw snapshot back onto /dev/sda (preserves target NVMe wallets at 384 MB by default)
   ./tools/nvme_snapshot.sh restore database/nano_ledger.snap /dev/sda

   # Optional: Also restore the wallet partition (only if restoring to the same OrangePi RV2 board)
   ./tools/nvme_snapshot.sh restore database/nano_ledger.snap /dev/sda --include-wallets
   ```
6. Re-insert the NVMe SSD into the Orange Pi RV2 (with `wipe_blocks = false` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml)) and boot. The node will immediately mount all 16 shards and the 64 MB `BlockBloom` filter with zero WAL recovery needed.

## Network Stress Testing (`tools/nano-stress`)

The repository provides a high-throughput stress testing tool (`tools/nano-stress`) to test node stability, B-Tree storage performance, and queue behavior under heavy load by generating cryptographically valid blocks and streaming them directly over TCP to port **7075**.

> [!TIP]
> **Hardware Benchmark Results:** On the OrangePi RV2 (SpacemiT K1 8-core RISC-V SoC), the node achieves **21,002.5 blocks/second sustained end-to-end** (`4.76s` for 100,000 blocks), **25,368.0 blocks/second sustained TCP ingress** (`28,771 bps` peak batch = `6,293.7 KB/s`), and **22,396.4 – 28,328.6 blocks/second active pipeline throughput** (`19.32 – 19.92 µs/block` parallel B-Tree ledger commit time across Harts 5 & 6) with full Ed25519 signature verification and RVV Blake2b PoW validation. See the full [Stress Test & Throughput Report (`STRESS_TEST_REPORT.md`)](file:///mnt/data/rust-workspace/barely-nano/STRESS_TEST_REPORT.md) for detailed stage-by-stage hardware profiling and multi-hart metrics.

### 1. Node Dev Network Setup

To prepare the node for stress testing, set the following in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) on the SD card:

```toml
[bare_metal]
network = "dev"
wipe_blocks = true         # Wipes NVMe B-Tree shards to start fresh from dev genesis
ipv4_address = "192.168.1.200"

[node]
peering_port = 7075        # Port where blocks will be published
enable_voting = true
allow_local_peers = true
online_weight_minimum = "0"
representative_vote_weight_minimum = "0"
preconfigured_peers = []   # Do not connect to public live peers in dev mode

[rpc]
enable = true
port = 7076
enable_control = true
```

Reboot the board. The node will initialize the dev ledger with `DEV_GENESIS_BLOCK` and start listening on ports `7075` (peering), `7076` (RPC), and `7078` (WebSocket).

### 2. Running the Stress Test

```bash
# Build the tool and stream default batch (1,000 blocks at 500 BPS to 192.168.1.200:7075)
make run-stress
# (or using alias)
make stress

# Benchmark block generation locally without network transmission (dry run)
make run-stress ARGS="--dry-run --blocks 10000"

# Stream 5,000 blocks across 10 independent accounts at a controlled rate of 500 BPS
make run-stress ARGS="--blocks 5000 --rate 500 --accounts 10"

# Unlimited throughput burst test across 20 independent accounts
make run-stress ARGS="--blocks 5000 --rate 0 --accounts 20"
```

> [!NOTE]
> Do **not** pass `--zero-pow` when streaming to the node. The node validates block entry thresholds against dev network rules (`0xf000000000000000`). Dev PoW calculation takes only ~100 µs on x86_64, ensuring all blocks pass ledger entry criteria.

## Workspace Structure
- [`STRESS_TEST_REPORT.md`](file:///mnt/data/rust-workspace/barely-nano/STRESS_TEST_REPORT.md) — Hardware stress test campaign results, peak/sustained BPS benchmarks, and per-hart microarchitectural profiling on the OrangePi RV2.
- [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) — Declarative TOML configuration for bare-metal network, node, bootstrap, queues, RPC, and WebSocket server.
- [`main/`](file:///mnt/data/rust-workspace/barely-nano/main) — Main bare-metal binary entry point (`entry.S`, `bin.rs`) and per-hart loops.
- [`common/`](file:///mnt/data/rust-workspace/barely-nano/common) — Core kernel library: S-mode trap handler, UART/SBI/U-Boot wrappers, OLED driver, 16-shard B-Tree + DRAM Bank 1 BPM + atomic WAL storage engine, cooperative async executor, and `smoltcp` networking stack.
- [`core/`](file:///mnt/data/rust-workspace/barely-nano/core) — Core Nano primitives (blocks, accounts, keys, amounts, votes, difficulty).
- [`messages/`](file:///mnt/data/rust-workspace/barely-nano/messages) — Nano P2P wire protocol messages and serialization.
- [`ledger/`](file:///mnt/data/rust-workspace/barely-nano/ledger) & [`ledger_store/`](file:///mnt/data/rust-workspace/barely-nano/ledger_store) — Ledger state management, block insertion/validation, and B-Tree store tables.
- [`network/`](file:///mnt/data/rust-workspace/barely-nano/network) & [`network_protocol/`](file:///mnt/data/rust-workspace/barely-nano/network_protocol) — P2P channel management, handshakes, and message framing.
- [`node/`](file:///mnt/data/rust-workspace/barely-nano/node) — High-level Nano node (consensus, active elections, vote processor, bootstrapper, cementation, telemetry, TOML config).
- [`rpc_messages/`](file:///mnt/data/rust-workspace/barely-nano/rpc_messages), [`rpc_handler/`](file:///mnt/data/rust-workspace/barely-nano/rpc_handler), [`rpc_server/`](file:///mnt/data/rust-workspace/barely-nano/rpc_server) — Bare-metal `no_std` JSON-RPC message types, command handlers, and HTTP server.
- [`websocket_messages/`](file:///mnt/data/rust-workspace/barely-nano/websocket_messages), [`websocket_client/`](file:///mnt/data/rust-workspace/barely-nano/websocket_client), [`websocket_server/`](file:///mnt/data/rust-workspace/barely-nano/websocket_server) — Bare-metal `no_std` WebSocket envelopes, client, and real-time event broadcast server.
- [`nullables/`](file:///mnt/data/rust-workspace/barely-nano/nullables) — Hardware/OS abstraction crates (`sha` RVV/Zbb crypto, `clock`, `random`, `tcp`, `http_client`, `output_tracker`) with nullable test doubles.
- [`stats/`](file:///mnt/data/rust-workspace/barely-nano/stats) & [`work/`](file:///mnt/data/rust-workspace/barely-nano/work) — Node statistics collector and PoW work generation/validation.
- [`tools/`](file:///mnt/data/rust-workspace/barely-nano/tools) — Host-side utilities: [`lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme) & [`lmdb_to_nvme.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb_to_nvme.sh) (direct `data.ldb` to raw NVMe B-Tree + Bloom migration tool), [`nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) & [`nvme_snapshot.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme_snapshot.sh) (direct NVMe to compact raw ledger snapshot dump & restore tool), [`nano-stress`](file:///mnt/data/rust-workspace/barely-nano/tools/nano-stress) (P2P block streamer), and [`ws_subscribe.py`](file:///mnt/data/rust-workspace/barely-nano/tools/ws_subscribe.py) (WebSocket topic subscriber).
- [`u-boot-orangepi-k1/`](file:///mnt/data/rust-workspace/barely-nano/u-boot-orangepi-k1) — Git submodule pointing to [`caiovini/u-boot-orangepi-k1`](https://github.com/caiovini/u-boot-orangepi-k1.git) (`CONFIG_API=y` U-Boot tree and `prebuilt/` bootloader images for Orange Pi RV2).
- [`linker-smode.ld`](file:///mnt/data/rust-workspace/barely-nano/linker-smode.ld) — S-mode linker script mapping physical memory layout for the Orange Pi RV2.
