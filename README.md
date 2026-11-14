# Bare-Metal RISC-V Nano Node (barely-nano)

This project is a bare-metal `no_std` RISC-V fork of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node), designed to run a full Nano cryptocurrency node directly on the **SpacemiT K3 Pico-ITX** with 16 RISC-V harts (`8× X100 @ 2.4 GHz, VLEN=256` + `8× A100 @ 2.0 GHz, VLEN=1024`), **16 GB LPDDR5 RAM**, Realtek RTL8127 10GbE SFP+, Waveshare 5-inch HDMI SimpleFB display, and a 1 TB NVMe SSD.

It establishes a self-contained operating environment on top of OpenSBI and U-Boot (`CONFIG_API=y`), complete with a 3 GB heap allocator, 16-hart exception handling, RVA23/`Zvkb` (`VLEN=256` & `VLEN=1024`) hardware-accelerated cryptography, a 64-shard B-Tree + 12 GB High-DRAM Buffer Pool + atomic Write-Ahead Log (WAL) storage engine on NVMe, a full TCP/IP networking stack, an HTTP JSON-RPC server, and a real-time WebSocket server.

## Table of Contents

- [Features Implemented](#features-implemented)
  - [1. S-Mode Execution on OpenSBI & U-Boot](#1-s-mode-execution-on-opensbi--u-boot)
  - [2. Memory Management](#2-memory-management)
  - [3. Exception and Trap Handling](#3-exception-and-trap-handling)
  - [4. Hardware Watchdogs & Timers](#4-hardware-watchdogs--timers)
  - [5. Hardware Accelerated Cryptography & Optimizations](#5-hardware-accelerated-cryptography--optimizations)
  - [6. U-Boot Networking, RPC & WebSocket Stack](#6-u-boot-networking-rpc--websocket-stack)
  - [7. 16-Hart Hybrid SMP Architecture (`8× X100` + `8× A100`)](#7-16-hart-hybrid-smp-architecture-8-x100--8-a100)
  - [8. Waveshare 5-Inch HDMI SimpleFB Display (`common/src/hdmi_fb.rs`)](#8-waveshare-5-inch-hdmi-simplefb-display-commonsrchdmi_fbrs)
- [Storage Architecture: Bare-Metal 64-Shard NVMe B-Tree + 12 GB Buffer Pool + WAL Engine](#storage-architecture-bare-metal-64-shard-nvme-b-tree--12-gb-buffer-pool--wal-engine)
  - [Why the Bare-Metal NVMe Ledger is ~68–86 GB vs. ~123 GB in LMDB (& Architectural Advantages)](#why-the-bare-metal-nvme-ledger-is-6886-gb-vs-123-gb-in-lmdb--architectural-advantages)
- [Representative & Wallet Key Security Architecture (`wallet_crypto`)](#representative--wallet-key-security-architecture-wallet_crypto)
  - [1. Physical & Cryptographic Isolation](#1-physical--cryptographic-isolation)
  - [2. Two Operator Modes: Unattended Auto-Voting vs. Passphrase Lock on Reboot](#2-two-operator-modes-unattended-auto-voting-vs-passphrase-lock-on-reboot)
  - [3. Configuring a Representative via RPC (Port `7076`)](#3-configuring-a-representative-via-rpc-port-7076)
  - [4. Dynamic Invoice QR & Watch-Only Stealth Derivation](#4-dynamic-invoice-qr--watch-only-stealth-derivation)
- [Hardware Requirements](#hardware-requirements)
- [Step-by-Step Setup: Toolchain, U-Boot, Fastboot / FDL & NVMe Boot Partition](#step-by-step-setup-toolchain-u-boot-fastboot--fdl--nvme-boot-partition)
  - [1. Install Host Prerequisites (RISC-V Tools, `fastboot`, `mkimage` & Rust Target)](#1-install-host-prerequisites-risc-v-tools-fastboot-mkimage--rust-target)
  - [2. Build Custom SpacemiT K3 U-Boot (`/mnt/data/rust-workspace/u-boot`) & Flash via Fastboot / FDL](#2-build-custom-spacemit-k3-u-boot-mntdatarust-workspaceu-boot--flash-via-fastboot--fdl)
  - [3. Create the `NANO_NODE` Boot Partition on NVMe (`nvme 0:1`, First 255 MB) or eMMC (`mmc 0:1`)](#3-create-the-nano_node-boot-partition-on-nvme-nvme-01-first-255-mb-or-emmc-mmc-01)
  - [4. Force SpacemiT K3 Pico-ITX to Probe 10GbE, NVMe & HDMI via `boot.scr`](#4-force-spacemit-k3-pico-itx-to-probe-10gbe-nvme--hdmi-via-bootscr)
  - [5. Build Firmware & Deploy `barely-nano.bin`, `boot.scr`, and `config.toml`](#5-build-firmware--deploy-barely-nanobin-bootscr-and-configtoml)
- [Build & Deployment System](#build--deployment-system)
  - [Build & Test Targets](#build--test-targets)
- [Deployment & Configuration (`config.toml`)](#deployment--configuration-configtoml)
  - [Live Configuration Loading (`0x1_03F0_0000` — No Recompilation Needed)](#live-configuration-loading-0x1_03f0_0000--no-recompilation-needed)
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
- [Authors & Credits](#authors--credits)

## Features Implemented

### 1. S-Mode Execution on OpenSBI & U-Boot
The kernel runs in **Supervisor Mode (S-mode)** launched from U-Boot on top of OpenSBI firmware, utilizing SBI `ecall`s for multi-hart management (HSM) and IPIs, and U-Boot's Standalone Jump Table (`gd->jt` at `gp + 0x20`) for 10GbE Ethernet, HDMI SimpleFB, and NVMe block device I/O.
- **Boot & Firmware Delivery (USB Fastboot / FDL + NVMe Partition 1 `nvme 0:1` or Onboard eMMC `mmc 0:1`):** Because the SpacemiT K3 Pico-ITX has no MicroSD slot, U-Boot / OpenSBI is flashed over USB Type-C using **SpacemiT FDL / `fastboot`**, while `boot.scr`, `barely-nano.bin`, and `config.toml` are loaded from a `255 MB` FAT32/ext4 boot partition at the start of the NVMe SSD (`nvme 0:1`, `LBA 2048..524,287` — strictly below `BLOOM_HEADER_LBA = 524,288`), onboard eMMC (`mmc 0:1`), or staged directly via `fastboot` / PXE.
- **Ledger Media (1 TB NVMe SSD):** Automatically discovered via U-Boot's primary NVMe namespace (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at Jump Table indices `56..58`), with raw ledger structures starting at `256 MB` (`BLOOM_HEADER_LBA = 524,288`), `384 MB` (`WALLET_STORAGE_LBA = 786,432`), and `512 MB` (`STORAGE_BASE_LBA = 1,048,576`).

### 2. Memory Management
- **Boot sequence:** Written in custom assembly (`main/src/entry.S`) to set up stack pointers for all 16 harts, capture U-Boot's Jump Table pointer, zero BSS, and jump into Rust at `0x1_0400_0000`.
- **Heap Allocator (`0x1_1800_0000 .. 0x1_D800_0000`):** Uses `linked_list_allocator` wrapped in `InterruptSafeAllocator` to provide a global dynamic memory allocator (`ALLOCATOR`) configured in `linker-smode.ld` with a **3.00 GB heap** and **256 MB stack** (16 MB per hart across 16 harts).
- **High DRAM Sector Cache (`0x1_E000_0000 .. 0x4_E000_0000`):** Directly utilizes the SpacemiT K3 16 GB LPDDR5 memory window (`0x1_E000_0000+`) to host a **12.00 GB B-Tree Buffer Pool** (`49,152` × 4 KB frames per shard across 64 shards).

### 3. Exception and Trap Handling
- **Trap Vector:** Custom S-mode assembly trap vector (`trap_vector`) saves and restores all 31 GPRs upon interrupts and exceptions and enables `sstatus.FS` and `sstatus.VS` for floating-point and RVV 1.0 vector instructions across all 16 harts (`VLEN=256` on X100 Harts `0..7` and `VLEN=1024` on A100 Harts `8..15`).
- **Trap Dispatcher:** Evaluates the `scause` CSR to differentiate between asynchronous interrupts and synchronous exceptions, with atomic probe recovery (`trap::probe_read_u32` / `trap::probe_rw_u64`) safe across 16 concurrent harts.
- **Diagnostics:** Exceptions automatically print the hart ID, `scause`, `sepc`, `stval`, and `ra` registers over SpacemiT K3 UART0 (`0xD401_7000`).

### 4. Hardware Watchdogs & Timers
- **CLINT Timer:** Reads the 24 MHz hardware `time` CSR (`rdtime`) for cooperative async scheduling, profiling, and timestamps.
- **Watchdog Management:** Trap-safely probes, disables, and periodically kicks both the SpacemiT K3 SoC watchdog (`0xD408_0000`) and the PMIC watchdog over PWR I2C bus 8 (`0xD401_D800`).

### 5. Hardware Accelerated Cryptography & Optimizations
To achieve maximum transaction throughput across the hybrid 16-core silicon, the node compiles with the full SpacemiT K3 RVA23 extension set (`+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicbop,+zicboz,+zicond,+zihintntl,+zihintpause,+zvbb,+zvbc,+zvkb`):
- **Vector Crypto Bit-Manipulation (`Zvkb`, `Zvbb`, `Zvbc`):** Single-instruction native vector rotates (`vror.vi v, v, 32/24/16/63`) in [`nullables/sha/src/blake2b.rs`](file:///mnt/data/rust-workspace/barely-nano/nullables/sha/src/blake2b.rs), replacing multi-instruction shift-and-XOR sequences across every Blake2b G-mixing round.
- **Hybrid `VLEN=256` (4-Way) & `VLEN=1024` (16-Way) RVV 1.0 Batching:**
  - **X100 Harts `2..3` (`VLEN=256`):** Execute zero-spill `LMUL=1` 4-way Blake2b (`compress_four_blocks`, `hash_batch_4`, `hash64_batch_4`) and 4-way Ed25519 signature verification (`verify_batch_4`).
  - **A100 Harts `8..14` (`VLEN=1024`):** Runtime `csrr vlenb` (`>= 128`) detection in [`has_vlen_1024()`](file:///mnt/data/rust-workspace/barely-nano/nullables/sha/src/blake2b.rs) and [`SignatureChecker::help_verify()`](file:///mnt/data/rust-workspace/barely-nano/node/src/signatures.rs) automatically dispatches **16-way lockstep SIMD** Blake2b (`compress_16_blocks`, `hash_batch_16`, `hash64_batch_16`) and 16-way batch Montgomery inversion Ed25519 verification (`verify_batch_16`), verifying 16 blocks per call on each of the 7 A100 crypto harts.
- **Scalar Bit-Manipulation & Conditional Ops (`Zba`, `Zbb`, `Zbs`, `Zbc`, `Zicond`):** Single-cycle shifted address generation (`sh1add`, `sh2add`, `sh3add`), branchless conditional moves (`czero.eqz`/`czero.nez`), and scalar `rori` rotations.
- **Cache-Block Prefetch (`Zicbop`, `Zihintntl`):** Inline `prefetch.r` and `prefetch.w` hints ([`ledger_store/src/btree/arena.rs`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/btree/arena.rs)) during B-Tree binary search, child page descent, and leaf mutation, plus branchless 8-byte big-endian `u64` prefix comparisons ([`NodePage::search_slot`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/btree/node.rs#L156-L202)).
- **Cache-Block Zero & Management (`Zicboz`, `Zicbom`, `Zihintpause`):** Fast 64-byte cache-line zeroing (`cbo.zero`), explicit cross-cluster cache maintenance (`cbo.clean`, `cbo.inval`), and adaptive `pause` spinloop backoff.

### 6. U-Boot Networking, RPC & WebSocket Stack
- **10GbE SFP+ Hardware Probing (`RTL8127ATF`):** Discovers and drives the Realtek RTL8127 10GbE fiber controller via the U-Boot Standalone Jump Table API (`eth_init`, `eth_send`, `eth_rx`, `eth_receive` at indices `42..48`) with 64 TX / 512 RX 64-bit PCIe DMA descriptors, batched 4-descriptor cache-line RX re-arming, and burst RX draining (up to 64 frames per poll).
- **TCP/IP Stack (`smoltcp` 0.12):** Full-featured `no_std` TCP/IP/DNS/ICMP stack integrated with the cooperative async executor.
- **Nano Services:**
  - **Port `7075` (Peering):** Live Nano P2P protocol peering and bootstrap (`96` concurrent peers).
  - **Port `7076` (HTTP JSON-RPC):** Full `no_std` Nano RPC command handler (`rsnano_rpc_messages`, `rsnano_rpc_handler`, `rsnano_rpc_server`).
  - **Port `7078` (WebSocket Server):** Full `no_std` WebSocket server and client (`rsnano_websocket_messages`, `rsnano_websocket_client`, `rsnano_websocket_server`) streaming live node events (`confirmation`, `vote`, `started_election`, `stopped_election`, `telemetry`, `bootstrap`, `new_unconfirmed_block`, etc.).
  - **Port `7080` (Live Web Dashboard & `/api/metrics`):** Built-in HTTP telemetry dashboard ([`main/src/dashboard.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/dashboard.rs)) serving a real-time web UI and `/api/metrics` JSON endpoint for hardware profiling and WAL/B-Tree monitoring.

### 7. 16-Hart Hybrid SMP Architecture (`8× X100` + `8× A100`)
All 16 harts on the SpacemiT K3 are awakened via OpenSBI HSM (`sbi::hart_start`) and pinned by physical CPU cluster in [`main/src/bin.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs):

- **Cluster 0 — X100 Harts `0..=3` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb` — Ingress, Network & 4-Way Crypto):**
  - **Hart 0 (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 1 (`Maint+Net`):** Boot initialization, 10GbE `smoltcp` network polling, HTTP JSON-RPC (`7076`), WebSocket (`7078`), Live Web Dashboard (`7080`), gateway ICMP keepalive, and watchdog kicking.
  - **Harts 2 & 3 (`Crypto-X100`):** Dedicated 4-way `VLEN=256` (`Zvkb`) Ed25519 signature verification workers (`signature_checker.help_verify()`) and inbound `MessageProcessor::run_batch()` drainers.
- **Cluster 1 — X100 Harts `4..=7` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb` — Consensus & 3-Stripe Parallel 64-Shard Ledger Storage):**
  - **Hart 4 (`Consensus`):** Drives `aec_voter` and `aec_ticker` (300 ms ticks), `election_schedulers`, `backlog_scan`, `bootstrap_server`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 5 (`Ledger-B`):** Parallel **Shards `21..=41`** (`[21, 42)`, 21 shards) B-Tree mutations ([`Ledger::try_run_ledger_b_job`](file:///mnt/data/rust-workspace/barely-nano/ledger/src/ledger.rs)), WAL segments, and `unchecked` dependency queue drain.
  - **Hart 6 (`Ledger-A`):** Primary `block_processor` batch coordinator (`FAST_BATCH_SLOTS = 2048`), parallel **Shards `0..=20`** (`[0, 21)`, 21 shards) B-Tree mutations, WAL checkpoint coordinator, and `confirming_set` cementation.
  - **Hart 7 (`Ledger-C + Storage`):** Parallel **Shards `42..=63`** (`[42, 64)`, 22 shards) B-Tree mutations ([`Ledger::try_run_ledger_c_job`](file:///mnt/data/rust-workspace/barely-nano/ledger/src/ledger.rs)), online weight calculation, NVMe Free-List GC/reclamation (`ledger_pruning`), and cooperative async executor tasks (`WORKER_HARTS`).
- **Clusters 2 & 3 — A100 Harts `8..=15` (`2.0 GHz`, `VLEN=1024`, `Zvkb` — 16-Way Wide-Vector Crypto & HDMI Telemetry):**
  - **Harts `8..=14` (`Crypto-A100`):** 7 dedicated **16-way `VLEN=1024` SIMD** Ed25519 signature verification (`verify_batch_16`) + Blake2b (`compress_16_blocks`) workers and inbound `MessageProcessor::run_batch()` drainers.
  - **Hart `15` (`HDMI+Mon`):** Drives the 2-second Waveshare 5-inch HDMI SimpleFB graphical dashboard ([`common/src/hdmi_fb.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/hdmi_fb.rs)) and 15-second hardware telemetry/lock-contention reports.

### 8. Waveshare 5-Inch HDMI SimpleFB Display (`common/src/hdmi_fb.rs`)
- **Zero-Allocation `no_std` Framebuffer Driver:** Automatically discovers the `simple-framebuffer` (`simplefb`) region initialized by U-Boot's HDMI driver from the Flattened Device Tree (`0x1_1F00_0000`) or U-Boot Jump Table (`video_get_fb`, `video_get_xsize`, `video_get_ysize`, `video_sync_all` at indices `49..55`), supporting both `r5g6b5` (`RGB565`, 16 bpp) and `x8r8g8b8` / `a8r8g8b8` (`XRGB8888`, 32 bpp) panels (`800×480` Waveshare 5-inch HDMI LCD).
- **Live Graphical Dashboard on Hart 15:** Renders node header, connected peer count, cemented/total block counts, live BPS rate, active elections (`AEC`), 64-shard NVMe DB footprint, and 12 GB High-DRAM Buffer Pool utilization every 2 seconds on Hart 15, flushing dirty cache lines via `cbo.clean` + `video_sync_all`.

## Storage Architecture: Bare-Metal 64-Shard NVMe B-Tree + 12 GB Buffer Pool + WAL Engine

Without an OS filesystem or traditional database (LMDB/RocksDB), `barely-nano` implements a crash-resilient **64-Shard B-Tree + 12 GB High-DRAM Buffer Pool + Atomic Write-Ahead Log (WAL)** directly on the 1 TB PCIe Gen3 ×4 NVMe SSD:

1. **Physical Layer (`StorageBackend::UBoot`):** Transfers raw 4 KB pages (`8 × 512 B` sectors, batched up to 64 pages / 256 KB per command via zero-copy 64-bit NVMe PRP DMA) over U-Boot's Jump Table block interface (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at indices `56..58`, `NVME_Q_DEPTH = 64`) directly from High DRAM (`0x1_E000_0000+`) to the NVMe SSD. (`STORAGE_BASE_LBA = 1,048,576` leaves a 512 MB safety margin at the start of the drive, with a 64 MB persistent Bloom filter at `BLOOM_HEADER_LBA = 524,288`.)
2. **12 GB High-DRAM Buffer Pool Manager (`common/src/storage/bpm.rs`):** Manages `49,152` × 4 KB frames per shard (`192 MB/shard × 64 shards = 12.00 GB` target, `51,200` frames = `12.50 GB` max) in SpacemiT K3 LPDDR5 (`0x1_E000_0000 .. 0x4_E000_0000`), paired with compact 24-byte `FrameMeta` entries in the 3.00 GB Heap (`0x1_1800_0000 .. 0x1_D800_0000`), an open-addressed page hash table, and a **Clock (second-chance)** eviction policy. Dirty frames are **never written during eviction**, preventing partial cross-shard updates from reaching disk.
3. **Atomic Dual-Area Write-Ahead Log (`common/src/storage/wal.rs`):** Reserves `256 MB` (`2 × 128 MB` alternating areas) on NVMe. Between block batches on Cluster 1, `wal::checkpoint()` snapshots dirty pages across all 64 shards in parallel, writes `[descriptor pages][data pages][commit page]` sequentially with a 64-bit checksum and boot epoch, and then writes the pages to their home shard locations sorted by LBA. On boot, `wal::recover()` idempotently replays the latest committed batch.
4. **64-Shard B-Tree Engine (`ledger_store/src/btree/` & `common/src/storage/btree.rs`):** Prefix-partitioned across 64 shards by the top 6 bits of `key[1]` (`(key[1] >> 2) as usize`, striped across Hart 6 Shards `0..=20`, Hart 5 Shards `21..=41`, and Hart 7 Shards `42..=63`), supporting `Zicbop`-prefetched (`prefetch.r`/`prefetch.w`) branchless 8-byte `u64` prefix binary search, proactive-split inserts, sibling borrow/merge deletes with embedded free-list page reclamation, and stack-cached range iterators.
5. **Isolated Wallet B-Tree & Atomic Redo Journal (`WALLET_SHARD` at `WALLET_STORAGE_LBA = 786,432`):** Wallet and representative keys live in a dedicated B-Tree partition at the `384 MB` NVMe mark (`LBA 786,432 .. 819,712`, `16 MB` B-Tree + `256 KB` checksummed redo journal), strictly below `STORAGE_BASE_LBA` (`512 MB`) and enforced by compile-time assertions and runtime LBA bounds guards so wallet mutations can never overwrite or contend with a synced ledger.

### Why the Bare-Metal NVMe Ledger is ~68–86 GB vs. ~123 GB in LMDB (& Architectural Advantages)

On the exact same unpruned Nano mainnet ledger (**220.89M blocks**, **37.44M accounts**, **218.53M confirmed**), the standard LMDB `data.ldb` file occupies **~122.2 GiB (`131.2 GB` / `32,029,037` × 4 KB pages)**, whereas `barely-nano`'s 64-shard NVMe B-Tree with **Bit-Packed Compact Block Encoding + Top-256 Representative Indexing** occupies **~68 GiB (`~86 GiB` steady-state after long-term random live splits)**—saving **~36–54 GiB** before even enabling pruning:

| Metric | Standard Linux Node (LMDB `data.ldb`) | `barely-nano` Bare-Metal NVMe B-Tree (SpacemiT K3) | Advantage |
| :--- | :--- | :--- | :--- |
| **Unpruned Ledger Size (220.9M blocks)** | **~122.2 GiB (`131.2 GB`)** (`32,029,037` pages) | **~68 GiB** after `lmdb-to-nvme` (**~86 GiB** steady-state) | **~36–54 GiB smaller** on disk |
| **Pruned Ledger Size (`--prune`)** | ~45–55 GiB (requires offline `mdb_copy -c` to reclaim file size) | **~24–28 GiB** (reclaims freed 4 KB pages in-place via shard free-list) | **~78% smaller** than unpruned LMDB; zero offline compaction needed |
| **Block Cell Size (`TABLE_BLOCKS`)** | **309 B** (`32 B` key + `267 B` value + `10 B` node hdr; max **13 blocks/page**) | **209.4 B hot frontier / 260.4 B cold** (`251.3 B` weighted avg; max **16 blocks/page**) | **+23% more blocks per 4 KB page** + skips Blake2b re-hashing on read |
| **Leaf Page Fill Factor** | **~67–69%** average (`50%` immediately after random 256-bit hash page splits) | **~88–95%** initial (`PAGE_TARGET_USED = 3,584 B`, `0` dead bytes; ~75% steady-state) | Eliminates internal B+Tree split slack and compacts dead slot bytes in-place |
| **Copy-on-Write (COW) & Freelist Overhead** | **~8.7 GiB** (`~2.26M` pages in branch nodes, un-reclaimed COW pages & `FREE_DBI` lists) | **0 B COW bloat** (in-place page updates + fixed `256 MB` dual-area WAL) | Eliminates multi-version page duplication and freelist fragmentation |
| **Live Growth Rate per `+10M` Blocks** | **~5.6 GB / 10M blocks** (COW leaf + ancestor branch path writes) | **~3.8 GB / 10M blocks** (steady-state with 16 blocks/page capacity) | **~32% slower ledger growth rate** for non-pruned Principal Representative nodes |
| **SSD Write Amplification & Filesystem Overhead** | High (every commit writes COW leaf-to-root path + ext4/XFS journal + `msync`) | **Minimal** (dirty pages stay in 12 GB High-DRAM Buffer Pool; flushed in LBA-sorted 256 KB zero-copy PRP DMA batches) | Drastically lower NVMe wear and higher sustained IOPS |
| **RAM & Cache Predictability (16 GB Board)** | Unbounded `mmap` RSS + OS page-cache thrashing + Linux OOM risk | **Fixed `12.00 GB` High-DRAM Sector Cache (`64 shards × 192 MB`) + `3.00 GB` Heap** | Zero OS page faults, zero swap, deterministic memory ceiling |
| **Negative Block Lookup Cost** | Full B+Tree root-to-leaf traversal (`3–4` page reads per check) | **$O(1)$ in-RAM `64 MB` `BlockBloom` filter** (persisted on NVMe) | Instant duplicate/existence filtering before touching B-Tree shards |
| **CPU Scheduling & Execution Model** | Linux CFS thread migration, syscalls, interrupts, and multi-thread DB lock contention | **Hard-pinned 16-Hart Hybrid SMP** (2× X100 4-way + 7× A100 16-way `VLEN=1024` Crypto harts, 3-Stripe Parallel 64-Shard Ledger harts on Cluster 1) | Zero context-switch or syscall overhead; 16-way `VLEN=1024` SIMD + 3-stripe 64-shard B-Tree parallelism |

#### Why the Storage Size Difference Occurs
1. **Self-Contained Bit-Packed Compact Block Encoding & Top-256 Representative Table (~22 GiB saved):**
   - In [`SavedBlock::serialize_compact`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L862) and [`SavedBlock::deserialize_compact`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L1026) (`COMPACT_BLOCK_TAG = 0x80`), `barely-nano` eliminates redundant fields inside a `3`-byte bit-packed header:
     - **Hot Frontier Elision (`successor == 0`):** Active account head blocks (`~17.9%` of the ledger) omit the trailing 32 zero bytes of `successor` (`COMPACT_HAS_SUCCESSOR = 0x40` clear); when a newer block is later published for that account, [`BTree::update_compact_successor_32`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/btree.rs#L815-L839) appends the 32-byte `successor` in place.
     - **1-Byte Top-256 Representative Index (`rl_code = 6..7`):** Across the Nano ledger, **89.61%** of all `StateBlock`s either have `representative == account` (`0 B` stored) or delegate to one of the Top 256 Principal/Historical Representatives configured in `compact_representatives` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml), replacing the 32-byte public key with a **1-byte table index (`0..=255`)** decoded in $O(1)$ (`~1 ns`) via [`get_compact_rep_bytes`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L783-L793).
     - **Open Block, Link, Balance & Sideband Compression:** Open blocks (`previous == 0`, `~10.7%`) omit the 32-byte `previous` field; zero and epoch v1/v2 `link` values occupy `0 B`; `10^24`-scaled NANO balances and variable-length `height`/`timestamp` fields strip leading zero bytes; and [`Block::deserialize_with_hash`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L525) reuses the 32-byte `BlockHash` from the B-Tree key to skip redundant Blake2b hashing on every disk read.
2. **No Copy-on-Write (COW) or Freelist DB Bloat (~8.7 GiB saved):** LMDB never modifies pages in place; every transaction allocates new 4 KB pages for the modified leaf and every parent branch page up to the root, recording old page numbers in `FREE_DBI`. In a 220.9M-block `data.ldb` (`32,029,037` pages), only `29,760,946` pages contain active leaf data, leaving **over 2.26 million pages (~8.65 GiB)** consumed by historical COW snapshots, freelist tracking, and deeper branch trees. `barely-nano` updates slotted pages in place in its 12 GB High-DRAM buffer pool and commits atomically via a fixed `256 MB` dual-area WAL.
3. **Higher Slotted-Page Packing Density (Up to 16 Blocks/Page vs. 13 in LMDB):** Because Nano block hashes and account public keys are uniformly distributed 256-bit values, random insertions into LMDB split 4 KB leaf pages in half (`50%` occupancy after split, averaging `~68%` across the file—wasting ~1.3 KB per page). `barely-nano`'s slotted-page B-Tree packs compact block entries (`209.3 B` average value + `42 B` key/slot/SoA head = `251.3 B/block`) up to **16 blocks per 4 KB leaf page**, reclaiming dead slot bytes in place via [`leaf_compact_and_insert`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/btree.rs#L337-L370) before splitting.
4. **In-Place Page Reclamation Without Offline Compaction:** When blocks or pending entries are deleted (or pruned), LMDB's `data.ldb` file never shrinks on disk unless taken offline and rewritten with `mdb_copy -c`. In `barely-nano`, each of the 64 B-Tree shards maintains an embedded $O(1)$ free-page list (`free_list_head`) that immediately recycles emptied 4 KB pages on the fly.

## Representative & Wallet Key Security Architecture (`wallet_crypto`)

To protect representative private keys against physical NVMe SSD theft, power loss, and unauthorized extraction without sacrificing bare-metal voting throughput, `barely-nano` combines **SpacemiT SoC hardware eFuse binding** with **iterated `Blake2b-256` key derivation (`KDF`)** in [`common/src/storage/wallet_crypto.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wallet_crypto.rs) and [`ledger_store/src/wallet_store.rs`](file:///mnt/data/rust-workspace/barely-nano/ledger_store/src/wallet_store.rs):

### 1. Physical & Cryptographic Isolation
- **Strict NVMe Partition Separation:** Wallet storage uses [`WALLET_SHARD`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs) at `WALLET_STORAGE_LBA = 786,432` (`384 MB` offset) with its own 64-page redo journal. It never touches the 64 ledger [`SHARDS`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs) or the ledger WAL (`>= 512 MB`), and survives full ledger wipes (`wipe_blocks = true`).
- **No Plaintext Keys or Passwords on Disk:** Neither the wallet `seed`, any account `priv_key`, nor the wallet `password` is ever written in plaintext to the NVMe SSD.
- **Hardware-Bound Key Derivation (`SpacemiT SoC eFuse + Salt + Password`):**
  1. At boot, [`hardware_wallet_key()`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/wallet_crypto.rs#L99) derives a 256-bit SoC hardware root key from the **SpacemiT on-chip eFuse banks** (`0xD401_F000`), Chip ID Unit (`0xD428_2C00`), factory MAC registers (`0xCAC8_0040`), and U-Boot's eFuse-derived `serial#` / `ethaddr` / optional `wallet_hw_key` environment variables using trap-safe S-mode MMIO probes ([`trap::probe_read_u32`](file:///mnt/data/rust-workspace/barely-nano/common/src/trap.rs)).
  2. Each wallet generates a unique 256-bit random `salt` and derives its 256-bit wrapping key via **4,096 rounds of iterated `Blake2b-256`**:
     $$\text{WrappingKey} = \text{Blake2b-256}^{4096}\bigl(\text{SoCeFuseKey} \parallel \text{WalletSalt} \parallel \text{Password}\bigr)$$
  3. The wallet `seed` and each account `priv_key` are encrypted with domain-separated `Blake2b-256` keystream pads (`crypt_seed`, `crypt_priv_key`) and authenticated by a 256-bit `check_tag` (`compute_check_tag`). On unlock, every decrypted private key is also verified against its stored `PublicKey` (`priv_key.public_key() == pub_key`).
- **Zero Runtime Voting Overhead:** Decryption runs only once when the wallet is unlocked (at boot or on `wallet_enter`, taking `< 1.5 ms`). Once unlocked, Hart 4 ([`VoteGenerator`](file:///mnt/data/rust-workspace/barely-nano/node/src/consensus/vote_generation/vote_generator.rs)) signs votes directly from RAM with 0% cryptographic overhead.

### 2. Two Operator Modes: Unattended Auto-Voting vs. Passphrase Lock on Reboot

| Mode | Configuration | Behavior on Reboot / Power Loss | Protection if NVMe SSD is Stolen |
| :--- | :--- | :--- | :--- |
| **Mode A: Unattended Auto-Voting** *(Default)* | Default empty password (`""`) | Node automatically derives the wrapping key from the SpacemiT K3's **SoC eFuse**, unseals the representative key into RAM during `Wallets::initialize()`, runs `compute_reps()`, and **starts voting immediately with zero human input**. | **Encrypted on disk** — another machine lacks the board's internal SoC eFuse key and cannot verify `check_tag` or decrypt the keys. |
| **Mode B: Passphrase Required at Reboot** *(Optional)* | Operator sets a password via `wallet_rekey` | Node boots and syncs the ledger normally, but the wallet stays **locked with zero plaintext private keys in RAM** until the operator sends `wallet_enter` over RPC. | **Dual-factor encrypted** — requires **both** the physical SpacemiT K3 SoC eFuse **and** the operator's passphrase. |

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
# Set a passphrase (re-encrypts wallet seed & keys under SpacemiT SoC eFuse + passphrase; never stores passphrase)
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
Configure a wallet on the SpacemiT K3 Pico-ITX with the Scan Key $v$ (`seed`) and the Cold Vault's Spend Public Key $S$ (`spend_public_key`):

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
| **Single-Board Computer** | **SpacemiT K3 Pico-ITX (16 GB LPDDR5)** | 16-core hybrid RVA23 RISC-V SoC (`8× X100 @ 2.4 GHz, VLEN=256` + `8× A100 @ 2.0 GHz, VLEN=1024`; `16 GB` LPDDR5 at `0x1_0000_0000 .. 0x5_0000_0000` hosting a `3.00 GB` Heap and `12.00 GB` 64-shard B-Tree Buffer Pool) |
| **NVMe SSD** | **$\ge 128\text{ GB}$ M.2 2280 PCIe Gen3 ×4** (up to 1 TB) | Installed in the **M.2 M-Key 2280 slot** (keep the M.2 B-Key slot empty to preserve all 4 PCIe Gen3 lanes). Partition 1 (`nvme 0:1`, `LBA 2048..524,287`, first `255 MB`) holds `boot.scr`, `barely-nano.bin`, and `config.toml`; raw sectors `>= 256 MB` (`LBA 524,288+`) store the `BlockBloom` filter, isolated hardware wallet B-Tree (`384 MB`), and 64-shard B-Tree + WAL ledger (`512 MB+`) |
| **USB Type-C Cable (Fastboot / FDL)** | **USB Type-C data cable to host PC** | Used with **SpacemiT FDL / `fastboot`** to flash custom U-Boot (`u-boot.bin` / `u-boot-opensbi.itb`) to onboard SPI NOR / eMMC (the Pico-ITX board has no MicroSD slot) |
| **10GbE SFP+ / Ethernet** | **Realtek RTL8127 10GbE SFP+ Fiber (or RJ45 transceiver)** | High-throughput 10GbE P2P peering, bootstrap, JSON-RPC, WebSocket, and telemetry |
| **USB-to-TTL UART Adapter** | **3.3V Logic (`115200 8N1`)**, e.g. **DSD TECH SH-U09C5** | Connects to the debug UART0 header (`TX`, `RX`, `GND`) to monitor serial logs (`orpi.log`) |

### Optional (Recommended) Accessories

| Accessory | Specification | Purpose |
| :--- | :--- | :--- |
| **HDMI Status Display** | **Waveshare 5-Inch HDMI LCD (`800×480`, SimpleFB)** | Driven natively by Hart 15 ([`common/src/hdmi_fb.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/hdmi_fb.rs)) via U-Boot `simplefb` (`RGB565`/`XRGB8888`) to display real-time peers, blocks, confirmed height, BPS, active elections, and 12 GB Buffer Pool status |
| **Active Heatsink / Fan** | **Pico-ITX PWM Fan / Heatsink** | Keeps the 16-core SpacemiT K3 SoC, RTL8127 10GbE, and PCIe Gen3 ×4 NVMe SSD cool during continuous 24/7 operation |

#### Debug UART Wiring (DSD TECH SH-U09C5 → SpacemiT K3 Debug UART0)

> [!IMPORTANT]
> Set the jumper on the **DSD TECH SH-U09C5** (or equivalent USB-to-TTL adapter) to **3.3V** before connecting it to the SpacemiT K3 Pico-ITX. Do **not** connect the `VCC` pin if the board is powered externally.

| USB-to-TTL Adapter Pin | SpacemiT K3 Debug UART Header | Notes |
| :--- | :--- | :--- |
| **GND** | **GND** | Common ground |
| **RXD** | **TX** | Adapter RX connects to Board TX |
| **TXD** | **RX** | Adapter TX connects to Board RX |

---

## Step-by-Step Setup: Toolchain, U-Boot, Fastboot / FDL & NVMe Boot Partition

Because the **SpacemiT K3 Pico-ITX does not have a MicroSD card slot**, the bootloader (`FSBL` / OpenSBI / U-Boot) is flashed over USB using **SpacemiT FDL / `fastboot`**, while `boot.scr`, `barely-nano.bin`, and `config.toml` reside on either:
- **Option A — NVMe Boot Partition (`nvme 0:1`):** A `255 MB` FAT32/ext4 partition in the first `256 MB` of the NVMe SSD (`LBA 2048 .. 524,287`), strictly below `BLOOM_HEADER_LBA = 524,288` (`256 MB`), `WALLET_STORAGE_LBA = 786,432` (`384 MB`), and `STORAGE_BASE_LBA = 1,048,576` (`512 MB`).
- **Option B — Onboard eMMC (`mmc 0:1`) or Fastboot / TFTP RAM Staging:** Directly on the onboard eMMC boot partition (`mmc 0:1`) or staged into RAM over USB `fastboot` / Ethernet TFTP.

### 1. Install Host Prerequisites (RISC-V Tools, `fastboot`, `mkimage` & Rust Target)

On an Ubuntu/Debian x86_64 host, install the RISC-V GNU cross-compiler toolchain, `fastboot` (`android-sdk-platform-tools`), U-Boot `mkimage` utility (`u-boot-tools`), device-tree compiler, FAT32 formatting tools, and serial console monitor:

```bash
sudo apt update
sudo apt install -y \
    gcc-riscv64-linux-gnu \
    binutils-riscv64-linux-gnu \
    android-sdk-platform-tools \
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

Install the bare-metal RISC-V target and `llvm-tools-preview` component via `rustup`:

```bash
rustup target add riscv64gc-unknown-none-elf
rustup component add llvm-tools-preview
```

---

### 2. Build Custom SpacemiT K3 U-Boot ([`u-boot-pico-itx-k3/`](file:///mnt/data/rust-workspace/barely-nano/u-boot-pico-itx-k3)) & Flash via Fastboot / FDL

The custom U-Boot submodule for the SpacemiT K3 Pico-ITX (`spacemit_k3_defconfig`) is located in [`u-boot-pico-itx-k3/`](file:///mnt/data/rust-workspace/barely-nano/u-boot-pico-itx-k3) (`https://github.com/caiovini/u-boot-pico-itx-k3.git`) and includes:
- **Extended 59-Entry Standalone Jump Table (`gd->jt` at `gp + 0x20`)** (`include/_exports.h`, `common/exports.c`) — Exports Ethernet (`eth_init`..`eth_set_current` at indices `42..48`), HDMI Video (`video_get_fb`..`bmp_display` at indices `49..55`), and primary NVMe Block I/O (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at indices `56..58`).
- **A100 Cluster 2 & 3 Clock Ungating & 16-Hart DTB (`hart 0..15`)** (`drivers/clk/spacemit/clk-k3.c`, `board/spacemit/k3/board.c`, `k3.dtsi`) — Ungates `cpu_c2_core_clk`, `cpu_c3_core_clk`, `c2_tcm_pipe_clk`, and `c3_tcm_pipe_clk`, enables `sstatus.VS`, and registers all 16 harts (`8× X100` + `8× A100`) with OpenSBI HSM.
- **High-Throughput 64-Bit NVMe Driver (`NVME_Q_DEPTH = 64`, `MAX_PRP_POOL = 4096`)** (`drivers/nvme/nvme.c`) — Zero-copy 64-bit PRP DMA directly from High DRAM (`0x1_E000_0000+`) with `256 KB` (`512`-sector) transfers.
- **Realtek RTL8127 10GbE SFP+ Fiber Driver (`64 TX / 512 RX` 64-Bit DMA Rings)** (`drivers/net/rtl8169.c`) — Full 64-bit PCIe DMA descriptor addressing (`>= 0x1_0000_0000`), SFP+ SerDes calibration, batched 4-descriptor cache-line RX re-arming, and zero-`udelay` TX polling.
- **Waveshare HDMI LCD SimpleFB (`CONFIG_VIDEO_DT_SIMPLEFB=y`)** — Initializes the HDMI framebuffer before jumping to `barely-nano.bin`.

1. Build U-Boot using [`build_u-boot.sh`](file:///mnt/data/rust-workspace/barely-nano/build_u-boot.sh) (which invokes `u-boot-pico-itx-k3/build.sh`):
   ```bash
   ./build_u-boot.sh
   ```
2. Put the SpacemiT K3 Pico-ITX into **USB Download / FDL mode** (hold the **FDL / Boot** button while powering on or resetting with the USB Type-C cable connected to your host PC) and flash the bootloader stages via `fastboot`:
   ```bash
   # Stage FDL1 / FSBL and U-Boot via SpacemiT fastboot protocol:
   fastboot stage u-boot-pico-itx-k3/FSBL.bin
   fastboot continue
   # Once in U-Boot fastboot mode, flash U-Boot + OpenSBI to onboard storage (eMMC / SPI NOR):
   fastboot flash uboot u-boot-pico-itx-k3/u-boot.bin
   fastboot reboot
   ```

---

### 3. Create the `NANO_NODE` Boot Partition on NVMe (`nvme 0:1`, First 255 MB) or eMMC (`mmc 0:1`)

Because `barely-nano` starts its raw NVMe structures at `256 MB` (`BLOOM_HEADER_LBA = 524,288`), `384 MB` (`WALLET_STORAGE_LBA = 786,432`), and `512 MB` (`STORAGE_BASE_LBA = 1,048,576`), the first **`255 MB` (`LBA 2048 .. 524,287`)** of the NVMe SSD can safely host a standard MBR/GPT Partition 1 (`NANO_NODE`, `nvme 0:1`) for `boot.scr`, `barely-nano.bin`, and `config.toml` without ever overlapping the ledger or wallet partitions:

| NVMe LBA Range (512 B Sectors) | Byte Offset | Size | Purpose |
| :--- | :--- | :--- | :--- |
| **`LBA 2048 .. 524,287`** (`nvme 0:1`) | `1 MiB .. 256 MiB` | **`255 MiB`** | **FAT32 Boot Partition (`NANO_NODE`)**: holds `boot.scr`, `barely-nano.bin`, and `config.toml` |
| **`LBA 524,288 .. 655,367`** | `256 MiB .. 320 MiB` | `64 MiB + 4 KB` | Persistent `BlockBloom` filter (`BLOOM_HEADER_LBA = 524,288`) |
| **`LBA 786,432 .. 819,719`** | `384 MiB .. 400.25 MiB` | `16.25 MiB` | Isolated Hardware Wallet B-Tree + Redo Journal (`WALLET_STORAGE_LBA = 786,432`) |
| **`LBA 1,048,576` → End** | `512 MiB` → End | `~999.5 GB` | 64-Shard Ledger B-Tree + 256 MB Dual-Area WAL (`STORAGE_BASE_LBA = 1,048,576`) |

When the NVMe SSD is attached to your host workstation (e.g. via a USB-C NVMe adapter at `/dev/sdX`), create the `255 MiB` boot partition in `LBA 2048 .. 524,287` (this leaves all sectors `>= 524,288` untouched):

```bash
export NVME_DEV=/dev/sdX

# Create Partition 1 strictly within LBA 2048..524287 (522,240 sectors = 255 MiB)
sudo sfdisk "${NVME_DEV}" << 'EOF'
label: dos
start=2048, size=522240, type=0c, bootable
EOF

# Format Partition 1 as FAT32 with volume label NANO_NODE
sudo mkfs.vfat -F 32 -n NANO_NODE "${NVME_DEV}1"
```

---

### 4. Force SpacemiT K3 Pico-ITX to Probe 10GbE, NVMe & HDMI via `boot.scr`

[`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) (compiled into `boot.scr`) initializes PCIe, the Realtek RTL8127 10GbE NIC, and the M.2 NVMe SSD before loading `config.toml` at `0x1_03F0_0000` and `barely-nano.bin` at `0x1_0400_0000` from NVMe (`nvme 0:1`), onboard eMMC (`mmc 0:1`), or USB (`usb 0:1`):

```text
# Enumerate PCIe buses (Realtek RTL8127 10GbE SFP+ NIC & M.2 NVMe SSD)
pci enum
nvme scan

# Force the active Ethernet driver to initialize
ethaddr

# Load config.toml into K3 DRAM at 0x103F00000 (64 KB zeroed buffer, 1 MB below kernel _start)
# Checks NVMe Partition 1 (nvme 0:1, LBA 2048..524287 below 256 MB Bloom/Ledger), eMMC (mmc 0:1), or USB (usb 0:1)
mw.b 0x103F00000 0 0x10000
setenv config_size 0
if load nvme 0:1 0x103F00000 config.toml || load mmc 0:1 0x103F00000 config.toml || load usb 0:1 0x103F00000 config.toml; then
    setenv config_addr 0x103F00000
    setenv config_size ${filesize}
fi

if load nvme 0:1 0x104000000 barely-nano.bin || load mmc 0:1 0x104000000 barely-nano.bin || load usb 0:1 0x104000000 barely-nano.bin; then
    go 0x104000000
fi
```

At the U-Boot prompt (`=>`), configure `bootcmd` once to auto-run `boot.scr` from NVMe (or eMMC):

```text
=> setenv bootcmd "pci enum; nvme scan; if load nvme 0:1 0x138300000 boot.scr || load mmc 0:1 0x138300000 boot.scr; then source 0x138300000; fi"
=> saveenv
```

---

### 5. Build Firmware & Deploy `barely-nano.bin`, `boot.scr`, and `config.toml`

1. Edit [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) to match your local LAN subnet (`ipv4_address`, `gateway`, `dns_server`, `mac_address`).
2. Build the firmware and `boot.scr` using `make build`:
   ```bash
   make build
   ```
   - `make build` compiles [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) → `boot.scr` using `mkimage`, compiles `barely-nano` in release mode for `riscv64gc-unknown-none-elf` with RVA23 / `Zvkb` features, and extracts `barely-nano.bin` via `llvm-objcopy`.
3. **Deploy to NVMe Partition 1 (`NANO_NODE`) or via Fastboot:**
   - **Stage directly into RAM over USB Fastboot (`make fastboot-stage`):**
     At the U-Boot prompt, run `fastboot usb 0`, then from the host PC:
     ```bash
     make fastboot-stage
     ```
     And at the U-Boot prompt: `go 0x104000000` (or `tftpboot 0x104000000 barely-nano.bin; go 0x104000000` over 10GbE/GbE).
   - **Or copy to the NVMe `NANO_NODE` partition (`nvme 0:1`):**
     ```bash
     sudo cp barely-nano.bin boot.scr config.toml /media/$USER/NANO_NODE/
     sync
     sudo umount /media/$USER/NANO_NODE
     ```

---

## Build & Deployment System

- [`Makefile`](file:///mnt/data/rust-workspace/barely-nano/Makefile) is the authoritative build interface.
- Cross-compiles for `riscv64gc-unknown-none-elf` with `K3_RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicbop,+zicboz,+zicond,+zihintntl,+zihintpause,+zvbb,+zvbc,+zvkb"`.
- Compiles [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) into `boot.scr` via `mkimage` and converts the ELF into `barely-nano.bin`.

### Build & Test Targets

| Target | Feature flag | Purpose |
|---|---|---|
| `make build` / `make build-smode` | `simd` | **Production build (SpacemiT K3 Pico-ITX 16 GB LPDDR5)** — builds `barely-nano.bin` and `boot.scr` (16 harts, 64 shards, 12.00 GB High-DRAM Buffer Pool) |
| `make fastboot-stage` | `simd` | Builds `barely-nano.bin` and stages it directly to the board's DRAM over USB Fastboot (`fastboot stage barely-nano.bin`) |
| `make build-block-test` / `make build-spammer` | `block-test,simd` | **Deterministic hardware benchmark** — measures single-block and batch pipeline latency and BPS on hardware |
| `make run-tests` | — | Runs all host-side unit test suites (`crypto`, `core`, `ledger_store`, `ledger`, `messages`, `network`, `work`, `node`, `rpc_messages`, `rpc_handler`, `websocket_messages`, `websocket_client`, `websocket_server`) |
| `make run-crypto-tests` | `simd` | Runs cryptographic (`rsnano_sha`) unit tests (including 4-way and 16-way Blake2b / Ed25519 batch verification) |
| `make run-core-tests` | — | Runs `rsnano_core` unit tests |
| `make run-ledger-tests` | — | Runs `rsnano_ledger_store` and `rsnano_ledger` unit tests |
| `make run-messages-tests` | — | Runs `rsnano_messages` unit tests |
| `make run-node-tests` | — | Runs `rsnano_node` unit tests |
| `make run-rpc-messages-tests` | — | Runs `rsnano_rpc_messages` unit tests |
| `make run-rpc-handler-tests` | — | Runs `rsnano_rpc_handler` unit tests |
| `make run-websocket-messages-tests` | — | Runs `rsnano_websocket_messages` unit tests |
| `make run-websocket-client-tests` | — | Runs `rsnano_websocket_client` unit tests |
| `make run-websocket-server-tests` | — | Runs `rsnano_websocket_server` unit tests |
| `make build-lmdb-to-nvme` | — | Builds the host-side [`tools/lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme) binary (`x86_64`) for migrating a Nano `data.ldb` snapshot directly to 64-shard NVMe sectors |
| `make build-nvme-snapshot` | — | Builds the host-side [`tools/nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) binary (`x86_64`) for dumping a compact raw 64-shard ledger snapshot from a synced NVMe SSD (`dump`), inspecting it (`info`), and restoring it to an NVMe SSD (`restore`) |
| `make build-stress` | — | Builds the host-side `tools/nano-stress` binary (`x86_64`) |
| `make run-stress` / `make stress` | — | **Network stress test** — streams blocks to the node on port `7075` |
| `make run-picocom` | — | Captures serial console output from `/dev/ttyUSB0` to `orpi.log` |
| `make clean` | — | Cleans Cargo build artifacts |

## Deployment & Configuration (`config.toml`)

Node, RPC, WebSocket, and bare-metal network parameters are configured declaratively in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml), following the Nano / RSNano TOML schema.

### Live Configuration Loading (`0x1_03F0_0000` — No Recompilation Needed)
1. **U-Boot `load` (`0x1_03F0_0000`):** On boot, [`boot.cmd`](file:///mnt/data/rust-workspace/barely-nano/boot.cmd) (`boot.scr`) zeroes a 64 KB buffer at physical RAM address `0x1_03F0_0000` (1 MB below the kernel load address `0x1_0400_0000`), loads `config.toml` from `nvme 0:1` (or `mmc 0:1` / `usb 0:1`), and records `config_addr` and `config_size` in the U-Boot environment.
2. **Kernel Boot Parsing:** [`load_config_toml_str()`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs) reads `config.toml` directly from `0x1_03F0_0000` and merges it into `DaemonConfig` (`NodeConfig`, `NodeRpcConfig`, `WebsocketConfig`, and `BareMetalNetConfig`).
3. **Compile-Time Fallback:** If `config.toml` is not present in the boot partition or fails to parse, the kernel automatically falls back to the copy embedded at compile time (`include_str!("../../config.toml")`).

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
# Top 256 Principal/Historical Representatives used for 1-byte compact block encoding (`0..255`).
# Persisted into the NVMe ledger (`TABLE_VERSION = 11`, `REPTBL` in Shard 20) when the ledger is created.
compact_representatives = [
    "nano_3arg3asgtigae3xckabaaewkx3bzsh7nwz7jkmjos79ihyaxwphhm6qgjps4",
    "nano_3pnanopr3d5g7o45zh3nmdkqpaqxhhp3mw14nzr41smjz8xsrfyhtf9xac77",
    # ... (up to 256 representatives; see config.toml)
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

Standard Nano nodes store their ledger in an LMDB file (`data.ldb`) with separate named sub-databases (`accounts`, `blocks`, `confirmation_height`, `pending`, `rep_weights`, `online_weight`, `peers`, `version`), whereas `barely-nano` uses a bare-metal 64-shard Slotted-Page B-Tree (`~15 GB` per shard on a 1 TB drive, `4 KB` pages, 1-byte `table_id` key prefix) plus a `64 MB` on-disk `BlockBloom` filter.

Instead of performing random 4 KB B-Tree insertions across 220M+ records (which would cause severe SSD write amplification), [`tools/lmdb-to-nvme/src/main.rs`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) executes a **2-pass parallel streaming pipeline**:

1. **Startup Self-Test (`verify_btree_builder_self_test`):** Before touching any disk sectors, the tool constructs an in-memory multi-level B-Tree of 5,000 entries and verifies that every page passes `barely-nano`'s exact `!page_is_full` invariant and binary-search lookup semantics.
2. **Pass 1 — Parallel Sequential `data.ldb` Leaf Scan:** Memory-maps `data.ldb` (`PROT_READ`, `MAP_PRIVATE`, `MADV_SEQUENTIAL`) and scans all 4 KB LMDB pages across all CPU cores (~6 GiB/s). Valid leaf pages (`mp_flags == 0x02`, `mp_pgno == pgno`) are parsed and each record is classified by its exact `(key_len, val_len)` and inner sub-tree root page numbers into:
   - `accounts` (`TABLE_ID = 1`, 32B key $\rightarrow$ 129B value)
   - `blocks` (`TABLE_ID = 2`, 32B key $\rightarrow$ self-contained bit-packed compact `SavedBlock` value (`107..=267 B`, `COMPACT_BLOCK_TAG = 0x80`) with 1-byte `compact_representatives` table index, omitted zero `previous`/`successor`/`link`, and variable-length balance/height/timestamp matching [`SavedBlock::serialize_compact`](file:///mnt/data/rust-workspace/barely-nano/core/src/blocks/mod.rs#L862))
   - `confirmation_height` (`TABLE_ID = 3`, 32B key $\rightarrow$ 40B value)
   - `pruned` (`TABLE_ID = 5`, 32B key $\rightarrow$ 0B value, populated when `--prune` is enabled or if the source `data.ldb` contains pre-existing pruned hashes)
   - `peers` (`TABLE_ID = 6`, 18B key $\rightarrow$ 8B value)
   - `pending` (`TABLE_ID = 7`, 64B key $\rightarrow$ 49B value)
   - `online_weight` (`TABLE_ID = 9`, 8B key $\rightarrow$ 16B value)
   - `rep_weights` (`TABLE_ID = 12`, 32B key $\rightarrow$ 16B value)
3. **Optional Full-History Pruning (`--prune` / `--prune-depth` / `--prune-age`):**
   - When `--prune` is passed, right after Pass 1 the tool builds a 4,096-bucket in-memory [`ConfHeightIndex`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) in parallel across all 64 shards (~1.5s).
   - During Pass 2, each block's `(account, height, timestamp, successor)` is inspected directly from its raw LMDB bytes in $O(1)$ time via [`should_prune_block`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs).
   - The Genesis block, any account head block (`successor == 0`), any unconfirmed block (`height > confirmed_height`), and the top `--prune-depth` confirmed blocks (`height > confirmed_height - max_depth`, default `1` = confirmed frontier) are retained in `TABLE_BLOCKS (2)`.
   - All older confirmed blocks (`1 ..= confirmed_height - max_depth`) are omitted from `TABLE_BLOCKS (2)` and streamed in sorted order into `TABLE_PRUNED (5)` (`0`-byte value), reducing total NVMe B-Tree storage by **~65%** while recording exact `(account_count, block_count, confirmed_count, pruned_count)` in `CACHCNTS`.
4. **Pass 2 — Per-Shard 256-Bucket Parallel Radix Sort & Bottom-Up B-Tree Construction:**
   - Processes all 64 shards (`shard_idx = (key[1] >> 2)`), writing the active `compact_representatives` lookup table (`REPTBLC` / `REPTBLHD`, `REPTBL_MAGIC = 0x4E414E4F52455031`) into Shard `20` (`b'R' >> 2 == 20`)'s `TABLE_VERSION = 11` and running Shard `16` last (`b'C' >> 2 == 16`) so that `TABLE_VERSION = 11` receives the exact deduplicated global `CACHCNTS` (`COUNTS_MAGIC = 0x4E414E4F434E5431`: `account_count`, `block_count`, `confirmed_count`, `pruned_count`).
   - Within each `(shard, table)`, records are partitioned by their next key byte into 256 strictly ordered sub-buckets, sorted in parallel across all CPU threads, and deduplicated so only the newest LMDB Copy-On-Write record for each key survives.
   - [`StreamingBTreeBuilder`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) packs sorted records into 4 KB leaf and internal pages at an **~88% fill factor** (`PAGE_TARGET_USED = 3584` bytes, `dead_bytes = 0`, leaving headroom so runtime mutations never trigger an unsplit `page_is_full`) and flushes them in **16 MB sequential chunks** to `/dev/sda`.
5. **64 MB `BlockBloom` Filter & Global Storage Header:**
   - Every unique 32-byte block hash (both full and pruned) sets its bit positions in the 64 MB `BlockBloom` filter written to `BLOOM_DATA_LBA = 524,296` and `BLOOM_HEADER_LBA = 524,288` (`MAGIC = 0x424C4F4D56320006`), enabling $O(1)$ negative block lookups immediately on boot.
   - Zeroes both WAL area headers (`wal_base` and `wal_base + wal_pages / 2`) for every shard and writes the master `STORAGE_MAGIC = 0x5348415244534834` (`"SHARDSH4"`) header to `STORAGE_BASE_LBA = 1,048,576` (`512 MB` offset) last.
   - **Hardware Wallet Preservation:** The isolated hardware wallet region at `WALLET_STORAGE_LBA = 786,432` (`384 MB` offset) is never touched, preserving any existing encrypted representative or wallet keys on the NVMe drive.

### 2. Step-by-Step Usage (`tools/lmdb_to_nvme.sh`)

1. Place your downloaded Nano LMDB snapshot at `database/data.ldb` (or pass any path as the first argument).
2. Connect the SpacemiT K3's 1 TB NVMe SSD to your host machine via a USB-C NVMe adapter and verify its block device path (e.g. `/dev/sda`) using `lsblk`.
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
5. Ensure `wipe_blocks = false` under `[bare_metal]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml), re-insert the NVMe SSD into the SpacemiT K3 Pico-ITX, and boot the board. The node will detect `STORAGE_MAGIC` (`"SHARDSH4"`) and `BLOOM_MAGIC` (`"BLOMV2"`), load the exact block/cemented/account/pruned counts from `CACHCNTS` (automatically enabling `enable_pruning` if `pruned_count > 0`), and immediately join live consensus at the snapshot frontier.

## Direct NVMe Ledger Snapshot & Restore (`tools/nvme-snapshot`)

Once your SpacemiT K3 Pico-ITX has finished syncing the live Nano ledger on its NVMe SSD, you can attach the NVMe SSD to a Linux host over USB-C (e.g. `/dev/sda`) and use [`tools/nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) ([`tools/nvme_snapshot.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme_snapshot.sh)) to export a **compact, checksummed raw ledger snapshot file (`BNSNAPV1`)** or restore a snapshot file directly onto any target NVMe SSD in minutes.

### 1. How Sparse-Shard Snapshot Streaming & WAL Replay Works

On a 1 TB NVMe drive, the 64 B-Tree shards are spaced ~15 GB apart across the 1 TB LBA address space (`shard_size = 3,903,977` pages per shard), even though the allocated B-Tree pages (`0 .. next_free_page`) occupy only **~68–90 GiB total** for an unpruned ledger or **~24–30 GiB total** for a pruned ledger.

Instead of `dd`-ing 1,000 GB of mostly unallocated sectors, [`tools/nvme-snapshot/src/main.rs`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot/src/main.rs) operates directly on `barely-nano`'s native storage structures:

1. **Non-Destructive In-Memory WAL Replay on Dump (`collect_wal_overlay`):**
   - Opens `/dev/sda` strictly **read-only (`O_RDONLY`)** during `dump` and `info`.
   - Inspects both 128 MB WAL areas (`wal_base` and `wal_base + wal_pages / 2`) using `barely-nano`'s exact `WAL_DESC` (`0x57414C5F44455343`), `WAL_CMIT` (`0x57414C5F434D4954`), and 64-bit checksum verification (`common/src/storage/wal.rs`).
   - If the board was powered off while a committed WAL batch had not yet been checkpointed to its home shard pages, those pages are overlaid in memory while streaming the snapshot—guaranteeing a **100% transactionally consistent snapshot** without modifying a single sector on the source NVMe.
2. **Sparse-Shard 16 MB Sequential Streaming (`BNSNAPV1` Format):**
   - Reads the 4 KB `STORAGE_MAGIC` (`"SHARDSH4"`) header at `STORAGE_BASE_LBA = 1,048,576` (`512 MB` mark), extracts `CACHCNTS` (`account_count`, `block_count`, `confirmed_count`, `pruned_count`) from Shard 16's B-Tree, and reads each shard's `MetaData` page (`0xBEEFCB03`).
   - Streams only the allocated pages `0 .. next_free_page` from each of the 64 shards (plus the `64 MB` `BlockBloom` filter at `BLOOM_HEADER_LBA = 524,288`) in **16 MB sequential chunks**, packing them contiguously into the output `.snap` file.
3. **End-to-End 64-Bit Integrity Checksums:**
   - Computes a 64-bit checksum over the 64 MB `BlockBloom` region, each of the 64 B-Tree shard payloads, the optional 16.25 MB wallet partition, and the 4 KB `BNSNAPV1` file header itself.
   - During `restore`, every region's checksum and `BTREE_META_MAGIC` (`0xBEEFCB03`) are verified before committing `STORAGE_MAGIC` (`"SHARDSH4"`) to the target NVMe.
4. **Cross-Drive Capacity Portability on Restore:**
   - When restoring onto a different-sized NVMe SSD (e.g. restoring a snapshot taken from a 1 TB NVMe onto a 256 GB or 512 GB NVMe), `restore` queries the target drive's sector count via `BLKGETSIZE64`, computes the target drive's `shard_size`, `wal_base`, and `wal_pages`, verifies that every shard's `next_free_page` fits within the target `shard_size`, and writes each shard `0..64` to its proper target `base_page = 1 + i * (target_shard_size + 1)`.
5. **Hardware Wallet Isolation (`--include-wallets` Optional):**
   - By default, both `dump` and `restore` **skip** the isolated hardware wallet partition at `WALLET_STORAGE_LBA = 786,432` (`384 MB` mark), so restoring a ledger snapshot onto a node never overwrites its eFuse-encrypted representative or wallet keys. Pass `--include-wallets` to include or restore the 16.25 MB wallet B-Tree + redo journal when cloning an entire node onto the same board.

### 2. `BNSNAPV1` Raw Snapshot File Layout

Every `.snap` file generated by `nvme-snapshot dump` uses a deterministic, page-aligned (`4 KB`) binary container (`SNAP_MAGIC = 0x3156_5041_4E53_4E42`, `"BNSNAPV1"`):

| File Offset | Size | Region | Description |
| :--- | :--- | :--- | :--- |
| `0x0000_0000` | `4 KB` (`1` page) | **`BNSNAPV1` Header** | Magic (`"BNSNAPV1"`), flags (`FLAG_HAS_BLOOM`, `FLAG_HAS_WALLETS`, `FLAG_PRUNED`), timestamp, source `shard_size` & `wal_epoch`, cached ledger counts (`accounts`, `blocks`, `confirmed`, `pruned`), 64-bit region checksums, and 64 × 32-byte [`ShardDescriptor`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot/src/main.rs) entries (`root_page_id`, `next_free_page`, `first_free_page`, `file_offset_bytes`, `checksum`) |
| `0x0000_1000` | `64 MB + 4 KB` (`16,385` pages) | **`BlockBloom` Filter** | Raw copy of `BLOOM_HEADER_LBA` (`524,288`) + `BLOOM_DATA_LBA` (`524,296`, `64 MB` bitset) for instant $O(1)$ negative block lookups on boot |
| *(Optional)* | `16.25 MB` (`4,161` pages) | **Wallet Partition** | Present only when `--include-wallets` is passed; holds `WALLET_STORAGE_LBA` (`786,432`, `4,096` B-Tree pages + `1` header + `64` journal pages) |
| Contiguous | `next_free_page × 4 KB` per shard | **Shards `0..63` B-Tree Pages** | Only the allocated pages (`0 .. next_free_page`) of each of the 64 B-Tree shards (~68–90 GiB total unpruned / ~24–30 GiB pruned), with any committed WAL pages already overlaid in place |

### 3. Step-by-Step Usage (`tools/nvme_snapshot.sh`)

1. Power off the SpacemiT K3 Pico-ITX, move the NVMe SSD into a USB-C NVMe enclosure on your Linux host, and check its device path with `lsblk` (e.g. `/dev/sda`).
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

   # Optional: Also restore the wallet partition (only if restoring to the same board)
   ./tools/nvme_snapshot.sh restore database/nano_ledger.snap /dev/sda --include-wallets
   ```
6. Re-insert the NVMe SSD into the SpacemiT K3 Pico-ITX (with `wipe_blocks = false` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml)) and boot. The node will immediately mount all 64 shards and the 64 MB `BlockBloom` filter with zero WAL recovery needed.

## Network Stress Testing (`tools/nano-stress`)

The repository provides a high-throughput stress testing tool (`tools/nano-stress`) to test node stability, B-Tree storage performance, and queue behavior under heavy load by generating cryptographically valid blocks and streaming them directly over TCP to port **7075**.

### 1. Node Dev Network Setup

To prepare the node for stress testing, set the following in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) in the `NANO_NODE` boot partition:

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
- [`spacemit_k3_migration.md`](file:///mnt/data/rust-workspace/barely-nano/spacemit_k3_migration.md) — SpacemiT K3 Pico-ITX (`16 GB LPDDR5`) 16-hart, 64-shard bare-metal architecture reference.
- [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) — Declarative TOML configuration for bare-metal network, node, bootstrap, queues, RPC, and WebSocket server.
- [`main/`](file:///mnt/data/rust-workspace/barely-nano/main) — Main bare-metal binary entry point (`entry.S`, `bin.rs`) and 16-hart SMP loops.
- [`common/`](file:///mnt/data/rust-workspace/barely-nano/common) — Core kernel library: S-mode trap handler, UART/SBI/U-Boot wrappers, Waveshare 5-inch HDMI SimpleFB driver (`hdmi_fb.rs`), 64-shard B-Tree + 12 GB High-DRAM BPM + atomic WAL storage engine, cooperative async executor, and `smoltcp` networking stack.
- [`core/`](file:///mnt/data/rust-workspace/barely-nano/core) — Core Nano primitives (blocks, accounts, keys, amounts, votes, difficulty).
- [`messages/`](file:///mnt/data/rust-workspace/barely-nano/messages) — Nano P2P wire protocol messages and serialization.
- [`ledger/`](file:///mnt/data/rust-workspace/barely-nano/ledger) & [`ledger_store/`](file:///mnt/data/rust-workspace/barely-nano/ledger_store) — Ledger state management, 3-stripe parallel block insertion/validation, and 64-shard B-Tree store tables.
- [`network/`](file:///mnt/data/rust-workspace/barely-nano/network) & [`network_protocol/`](file:///mnt/data/rust-workspace/barely-nano/network_protocol) — P2P channel management, handshakes, and message framing.
- [`node/`](file:///mnt/data/rust-workspace/barely-nano/node) — High-level Nano node (consensus, active elections, vote processor, bootstrapper, cementation, telemetry, TOML config).
- [`rpc_messages/`](file:///mnt/data/rust-workspace/barely-nano/rpc_messages), [`rpc_handler/`](file:///mnt/data/rust-workspace/barely-nano/rpc_handler), [`rpc_server/`](file:///mnt/data/rust-workspace/barely-nano/rpc_server) — Bare-metal `no_std` JSON-RPC message types, command handlers, and HTTP server.
- [`websocket_messages/`](file:///mnt/data/rust-workspace/barely-nano/websocket_messages), [`websocket_client/`](file:///mnt/data/rust-workspace/barely-nano/websocket_client), [`websocket_server/`](file:///mnt/data/rust-workspace/barely-nano/websocket_server) — Bare-metal `no_std` WebSocket envelopes, client, and real-time event broadcast server.
- [`nullables/`](file:///mnt/data/rust-workspace/barely-nano/nullables) — Hardware/OS abstraction crates (`sha` RVA23 / `Zvkb` `VLEN=256` & `VLEN=1024` crypto, `clock`, `random`, `tcp`, `http_client`, `output_tracker`) with nullable test doubles.
- [`stats/`](file:///mnt/data/rust-workspace/barely-nano/stats) & [`work/`](file:///mnt/data/rust-workspace/barely-nano/work) — Node statistics collector and PoW work generation/validation.
- [`tools/`](file:///mnt/data/rust-workspace/barely-nano/tools) — Host-side utilities: [`lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme) & [`lmdb_to_nvme.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb_to_nvme.sh) (direct `data.ldb` to raw 64-shard NVMe B-Tree + Bloom migration tool), [`nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot) & [`nvme_snapshot.sh`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme_snapshot.sh) (direct NVMe to compact raw 64-shard ledger snapshot dump & restore tool), [`nano-stress`](file:///mnt/data/rust-workspace/barely-nano/tools/nano-stress) (P2P block streamer), and [`ws_subscribe.py`](file:///mnt/data/rust-workspace/barely-nano/tools/ws_subscribe.py) (WebSocket topic subscriber).
- [`u-boot-pico-itx-k3/`](file:///mnt/data/rust-workspace/barely-nano/u-boot-pico-itx-k3) — Custom SpacemiT K3 Pico-ITX (`spacemit_k3_defconfig`) U-Boot git submodule (`https://github.com/caiovini/u-boot-pico-itx-k3.git`) with 59-entry Standalone Jump Table (`eth_*`, `video_*`, `blk_*`), RTL8127 10GbE SFP+ driver, 64-bit NVMe PRP DMA, and A100 Cluster 2/3 clock ungating.
- [`linker-smode.ld`](file:///mnt/data/rust-workspace/barely-nano/linker-smode.ld) — S-mode linker script mapping the `0x1_0400_0000` physical memory layout for the SpacemiT K3 Pico-ITX (`16 GB LPDDR5`).

## Authors & Credits

`barely-nano` is a bare-metal `no_std` RISC-V fork and hardware port of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node).

- **Caio Vinicius** (`<cvini@proton.me>`) — Bare-metal `no_std` RISC-V architecture, SpacemiT K3 / K1 SMP kernel, RVA23 / RVV 1.0 / `Zvkb` hardware-accelerated cryptography, 64-shard NVMe B-Tree + 12 GB High-DRAM Buffer Pool + WAL storage engine, U-Boot `smoltcp` networking, and embedded RPC/WebSocket/PoS stack.
- **Gustav Schauwecker** (`<service@simpago.com>`) — Creator and lead author of the original [RsNano](https://github.com/rsnano-node/rsnano-node) Rust implementation of the Nano node.
- **Gemini 4 Carbon** — Co-author on the `no_std` migration, storage engine, and RISC-V systems engineering.

