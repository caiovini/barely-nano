# Bare-Metal RISC-V Nano Node (barely-nano)

This project is a bare-metal `no_std` RISC-V fork of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node), designed to run a full Nano cryptocurrency node directly on the **SpacemiT K3 Pico-ITX** with 16 RISC-V harts (`8× X100 @ 2.4 GHz, VLEN=256` + `8× A100 @ 2.0 GHz, VLEN=1024`), **16 GB LPDDR5 RAM**, Realtek RTL8127 10GbE SFP+, Waveshare 5-inch HDMI SimpleFB display, and a 1 TB NVMe SSD.

It establishes a self-contained operating environment on top of OpenSBI and U-Boot (`CONFIG_API=y`), complete with a 3 GB heap allocator, 16-hart exception handling, RVA23/`Zvkb` (`VLEN=256` & `VLEN=1024`) hardware-accelerated cryptography, a 64-shard B-Tree + 12 GB High-DRAM Buffer Pool + atomic Write-Ahead Log (WAL) storage engine on NVMe, a full TCP/IP networking stack, an HTTP JSON-RPC server, and a real-time WebSocket server.

## Table of Contents

- [Features Implemented](#features-implemented)
  - [1. S-Mode Execution on OpenSBI & U-Boot](#1-s-mode-execution-on-opensbi--u-boot)
  - [2. Memory Management](#2-memory-management)
  - [3. Exception and Trap Handling](#3-exception-and-trap-handling)
  - [4. Hardware Watchdogs, Timers & Peer-Calibrated Wall Clock](#4-hardware-watchdogs-timers--peer-calibrated-wall-clock)
  - [5. Hardware Accelerated Cryptography & Optimizations](#5-hardware-accelerated-cryptography--optimizations)
  - [6. U-Boot Networking, RPC, Nault Wallet Compatibility & WebSocket Stack](#6-u-boot-networking-rpc-nault-wallet-compatibility--websocket-stack)
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
- [Step-by-Step Setup: Toolchain, U-Boot, SPI NOR (`mtd6`) & NVMe Boot Partition](#step-by-step-setup-toolchain-u-boot-spi-nor-mtd6--nvme-boot-partition)
  - [1. Install Host Prerequisites (RISC-V Tools, `fastboot`, `mkimage` & Rust Target)](#1-install-host-prerequisites-risc-v-tools-fastboot-mkimage--rust-target)
  - [2. Build Custom SpacemiT K3 U-Boot (`u-boot-pico-itx-k3/`) & Prepare NVMe (`./build_u-boot.sh /dev/sdX --flash`)](#2-build-custom-spacemit-k3-u-boot-u-boot-pico-itx-k3--prepare-nvme-build_u-bootsh-devsdx---flash)
  - [3. (Optional) Migrate the Nano Ledger to NVMe While Connected to the Host PC (`./tools/lmdb_to_nvme.sh`)](#3-optional-migrate-the-nano-ledger-to-nvme-while-connected-to-the-host-pc-toolslmdb_to_nvmesh)
  - [4. Flash Custom `u-boot.itb` to SPI NOR (`mtd6` @ `0x210000`)](#4-flash-custom-u-bootitb-to-spi-nor-mtd6--0x210000)
  - [5. How `boot.scr` Probes PCIe, NVMe & USB and Boots `barely-nano.bin`](#5-how-bootscr-probes-pcie-nvme--usb-and-boots-barely-nanobin)
  - [6. Updating `barely-nano.bin`, `boot.scr`, or `config.toml` Without Reflashing SPI NOR](#6-updating-barely-nanobin-bootscr-or-configtoml-without-reflashing-spi-nor)
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
The kernel runs in **Supervisor Mode (S-mode)** launched from U-Boot on top of OpenSBI firmware, utilizing SBI `ecall`s for multi-hart management (HSM) and IPIs, and U-Boot's Standalone Jump Table (`gd->jt` at `gp + 0x20`) for GbE/10GbE Ethernet, HDMI SimpleFB, and NVMe block device I/O.
- **Boot & Firmware Delivery (Onboard 8 MB SPI NOR `/dev/mtd0..mtd6` + NVMe Partition 1 `nvme 0:1`):** The SpacemiT K3 Pico-ITX boots from an **8 MB onboard SPI NOR flash (`d420c000.spi`, `/dev/mtd0..mtd6`)**: `mtd1` (`bootinfo`), `mtd2` (`fsbl`), `mtd3` (`env` @ `0xA0000`), `mtd4` (`esos`), `mtd5` (`opensbi`), and `mtd6` (`uboot` @ `0x210000`, loading our custom `u-boot.itb` FIT image at `0x1_0200_0000` with `conf_11` / `fdt_11` `k3-pico-itx` DTB and relocating U-Boot to the top of 16 GB DRAM at `~0x4_FEED_xxxx`). Once U-Boot initializes PCIe Gen3 ×4 (`pcie@80000000` `PCIE-0` bonding dual 2-lane PHYs `phy@81d00000` + `phy@81e00000`) and USB, it automatically loads `boot.scr`, `config.toml`, and `barely-nano.bin` from a USB drive (`usb 0:1`) or the `255 MiB` FAT32 `NANO_NODE` boot partition at the start of the NVMe SSD (`nvme 0:1`, `LBA 2048..524,287` — strictly below `BLOOM_HEADER_LBA = 524,288`).
- **Ledger Media (1 TB NVMe SSD):** Automatically discovered via U-Boot's primary NVMe namespace (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at Jump Table indices `56..58`), with raw ledger structures starting at `256 MiB` (`BLOOM_HEADER_LBA = 524,288`), `384 MiB` (`WALLET_STORAGE_LBA = 786,432`), and `512 MiB` (`STORAGE_BASE_LBA = 1,048,576`).

### 2. Memory Management
- **Boot sequence:** Written in custom assembly (`main/src/entry.S`) to set up stack pointers for all 16 harts, preserve U-Boot's `gp` (`uboot_gp`) and Jump Table pointer (`gd->jt`), zero BSS, flush instruction/data caches across `_start .. _bss_end`, and jump into Rust at `0x1_0400_0000`.
- **Heap Allocator (`0x1_1800_0000 .. 0x1_D800_0000`):** Uses `linked_list_allocator` wrapped in `InterruptSafeAllocator` to provide a global dynamic memory allocator (`ALLOCATOR`) configured in `linker-smode.ld` and [`common/src/soc.rs`](common/src/soc.rs) with a **3.00 GB heap** and **256 MB stack** (16 MB per hart across 16 harts).
- **High DRAM Sector Cache (`0x1_E000_0000 .. 0x4_F000_0000`):** Directly utilizes the SpacemiT K3 16 GB dual-channel LPDDR5 memory window (`2 × MT62F2G32D4DS` across `ddrc0` @ `0xcb000000` + `ddrc1` @ `0xcc000000`, `0x1_0000_0000 .. 0x5_0000_0000`) to host a **12.00 GB B-Tree Buffer Pool** (`49,152` target / `50,176` max × 4 KB frames per shard across 64 shards, leaving the top `256 MB` `0x4_F000_0000 .. 0x5_0000_0000` untouched for relocated U-Boot). *(Note: SPL logs per-channel density `MT62F2G32D4DS, Size: 8192MB`; `fsbl` shifts this left across both 32-bit channels for `16,384 MB` total.)*

### 3. Exception and Trap Handling
- **Trap Vector:** Custom S-mode assembly trap vector (`trap_vector`) saves and restores all 31 GPRs upon interrupts and exceptions and enables `sstatus.FS` and `sstatus.VS` for floating-point and RVV 1.0 vector instructions across all 16 harts (`VLEN=256` on X100 Harts `0..7` and `VLEN=1024` on A100 Harts `8..15`).
- **Trap Dispatcher:** Evaluates the `scause` CSR to differentiate between asynchronous interrupts and synchronous exceptions, with atomic probe recovery (`trap::probe_read_u32` / `trap::probe_rw_u64`) safe across 16 concurrent harts.
- **Diagnostics:** Exceptions automatically print the hart ID, `scause`, `sepc`, `stval`, and `ra` registers over SpacemiT K3 UART0 (`0xD401_7000`).

### 4. Hardware Watchdogs, Timers & Peer-Calibrated Wall Clock
- **CLINT Timer:** Reads the 24 MHz hardware `time` CSR (`rdtime`) for cooperative async scheduling, sub-microsecond profiling, and monotonic `SteadyClock` durations.
- **Peer-Telemetry-Calibrated Unix Wall Clock ([`core/src/utils/mod.rs`](core/src/utils/mod.rs), [`node/src/telemetry/mod.rs`](node/src/telemetry/mod.rs)):** Combines monotonic `SteadyClock` elapsed time with an atomic boot-epoch offset (`WALL_CLOCK_OFFSET_MS`) calibrated automatically from the median of up to 32 Ed25519-verified peer `TelemetryAck` timestamps—providing accurate Unix epoch timestamps (`UnixTimestamp::now()`, `UnixMillisTimestamp::now()`) for block sidebands, account modification times, outgoing `TelemetryAck` packets, WebSocket envelopes, and vote generation without an external RTC or NTP daemon.
- **Watchdog Management:** Disables the SpacemiT P1 PMIC watchdog over PWR I2C bus 8 (`0xD401_D800`) at boot while leaving the APB clock-gated K3 SoC watchdog (`watchdog@d4014000`, [`common/src/watchdog.rs`](common/src/watchdog.rs)) quiesced so unclocked APB reads never stall `PREADY`.

### 5. Hardware Accelerated Cryptography & Optimizations
To achieve maximum transaction throughput across the hybrid 16-core silicon, the node compiles with the SpacemiT K3 RVA23 extension set (`+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zicond,+zihintpause,+zvbb,+zvbc,+zvkb`):
- **Vector Crypto Bit-Manipulation (`Zvkb`, `Zvbb`, `Zvbc`):** Single-instruction native vector rotates (`vror.vi v, v, 32/24/16/63`) in [`nullables/sha/src/blake2b.rs`](nullables/sha/src/blake2b.rs), replacing multi-instruction shift-and-XOR sequences across every Blake2b G-mixing round.
- **Hybrid `VLEN=256` (4-Way) & `VLEN=1024` (16-Way) RVV 1.0 Batching:**
  - **X100 Harts `0..7` (`VLEN=256`):** Execute zero-spill `LMUL=1` 4-way Blake2b (`compress_four_blocks`, `hash_batch_4`, `hash64_batch_4`) and 4-way Ed25519 signature verification (`verify_batch_4`).
  - **A100 Harts `8..14` (`VLEN=1024`):** Runtime `csrr vlenb` (`>= 128`) detection in [`has_vlen_1024()`](nullables/sha/src/blake2b.rs) and [`SignatureChecker::help_verify()`](node/src/signatures.rs) automatically dispatches **16-way lockstep SIMD** Blake2b (`compress_16_blocks`, `hash_batch_16`, `hash64_batch_16`) and 16-way batch Montgomery inversion Ed25519 verification (`verify_batch_16`), verifying 16 blocks per call on each of the 7 A100 crypto harts.
- **Scalar Bit-Manipulation & Conditional Ops (`Zba`, `Zbb`, `Zbs`, `Zbc`, `Zicond`):** Single-cycle shifted address generation (`sh1add`, `sh2add`, `sh3add`), branchless conditional moves (`czero.eqz`/`czero.nez`), and scalar `rori` rotations.
- **B-Tree Prefix Search & `Zicbop` Prefetch:** Branchless 8-byte big-endian `u64` prefix comparisons ([`page_lower_bound`](common/src/storage/btree.rs#L312-L378)) plus architectural `prefetch.r` / `prefetch.w` helpers ([`common/src/storage/mod.rs`](common/src/storage/mod.rs#L31-L65)) during B-Tree binary search, child page descent, and leaf mutation.
- **Cache-Block Zero & Management (`Zicboz`, `Zicbom`, `Zihintpause`):** Fast 64-byte cache-line zeroing (`cbo.zero`), explicit cross-cluster cache maintenance (`cbo.clean`, `cbo.inval`), and adaptive `pause` spinloop backoff.

### 6. U-Boot Networking, RPC, Nault Wallet Compatibility & WebSocket Stack
- **Realtek RTL8127ATF 10GbE SFP+ (`eth1: eth_rtl8169`) & Onboard Gigabit (`eth0: ethernet@cac80000`) Networking:** Drives the onboard Realtek RTL8127ATF 10GbE SFP+ controller (`pcie@80800000` `PCIE-2` Gen3 ×2, 64-bit PCIe DMA, `rev 0x08` MAC MCU patch, 10GBASE-R SerDes, 1024 TX / 512 RX descriptors with cache-line-aligned batched RX re-arming) as well as the onboard RGMII Gigabit Ethernet controller (`dwc_eth_qos_spacemit`) via the U-Boot Standalone Jump Table API (`eth_init`, `eth_send`, `eth_rx`, `eth_receive` at indices `42..48`).
- **TCP/IP Stack (`smoltcp` 0.13):** Full-featured `no_std` TCP/IP/DNS/ICMP stack integrated with the cooperative async executor, non-blocking Hart 1 maintenance polling, and automatic listener recovery.
- **Nano Services:**
  - **Port `7075` (Peering):** Live Nano P2P protocol peering and bootstrap (`96` concurrent peers).
  - **Port `7076` (HTTP JSON-RPC — Nault / Natrium Compatible):** 8-socket concurrent HTTP JSON-RPC pool (`RPC_SOCKET_COUNT = 8`) backed by `rsnano_rpc_messages`, `rsnano_rpc_handler`, and `rsnano_rpc_server`, including legacy wallet action/field aliases (`accounts_pending`, `pending`, `pending_exists`, `search_pending`, `wallet_pending`), both stringified and object `process` blocks, standard `json_block` string-vs-object `contents` serialization, and browser CORS preflight support (`OPTIONS`). To connect **Nault**, configure Custom Server API `http://192.168.1.200:7076`, WebSocket `ws://192.168.1.200:7078`, and set PoW Source to **Client WebGL / Client CPU** (or an external PoW server).
  - **Port `7078` (WebSocket Server):** Full `no_std` WebSocket server and client (`rsnano_websocket_messages`, `rsnano_websocket_client`, `rsnano_websocket_server`) streaming live node events (`confirmation`, `vote`, `started_election`, `stopped_election`, `telemetry`, `bootstrap`, `new_unconfirmed_block`, and `ping`/`pong` keepalives).
  - **Port `7080` (Live Web Dashboard, Browser WASM Stress Test & Diagnostic Endpoints):** Built-in 6-socket HTTP server ([`main/src/dashboard.html`](main/src/dashboard.html), [`main/src/bin.rs`](main/src/bin.rs)) serving:
    - Real-time web dashboard UI, embedded wallet, Point-of-Sale (PoS) terminal, and browser-side WebAssembly stress test (`nano_stress.wasm` + `POST /api/stress_batch`).
    - `GET /api/metrics`: Real-time JSON telemetry including per-hart Buffer Pool misses (`miss_n_hart`, `miss_ms_hart`), shard lock hold times (`shard_hold_ms_hart`), NVMe device I/O counters (`io_read_*`, `io_write_*`), and ledger recount progress (`recount`).
    - `GET /api/recount_start` & `GET /api/recount_repair`: Incremental chunked merge-join verification and self-healing of `TABLE_ACCOUNTS` and `TABLE_CONFIRMATION_HEIGHT` ([`ledger/src/ledger_recount.rs`](ledger/src/ledger_recount.rs)).

### 7. 16-Hart Hybrid SMP Architecture (`8× X100` + `8× A100`)
All 16 harts on the SpacemiT K3 are awakened via OpenSBI HSM (`sbi::hart_start`) and pinned by physical CPU cluster in [`main/src/bin.rs`](main/src/bin.rs):

- **Cluster 0 — X100 Harts `0..=3` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb` — Ingress, Network, Deserialization & Unchecked/Bootstrap):**
  - **Hart 0 (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 1 (`Maint+Net`):** Boot initialization, GbE/10GbE `smoltcp` network polling, HTTP JSON-RPC (`7076`), WebSocket (`7078`), Live Web Dashboard (`7080`), gateway ICMP keepalive, and offline link recovery.
  - **Hart 2 (`MsgProc`):** Dedicated inbound network message deserialization (`MessageProcessor::run_batch()`) and 4-way `VLEN=256` (`Zvkb`) Ed25519 signature verification assist (`signature_checker.help_verify()`).
  - **Hart 3 (`Unchecked+Boot`):** Dedicated `unchecked` dependency queue drainer and `bootstrap_server` response worker.
- **Cluster 1 — X100 Harts `4..=7` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb` — Consensus & 3-Stripe Parallel 64-Shard Ledger Storage):**
  - **Hart 4 (`Consensus`):** Drives `aec_voter` and `aec_ticker` (300 ms ticks), `election_schedulers`, `backlog_scan`, `bootstrap_server`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 5 (`Ledger-B`):** Parallel **Shards `21..=41`** (`[21, 42)`, 21 shards) B-Tree mutations ([`Ledger::try_run_ledger_b_job`](ledger/src/ledger.rs)), WAL segments, and `unchecked` dependency queue drain.
  - **Hart 6 (`Ledger-A`):** Primary `block_processor` batch coordinator (`FAST_BATCH_SLOTS = 2048`), parallel **Shards `0..=20`** (`[0, 21)`, 21 shards) B-Tree mutations, WAL checkpoint coordinator, and `confirming_set` cementation.
  - **Hart 7 (`Ledger-C + Storage`):** Parallel **Shards `42..=63`** (`[42, 64)`, 22 shards) B-Tree mutations ([`Ledger::try_run_ledger_c_job`](ledger/src/ledger.rs)), online weight calculation, NVMe Free-List GC/reclamation (`ledger_pruning`), and cooperative async executor tasks (`WORKER_HARTS`).
- **Clusters 2 & 3 — A100 Harts `8..=15` (`2.0 GHz`, `VLEN=1024`, `Zvkb` — 16-Way Wide-Vector Crypto & HDMI Telemetry):**
  - **Harts `8..=14` (`Crypto-A100`):** 7 dedicated **16-way `VLEN=1024` SIMD** Ed25519 signature verification (`verify_batch_16`) + Blake2b (`compress_16_blocks`) workers and inbound `MessageProcessor::run_batch()` drainers.
  - **Hart `15` (`HDMI+Mon`):** Drives the 2-second Waveshare 5-inch HDMI SimpleFB graphical dashboard ([`common/src/hdmi_fb.rs`](common/src/hdmi_fb.rs)) and 15-second hardware telemetry/lock-contention reports.

### 8. Waveshare 5-Inch HDMI SimpleFB Display (`common/src/hdmi_fb.rs`)
- **Zero-Allocation `no_std` Framebuffer Driver:** Automatically discovers the `simple-framebuffer` (`simplefb`) region initialized by U-Boot's HDMI driver from the Flattened Device Tree (`0x1_1F00_0000`) or U-Boot Jump Table (`video_get_fb`, `video_get_xsize`, `video_get_ysize`, `video_sync_all` at indices `49..55`), supporting both `r5g6b5` (`RGB565`, 16 bpp) and `x8r8g8b8` / `a8r8g8b8` (`XRGB8888`, 32 bpp) panels (`800×480` Waveshare 5-inch HDMI LCD).
- **Live Graphical Dashboard on Hart 15:** Renders node header, connected peer count, cemented/total block counts, live BPS rate, active elections (`AEC`), 64-shard NVMe DB footprint, and 12 GB High-DRAM Buffer Pool utilization every 2 seconds on Hart 15, flushing dirty cache lines via `cbo.clean` + `video_sync_all`.

## Storage Architecture: Bare-Metal 64-Shard NVMe B-Tree + 12 GB Buffer Pool + WAL Engine

Without an OS filesystem or traditional database (LMDB/RocksDB), `barely-nano` implements a crash-resilient **64-Shard B-Tree + 12 GB High-DRAM Buffer Pool + Atomic Write-Ahead Log (WAL)** directly on the 1 TB PCIe Gen3 ×4 NVMe SSD:

1. **Physical Layer (`StorageBackend::UBoot`):** Transfers raw 4 KB pages (`8 × 512 B` sectors, batched up to 64 pages / 256 KB per write command via zero-copy 64-bit NVMe PRP DMA, and up to 16 contiguous pages / 64 KB per B-Tree range-scan read via `StorageContext::read_contiguous_into_frames`) over U-Boot's Jump Table block interface (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at indices `56..58`, `NVME_Q_DEPTH = 64`). (`STORAGE_BASE_LBA = 1,048,576` leaves a 512 MB safety margin at the start of the drive, with a 64 MB persistent Bloom filter at `BLOOM_HEADER_LBA = 524,288`.)
2. **12 GB High-DRAM Buffer Pool Manager ([`common/src/storage/bpm.rs`](common/src/storage/bpm.rs)):** Manages `49,152` × 4 KB frames per shard (`192 MB/shard × 64 shards = 12.00 GB` target, `50,176` frames = `12.25 GB` max) in SpacemiT K3 LPDDR5 (`0x1_E000_0000 .. 0x4_F000_0000`), paired with compact 24-byte `FrameMeta` entries and a two-level chunked page directory (`page_to_frame`) in the 3.00 GB Heap (`0x1_1800_0000 .. 0x1_D800_0000`), and a **Table-ID-Aware Clock (second-chance)** eviction policy:
   - Priority **`4`**: Shard metadata (`page_id == 0`) and internal B-Tree routing nodes (`is_leaf == 0`).
   - Priority **`2`**: Hot point-lookup leaf pages (`Account (1)`, `ConfirmationHeight (3)`, `Pending (7)`, `Version (11)`, `RepWeight (12)`).
   - Priority **`1`**: Cold `Block/Successor (2)` leaf pages.
   - Priority **`0` (`fetch_page_scan` / `prefetch_contiguous_scan`)**: Sequentially scanned leaf pages during full-table range scans (`FrontierChecker`, `BacklogScan`, `LEDGER_RECOUNT`), ensuring background scans are reclaimed immediately on the Clock hand's first pass without evicting hot point-lookup pages used by Hart 6 (`Ledger-A`).
   - Dirty frames are **never written during eviction**, preventing partial cross-shard updates from reaching disk.
3. **Asynchronous Dual-Area Write-Ahead Log ([`common/src/storage/wal.rs`](common/src/storage/wal.rs)):** Reserves `256 MB` (`2 × 128 MB` alternating areas) on NVMe. When a checkpoint is triggered (`wal::checkpoint_nonblocking()`), dirty pages across all 64 shards are snapshotted in parallel into staging buffers in RAM while **Hart 7 (`Ledger-C + WAL`)** asynchronously writes `[descriptor pages][data pages][commit page]` with a 64-bit checksum and boot epoch, followed by zero-copy contiguous LBA home-page runs (`apply_sorted_home_runs`). On boot, `wal::recover()` idempotently replays the latest committed batch.
4. **64-Shard Slotted-Page B-Tree Engine ([`common/src/storage/btree.rs`](common/src/storage/btree.rs)):** Prefix-partitioned across 64 shards by the top 6 bits of `key[1]` (`(key[1] >> 2) as usize`, striped across Hart 6 Shards `0..=20`, Hart 5 Shards `21..=41`, and Hart 7 Shards `42..=63`), featuring RVV 1.0 (`+v,+zvl256b`) 8-wide Structure-of-Arrays (SoA) 32-bit key-head vector searches, direct-mapped `HOT_ACCOUNTS` / `HOT_CONF_HEIGHT` / `HOT_BLOCKS` caches and an exact in-RAM `UNCONFIRMED_ACCOUNTS` index ([`ledger_store/src/account_store.rs`](ledger_store/src/account_store.rs), [`ledger_store/src/confirmation_height_store.rs`](ledger_store/src/confirmation_height_store.rs), [`ledger_store/src/block_store.rs`](ledger_store/src/block_store.rs)), branchless 64-bit prefix binary searches, 16-page contiguous sibling read-ahead (`BTree::prefetch_child_run`), proactive-split inserts, sibling borrow/merge deletes with embedded free-list page reclamation, and zero-allocation range iterators (`next_kv_with`).
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
| **CPU Scheduling & Execution Model** | Linux CFS thread migration, syscalls, interrupts, and multi-thread DB lock contention | **Hard-pinned 16-Hart Hybrid SMP** (X100 `MsgProc` + `Unchecked/Boot` + 7× A100 16-way `VLEN=1024` Crypto harts, 3-Stripe Parallel 64-Shard Ledger harts on Cluster 1) | Zero context-switch or syscall overhead; 16-way `VLEN=1024` SIMD + 3-stripe 64-shard B-Tree parallelism |

#### Why the Storage Size Difference Occurs
1. **Self-Contained Bit-Packed Compact Block Encoding & Top-256 Representative Table (~22 GiB saved):**
   - In [`SavedBlock::serialize_compact`](core/src/blocks/mod.rs#L862) and [`SavedBlock::deserialize_compact`](core/src/blocks/mod.rs#L1029) (`COMPACT_BLOCK_TAG = 0x80`), `barely-nano` eliminates redundant fields inside a `3`-byte bit-packed header:
     - **Hot Frontier Elision (`successor == 0`):** Active account head blocks (`~17.9%` of the ledger) omit the trailing 32 zero bytes of `successor` (`COMPACT_HAS_SUCCESSOR = 0x40` clear); when a newer block is later published for that account, [`BTree::update_compact_successor_32`](common/src/storage/btree.rs#L976-L1060) appends the 32-byte `successor` in place.
     - **1-Byte Top-256 Representative Index (`rl_code = 6..7`):** Across the Nano ledger, **89.61%** of all `StateBlock`s either have `representative == account` (`0 B` stored) or delegate to one of the Top 256 Principal/Historical Representatives configured in `compact_representatives` in [`config.toml`](config.toml), replacing the 32-byte public key with a **1-byte table index (`0..=255`)** decoded in $O(1)$ (`~1 ns`) via [`get_compact_rep_bytes`](core/src/blocks/mod.rs#L790-L800).
     - **Open Block, Link, Balance & Sideband Compression:** Open blocks (`previous == 0`, `~10.7%`) omit the 32-byte `previous` field; zero and epoch v1/v2 `link` values occupy `0 B`; `10^24`-scaled NANO balances and variable-length `height`/`timestamp` fields strip leading zero bytes; and [`Block::deserialize_with_hash`](core/src/blocks/mod.rs#L335-L342) reuses the 32-byte `BlockHash` from the B-Tree key to skip redundant Blake2b hashing on every disk read.
2. **No Copy-on-Write (COW) or Freelist DB Bloat (~8.7 GiB saved):** LMDB never modifies pages in place; every transaction allocates new 4 KB pages for the modified leaf and every parent branch page up to the root, recording old page numbers in `FREE_DBI`. In a 220.9M-block `data.ldb` (`32,029,037` pages), only `29,760,946` pages contain active leaf data, leaving **over 2.26 million pages (~8.65 GiB)** consumed by historical COW snapshots, freelist tracking, and deeper branch trees. `barely-nano` updates slotted pages in place in its 12 GB High-DRAM buffer pool and commits atomically via a fixed `256 MB` dual-area WAL.
3. **Higher Slotted-Page Packing Density (Up to 16 Blocks/Page vs. 13 in LMDB):** Because Nano block hashes and account public keys are uniformly distributed 256-bit values, random insertions into LMDB split 4 KB leaf pages in half (`50%` occupancy after split, averaging `~68%` across the file—wasting ~1.3 KB per page). `barely-nano`'s slotted-page B-Tree packs compact block entries (`209.3 B` average value + `42 B` key/slot/SoA head = `251.3 B/block`) up to **16 blocks per 4 KB leaf page**, reclaiming dead slot bytes in place via [`page_compact_skip`](common/src/storage/btree.rs#L381-L401) before splitting.
4. **In-Place Page Reclamation Without Offline Compaction:** When blocks or pending entries are deleted (or pruned), LMDB's `data.ldb` file never shrinks on disk unless taken offline and rewritten with `mdb_copy -c`. In `barely-nano`, each of the 64 B-Tree shards maintains an embedded $O(1)$ free-page list (`free_list_head`) that immediately recycles emptied 4 KB pages on the fly.

## Representative & Wallet Key Security Architecture (`wallet_crypto`)

To protect representative private keys against physical NVMe SSD theft, power loss, and unauthorized extraction without sacrificing bare-metal voting throughput, `barely-nano` combines **SpacemiT SoC hardware eFuse binding** with **iterated `Blake2b-256` key derivation (`KDF`)** in [`common/src/storage/wallet_crypto.rs`](common/src/storage/wallet_crypto.rs) and [`ledger_store/src/wallet_store.rs`](ledger_store/src/wallet_store.rs):

### 1. Physical & Cryptographic Isolation
- **Strict NVMe Partition Separation:** Wallet storage uses [`WALLET_SHARD`](common/src/storage/mod.rs) at `WALLET_STORAGE_LBA = 786,432` (`384 MB` offset) with its own 64-page redo journal. It never touches the 64 ledger [`SHARDS`](common/src/storage/mod.rs) or the ledger WAL (`>= 512 MB`), and survives full ledger wipes (`wipe_blocks = true`).
- **No Plaintext Keys or Passwords on Disk:** Neither the wallet `seed`, any account `priv_key`, nor the wallet `password` is ever written in plaintext to the NVMe SSD.
- **Hardware-Bound Key Derivation (`SpacemiT SoC Identity + Salt + Password`):**
  1. At boot, [`hardware_wallet_key()`](common/src/storage/wallet_crypto.rs#L77-L85) derives a 256-bit SoC hardware root key from the always-clocked **SpacemiT K3 Chip ID Unit (`CIU`, `0xD428_2C00`)** and U-Boot's eFuse/TLV-derived `serial#` / `ethaddr` / optional `wallet_hw_key` environment variables using trap-safe S-mode MMIO probes ([`trap::probe_read_u32`](common/src/trap.rs)), avoiding unclocked APB slave stalls on K3.
  2. Each wallet generates a unique 256-bit random `salt` and derives its 256-bit wrapping key via **4,096 rounds of iterated `Blake2b-256`**:
     $$\text{WrappingKey} = \text{Blake2b-256}^{4096}\bigl(\text{SoCeFuseKey} \parallel \text{WalletSalt} \parallel \text{Password}\bigr)$$
  3. The wallet `seed` and each account `priv_key` are encrypted with domain-separated `Blake2b-256` keystream pads (`crypt_seed`, `crypt_priv_key`) and authenticated by a 256-bit `check_tag` (`compute_check_tag`). On unlock, every decrypted private key is also verified against its stored `PublicKey` (`priv_key.public_key() == pub_key`).
- **Zero Runtime Voting Overhead:** Decryption runs only once when the wallet is unlocked (at boot or on `wallet_enter`, taking `< 1.5 ms`). Once unlocked, Hart 4 ([`VoteGenerator`](node/src/consensus/vote_generation/vote_generator.rs)) signs votes directly from RAM with 0% cryptographic overhead.

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

For Point-of-Sale (POS) terminals and merchant appliances, `barely-nano` implements **Watch-Only Stealth Address Derivation + $O(1)$ Stateless Ephemeral Invoice Generation** ([`nullables/sha/src/ed25519.rs`](nullables/sha/src/ed25519.rs), [`core/src/private_key.rs`](core/src/private_key.rs), [`node/src/wallets/wallet.rs`](node/src/wallets/wallet.rs)):

- **Zero Theft Surface on the POS Node:** The countertop node holds **only** a 256-bit Scan Key $v$ (`wallet.seed`) and the merchant's Cold Spend Public Key $S$ (`spend_public_key`). The Cold Master Spend Private Key $s$ is **never** loaded onto the node.
- **Additive Ed25519 Blinding:** For each invoice index $i$ (with domain tag $\text{DomainTag} =$ `"BARELY_NANO_STEALTH_V1"`), the node computes a deterministic blinding scalar:
  $$c_i = \text{Blake2b-512}\bigl(\text{DomainTag} \parallel v \parallel i_{\text{be}}\bigr) \bmod L$$
  and derives a one-time receiving account $P_i = S + c_i \cdot B$ using precomputed Radix-16 basepoint tables (`RADIX16_PRECOMP`). To the customer's wallet and the Nano network, $P_i$ is a standard `nano_1...` address.
- **$O(1)$ Memory & NVMe Storage (`"stateless": "true"`):** Passing `"stateless": "true"` to `account_create` updates **only** the 4-byte `deterministic_index` counter in [`PersistedWalletMeta`](ledger_store/src/wallet_store.rs#L33-L47) (`WALLET_SHARD`) without inserting per-invoice entries into `Wallet::keys` (`HashMap`) or allocating new NVMe pages—allowing millions of dynamic invoice QR codes in strictly $O(1)$ RAM and disk space.
- **Cold Vault Scalar Reconstruction:** An air-gapped Cold Vault holding both the Master Spend Private Key $s$ and Scan Key $v$ reconstructs the canonical one-time Ed25519 scalar key $p_i = (s_{\text{clamped}} + c_i) \pmod L$ via [`PrivateKey::from_stealth`](core/src/private_key.rs#L95-L102) / [`Wallets::insert_stealth_key_at`](node/src/wallets/wallets.rs#L635-L665) and signs standard Nano `StateBlock`s via [`sign_scalar`](nullables/sha/src/ed25519.rs#L1648-L1678).

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
| **Single-Board Computer** | **SpacemiT K3 Pico-ITX (16 GB LPDDR5)** | 16-core hybrid RVA23 RISC-V SoC (`8× X100 @ 2.4 GHz, VLEN=256` + `8× A100 @ 2.0 GHz, VLEN=1024`; `16 GB` dual-channel LPDDR5 `2 × MT62F2G32D4DS` at `0x1_0000_0000 .. 0x5_0000_0000` hosting a `3.00 GB` Heap and `12.00 GB` 64-shard B-Tree Buffer Pool; onboard **8 MB SPI NOR** `/dev/mtd0..mtd6` for FSBL/OpenSBI/U-Boot + **128 GB UFS**) |
| **NVMe SSD** | **$\ge 128\text{ GB}$ M.2 2280 PCIe Gen3 ×4** (up to 1 TB) | Installed in the **M.2 M-Key 2280 slot** (`pcie@80000000` `PCIE-0`, bonding dual 2-lane PHYs `phy@81d00000` + `phy@81e00000`; keep the M.2 B-Key slot empty to preserve all 4 PCIe Gen3 lanes). Partition 1 (`nvme 0:1`, `LBA 2048..524,287`, first `255 MiB`, FAT32 label `NANO_NODE`) holds `u-boot.itb`, `u-boot.bin`, `boot.scr`, `config.toml`, and `barely-nano.bin`; raw sectors `>= 256 MiB` (`LBA 524,288+`) store the `BlockBloom` filter, isolated hardware wallet B-Tree (`384 MiB`), and 64-shard B-Tree + WAL ledger (`512 MiB+`) |
| **USB-C NVMe Enclosure** | **USB 3.x M.2 NVMe Enclosure to Host PC** | Used on the host workstation (`/dev/sdX`) with `./build_u-boot.sh /dev/sdX --flash` and `./tools/lmdb_to_nvme.sh` to partition the `255 MiB` `NANO_NODE` FAT32 boot partition, copy `u-boot.itb` / `boot.scr` / `barely-nano.bin` / `config.toml`, and write the 64-shard Nano ledger directly to raw NVMe sectors |
| **GbE / 10GbE SFP+ Ethernet** | **Onboard Gigabit (`eth0: ethernet@cac80000`) or Realtek RTL8127 10GbE SFP+** | Active onboard RGMII 1GbE (plus RTL8127 10GbE SFP+ driver support) for P2P peering, bootstrap, JSON-RPC, WebSocket, and telemetry |
| **USB-to-TTL UART Adapter** | **3.3V Logic (`115200 8N1`)**, e.g. **DSD TECH SH-U09C5** | Connects to the debug UART0 header (`TX`, `RX`, `GND`) to monitor serial logs (`orpi.log`) |

### Optional (Recommended) Accessories

| Accessory | Specification | Purpose |
| :--- | :--- | :--- |
| **HDMI Status Display** | **Waveshare 5-Inch HDMI LCD (`800×480`, SimpleFB)** | Driven natively by Hart 15 ([`common/src/hdmi_fb.rs`](common/src/hdmi_fb.rs)) via U-Boot `simplefb` (`RGB565`/`XRGB8888`) to display real-time peers, blocks, confirmed height, BPS, active elections, and 12 GB Buffer Pool status |
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

## Step-by-Step Setup: Toolchain, U-Boot, SPI NOR (`mtd6`) & NVMe Boot Partition

The **SpacemiT K3 Pico-ITX** boots from an **8 MB onboard SPI NOR flash (`d420c000.spi`, `/dev/mtd0..mtd6`)** and comes from the factory with a 128 GB onboard UFS drive (`/dev/sda` on the board, pre-installed with Bianbu Linux):

| SPI NOR MTD Partition | Size | Name | Role in Boot Chain |
| :--- | :--- | :--- | :--- |
| `/dev/mtd0` | `8.0 MiB` | `d420c000.spi` | Whole 8 MB SPI NOR flash device |
| `/dev/mtd1` | `128 KiB` | `bootinfo` | Bootrom metadata and partition offsets (`0x00000`) |
| `/dev/mtd2` | `512 KiB` | `fsbl` | First-Stage Bootloader (`0x20000`; initializes 16 GB dual-channel LPDDR5, loads `esos`, `opensbi`, and `uboot` from `mtd6`) |
| `/dev/mtd3` | `64 KiB` | `env` | Factory U-Boot environment (`0xA0000`; ignored by our custom U-Boot via `CONFIG_ENV_IS_NOWHERE=y`) |
| `/dev/mtd4` | `1.0 MiB` | `esos` | Embedded system firmware (`0xB0000`) |
| `/dev/mtd5` | `384 KiB` | `opensbi` | OpenSBI M-mode firmware (`0x1B0000`, kept intact from factory) |
| `/dev/mtd6` | `5.9 MiB` | `uboot` | **U-Boot FIT image (`u-boot.itb` @ `0x210000`)** — `fsbl` matches board EEPROM `product_name = "k3-pico-itx"` (`conf_11` / `fdt_11`), loads `opensbi` (`0x1_0000_0000`) and `uboot` (`0x1_0200_0000`), and jumps to U-Boot via OpenSBI |

Because `barely-nano` starts its raw NVMe structures at **`256 MiB` (`BLOOM_HEADER_LBA = 524,288`)**, **`384 MiB` (`WALLET_STORAGE_LBA = 786,432`)**, and **`512 MiB` (`STORAGE_BASE_LBA = 1,048,576`)**, the first **`255 MiB` (`LBA 2048 .. 524,287`)** of the M.2 NVMe SSD safely hosts a standard MBR FAT32 Partition 1 (`NANO_NODE`, `nvme 0:1`) containing `u-boot.itb`, `u-boot.bin`, `boot.scr`, `config.toml`, and `barely-nano.bin` without ever overlapping the ledger or wallet partitions:

| NVMe LBA Range (512 B Sectors) | Byte Offset | Size | Purpose |
| :--- | :--- | :--- | :--- |
| **`LBA 2048 .. 524,287`** (`nvme 0:1`) | `1 MiB .. 256 MiB` | **`255 MiB`** | **FAT32 Boot Partition (`NANO_NODE`)**: holds `u-boot.itb`, `u-boot.bin`, `boot.scr`, `config.toml`, and `barely-nano.bin` |
| **`LBA 524,288 .. 655,367`** | `256 MiB .. 320 MiB` | `64 MiB + 4 KB` | Persistent `BlockBloom` filter (`BLOOM_HEADER_LBA = 524,288`) |
| **`LBA 786,432 .. 819,719`** | `384 MiB .. 400.25 MiB` | `16.25 MiB` | Isolated Hardware Wallet B-Tree + Redo Journal (`WALLET_STORAGE_LBA = 786,432`) |
| **`LBA 1,048,576` → End** | `512 MiB` → End | `~999.5 GB` | 64-Shard Ledger B-Tree + 256 MB Dual-Area WAL (`STORAGE_BASE_LBA = 1,048,576`) |

---

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

### 2. Build Custom SpacemiT K3 U-Boot ([`u-boot-pico-itx-k3/`](u-boot-pico-itx-k3)) & Prepare NVMe (`./build_u-boot.sh /dev/sdX --flash`)

The custom U-Boot submodule for the SpacemiT K3 Pico-ITX (`spacemit_k3_defconfig`) is located in [`u-boot-pico-itx-k3/`](u-boot-pico-itx-k3) (`https://github.com/caiovini/u-boot-pico-itx-k3.git`) and includes:
- **Extended 59-Entry Standalone Jump Table (`gd->jt` at `gp + 0x20`)** (`include/_exports.h`, `common/exports.c`) — Exports Ethernet (`eth_init`..`eth_set_current` at indices `42..48`), HDMI Video (`video_get_fb`..`bmp_display` at indices `49..55`), and primary NVMe Block I/O (`blk_get_capacity`, `blk_read_lba`, `blk_write_lba` at indices `56..58`).
- **A100 Cluster 2 & 3 Clock Ungating, 16-Hart DTB & Top-of-16GB Relocation** (`drivers/clk/spacemit/clk-k3.c`, `arch/riscv/cpu/k3/dram.c`, `k3.dtsi`, `k3-pico-itx.dts`) — Ungates Cluster 2/3 clocks, registers all 16 harts (`8× X100` + `8× A100`), and relocates U-Boot to the top of 16 GB LPDDR5 (`~0x4_FEED_xxxx .. 0x5_0000_0000`) above `barely-nano`'s 3.00 GB Heap and 12.00 GB Buffer Pool.
- **4-Lane PCIe Gen3 M.2 NVMe (`pcie@80000000`, `phy@81d00000` + `phy@81e00000`) & High-Throughput 64-Bit NVMe Driver (`NVME_Q_DEPTH = 64`, `MAX_PRP_POOL = 4096`)** (`drivers/phy/spacemit/phy-k1-pcie.c`, `k3.dtsi`, `k3-pico-itx.dts`, `drivers/nvme/nvme.c`) — Dual-PHY 4-lane PCIe Gen3 calibration for the Pico-ITX M.2 2280 NVMe slot (`~950 MiB/s` read) and zero-copy 64-bit PRP DMA directly from High DRAM (`0x1_E000_0000+`) with `256 KB` (`512`-sector) transfers.
- **Factory-SPL-Compatible External-Data `u-boot.itb` FIT Image for `/dev/mtd6` (`0x210000`)** ([`u-boot-pico-itx-k3/compile.sh`](u-boot-pico-itx-k3/compile.sh), [`u-boot-pico-itx-k3/firmware/`](u-boot-pico-itx-k3/firmware)) — Emits an `-E -B 8` external-data FIT image bundling `fw_dynamic.bin` (`0x1_0000_0000`), `u-boot.bin` (`0x1_0200_0000`, `CONFIG_OF_EMBED=y`), and `opensbi_fdt.dtb` (`fdt_11` / `conf_11` = `"k3-pico-itx"`) so factory `fsbl` (`mtd2`) boots it directly from SPI NOR `0x210000`.
- **Realtek RTL8127ATF 10GbE SFP+ (`eth1: eth_rtl8169` on `pcie2` Gen3 ×2) & Onboard Gigabit (`eth0: ethernet@cac80000`) Drivers** (`drivers/pci/pcie_dw_spacemit.c`, `drivers/net/rtl8169.c`, `drivers/net/rtl8127a_fw.h`, `drivers/net/dwc_eth_qos_spacemit.c`) — Hardware-verified 10GbE SFP+ driver (`rev 0x08` MAC MCU firmware, 10GBASE-R SerDes, 64-bit PCIe DMA, 1024-entry 32-byte TX ring, 512-entry RX ring) selected automatically via `setenv ethact eth1`, plus onboard RGMII GbE (`CONFIG_PHY_ANEG_TIMEOUT=500` fast offline boot).
- **Waveshare HDMI LCD SimpleFB (`CONFIG_VIDEO_DT_SIMPLEFB=y`)** — Initializes the HDMI framebuffer before jumping to `barely-nano.bin`.

#### One-Command Build, Repartition, Format & Copy to NVMe (`./build_u-boot.sh /dev/sdX --flash`)

1. Edit [`config.toml`](config.toml) on your host workstation to match your LAN (`ipv4_address`, `gateway`, `dns_server`, `mac_address`).
2. Build `barely-nano.bin` and `boot.scr` first using `make build`:
   ```bash
   make build
   ```
3. Plug your 1 TB M.2 NVMe SSD into your host PC via a USB-C NVMe enclosure and check its block device name with `lsblk` (for example, `/dev/sda`).
4. Run [`build_u-boot.sh`](build_u-boot.sh) with the target disk and `--flash`:
   ```bash
   ./build_u-boot.sh /dev/sda --flash
   ```
   What `./build_u-boot.sh /dev/sda --flash` does automatically:
   - Compiles custom U-Boot (`u-boot-pico-itx-k3/u-boot.itb` and `u-boot.bin`) and ensures `boot.scr` and `barely-nano.bin` are up to date.
   - Unmounts any active partitions on `/dev/sda` (while refusing to touch the host OS's internal `/dev/nvme0n1` or `/dev/nvme1n1` drives).
   - Deletes all existing partitions on `/dev/sda` (`wipefs -a` + `sfdisk --wipe always`) and creates a single bootable **`255 MiB` FAT32 partition (`start=2048, size=522240, type=0c`)** at `/dev/sda1` (`LBA 2048..524,287`).
   - Formats `/dev/sda1` as FAT32 with label **`NANO_NODE`** (`mkfs.vfat -F 32 -n NANO_NODE /dev/sda1`).
   - Mounts `/dev/sda1` and copies `u-boot.itb`, `u-boot.bin`, `boot.scr`, `config.toml`, and `barely-nano.bin` onto it.

   Verify with `lsblk`:
   ```text
   NAME   MAJ:MIN RM   SIZE RO TYPE MOUNTPOINTS
   sda      8:0    0 931.5G  0 disk
   └─sda1   8:1    0   255M  0 part
   ```

*(If you only want to compile U-Boot without flashing a disk, run `./build_u-boot.sh` with no arguments.)*

---

### 3. (Optional) Migrate the Nano Ledger to NVMe While Connected to the Host PC (`./tools/lmdb_to_nvme.sh`)

While the NVMe SSD is still connected to your host PC at `/dev/sda` right after `./build_u-boot.sh /dev/sda --flash`, you can pre-populate the 64-shard B-Tree ledger from a standard Nano `data.ldb` snapshot in minutes:

```bash
# Optional dry-run check first:
./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --dry-run

# Stream & convert data.ldb directly to /dev/sda raw sectors >= 256 MiB (leaves /dev/sda1 untouched!):
./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda

# Or migrate with on-the-fly ledger pruning (--prune):
./tools/lmdb_to_nvme.sh database/data.ldb /dev/sda --prune
```

Because `lmdb-to-nvme` writes strictly at **`LBA >= 524,288` (`256 MiB+`)** (`BlockBloom` at `256 MiB`, `SHARDSH4` and 64 B-Tree shards at `512 MiB+`), your `255 MiB` `NANO_NODE` boot partition (`/dev/sda1`, `LBA 2048..524,287`) and its boot files (`u-boot.itb`, `u-boot.bin`, `boot.scr`, `config.toml`, `barely-nano.bin`) remain completely intact.

---

### 4. Flash Custom `u-boot.itb` to SPI NOR (`mtd6` @ `0x210000`)

Once your NVMe SSD is installed in the **M.2 2280 M-Key slot**, flash custom `u-boot.itb` to onboard SPI NOR (`mtd6`, offset `0x210000`) using **Option A (recommended, over USB Type-C from your host PC)** or **Option B (from factory Bianbu Linux)**:

- **Option A — One-Command USB Type-C SPI NOR Flash from Host PC ([`flash_spi_k3.sh`](flash_spi_k3.sh)):**
  1. Connect a USB Type-C data cable from your host PC to the SpacemiT K3 Pico-ITX **USB Type-C download port**.
  2. Hold the **FEL / Download** button while pressing **RESET** (or powering on) so the board enumerates in BootROM USB mode (`361c:1001`).
  3. Run:
     ```bash
     ./build_u-boot.sh
     ./flash_spi_k3.sh
     ```
     [`flash_spi_k3.sh`](flash_spi_k3.sh) stages `fsbl.bin` to BootROM, stages `usb_stage_factory.itb` to SPL, stages `u-boot-pico-itx-k3/u-boot.itb` into DRAM (`0x1_4000_0000`), writes it to SPI NOR offset `0x210000` (`sf update`), verifies every byte via `sf read` + `cmp.b`, and resets into standalone NVMe boot.

- **Option B — Back Up `mtd0..mtd6` & Flash `/dev/mtd6` from Factory Bianbu Linux:**
  If the board is still running factory Bianbu Linux from onboard UFS (`root`):
  ```bash
  # 1. Back up all factory SPI NOR partitions (mtd0..mtd6)
  mkdir -p /root/spi_backup
  for i in 0 1 2 3 4 5 6; do dd if=/dev/mtdblock$i of=/root/spi_backup/mtd$i.bin bs=64K; done

  # 2. Mount NANO_NODE (/dev/nvme0n1p1) or USB drive (/dev/sdb1), save backup, and flash mtd6
  mkdir -p /mnt/boot && (mount /dev/nvme0n1p1 /mnt/boot || mount /dev/sdb1 /mnt/boot)
  cp -rv /root/spi_backup /mnt/boot/k3_spi_backup
  flashcp -v /mnt/boot/u-boot.itb /dev/mtd6
  cmp -n $(stat -c %s /mnt/boot/u-boot.itb) /mnt/boot/u-boot.itb /dev/mtdblock6 && echo "MTD6 VERIFIED OK!"
  sync && umount /mnt/boot && reboot
  ```

---

### 5. How `boot.scr` Probes PCIe, NVMe & USB and Boots `barely-nano.bin`

Our custom U-Boot in `/dev/mtd6` (`CONFIG_ENV_IS_NOWHERE=y`, [`spacemit_k3_defconfig`](u-boot-pico-itx-k3/configs/spacemit_k3_defconfig)) ignores any stale factory environment in `mtd3` and runs `CONFIG_BOOTCOMMAND` automatically on boot:
1. Scans PCIe (`pci enum`), M.2 NVMe (`nvme scan`), and USB (`usb start`).
2. If staged over USB Type-C via [`stage_k3.sh`](stage_k3.sh), decompresses `barely-nano.bin.gz` from `0x1_2000_0000` to `0x1_0400_0000`, persists the updated `barely-nano.bin` and `boot.scr` onto `nvme 0:1` (`fatwrite`), loads `config.toml`, and jumps to `0x1_0400_0000`.
3. On normal cold boots, loads `boot.scr` (`0x1_3830_0000`) from `usb 0:1` (USB override first), `nvme 0:1`, or `mmc 0:1` and executes [`boot.cmd`](boot.cmd):

```text
# Enumerate PCIe buses & M.2 NVMe SSD
pci enum
nvme scan

# Load config.toml into K3 DRAM at 0x103F00000 (64 KB zeroed buffer, 1 MB below kernel _start)
# Checks USB drive FIRST (usb 0:1), then NVMe Partition 1 (nvme 0:1), then eMMC (mmc 0:1)
mw.b 0x103F00000 0 0x10000
setenv config_size 0
if load usb 0:1 0x103F00000 config.toml || load nvme 0:1 0x103F00000 config.toml || load mmc 0:1 0x103F00000 config.toml; then
    setenv config_addr 0x103F00000
    setenv config_size ${filesize}
fi

if load usb 0:1 0x104000000 barely-nano.bin || load nvme 0:1 0x104000000 barely-nano.bin || load mmc 0:1 0x104000000 barely-nano.bin; then
    go 0x104000000
fi
```

---

### 6. Updating `barely-nano.bin`, `boot.scr`, or `config.toml` Without Reflashing SPI NOR

Once custom U-Boot (`u-boot.itb`) is flashed to `/dev/mtd6`, you **never need to reflash SPI NOR again** when updating `barely-nano.bin` or `config.toml`:
- **Option A — One-Command USB Type-C Stage & NVMe Auto-Update ([`stage_k3.sh`](stage_k3.sh)):**
  Without removing the NVMe SSD from the board, build `barely-nano.bin`, put the board into USB BootROM mode (`FEL + RESET`), and run:
  ```bash
  make build
  ./stage_k3.sh
  ```
  [`stage_k3.sh`](stage_k3.sh) compresses `barely-nano.bin` (`~2 MiB`), stages custom U-Boot + `barely-nano.bin.gz` + `boot.scr` over USB Type-C in `< 7s`, writes the updated `barely-nano.bin` and `boot.scr` onto `nvme 0:1` automatically, and boots `barely-nano`.
- **Option B — Update Files on a USB Drive (`usb 0:1`) or `NANO_NODE` (`nvme 0:1`):**
  Because `boot.scr` checks `usb 0:1` before `nvme 0:1`, plugging in a FAT32 USB flash drive containing `barely-nano.bin`, `boot.scr`, and/or `config.toml` overrides `nvme 0:1` immediately on boot (or copy them directly onto `/media/$USER/NANO_NODE/`):
  ```bash
  make build
  sudo cp -v barely-nano.bin boot.scr config.toml /media/$USER/NANO_NODE/
  sync && sudo umount /media/$USER/NANO_NODE
  ```

---

## Build & Deployment System

- [`Makefile`](Makefile) is the authoritative build interface.
- Cross-compiles for `riscv64gc-unknown-none-elf` with `K3_RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zicond,+zihintpause,+zvbb,+zvbc,+zvkb"`.
- Compiles [`boot.cmd`](boot.cmd) into `boot.scr` via `mkimage` and converts the ELF into `barely-nano.bin`.

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
| `make build-lmdb-to-nvme` | — | Builds the host-side [`tools/lmdb-to-nvme`](tools/lmdb-to-nvme) binary (`x86_64`) for migrating a Nano `data.ldb` snapshot directly to 64-shard NVMe sectors |
| `make build-nvme-snapshot` | — | Builds the host-side [`tools/nvme-snapshot`](tools/nvme-snapshot) binary (`x86_64`) for dumping a compact raw 64-shard ledger snapshot from a synced NVMe SSD (`dump`), inspecting it (`info`), and restoring it to an NVMe SSD (`restore`) |
| `make build-stress` | — | Builds the host-side `tools/nano-stress` binary (`x86_64`) |
| `make run-stress` / `make stress` | — | **Network stress test** — streams blocks to the node on port `7075` |
| `make run-picocom` | — | Captures serial console output from `/dev/ttyUSB0` to `orpi.log` |
| `make clean` | — | Cleans Cargo build artifacts |

## Deployment & Configuration (`config.toml`)

Node, RPC, WebSocket, and bare-metal network parameters are configured declaratively in [`config.toml`](config.toml), following the Nano / RSNano TOML schema.

### Live Configuration Loading (`0x1_03F0_0000` — No Recompilation Needed)
1. **U-Boot `load` (`0x1_03F0_0000`):** On boot, [`boot.cmd`](boot.cmd) (`boot.scr`) zeroes a 64 KB buffer at physical RAM address `0x1_03F0_0000` (1 MB below the kernel load address `0x1_0400_0000`), loads `config.toml` from `nvme 0:1` (or `mmc 0:1` / `usb 0:1`), and records `config_addr` and `config_size` in the U-Boot environment.
2. **Kernel Boot Parsing:** [`load_config_toml_str()`](main/src/bin.rs) reads `config.toml` directly from `0x1_03F0_0000` and merges it into `DaemonConfig` (`NodeConfig`, `NodeRpcConfig`, `WebsocketConfig`, and `BareMetalNetConfig`).
3. **Compile-Time Fallback:** If `config.toml` is not present in the boot partition or fails to parse, the kernel automatically falls back to the copy embedded at compile time (`include_str!("../../config.toml")`).

### Default `config.toml`

```toml
[bare_metal]
network = "live"
log_level = "info"
wipe_blocks = false
mac_address = "c0:74:2b:f9:15:ec"
ipv4_address = "192.168.1.200"
ipv4_prefix_len = 24
gateway = "192.168.1.1"
dns_server = "192.168.1.1"

[dashboard]
dashboard_enable = true
dashboard_port = 7080
dashboard_wallet_enabled = true
dashboard_pos_enable = true

[node]
peering_port = 7075
enable_voting = false
enable_bounded_backlog = false
allow_local_peers = true
online_weight_minimum = "60000000000000000000000000000000000000"
representative_vote_weight_minimum = "10000000000000000000000000000000"
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

When `[node.websocket]` is enabled (default port `7078`), you can subscribe to live node topics using [`tools/ws_subscribe.py`](tools/ws_subscribe.py):

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

Set `enable_pruning = true` under `[node.experimental]` in [`config.toml`](config.toml):

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
When pruning runs ([`LedgerPruning::ledger_pruning`](node/src/pruning.rs#L62-L108)), [`LedgerPruning::build_exempt_accounts`](node/src/pruning.rs#L51-L60) and [`Wallets::collect_pruning_exempt_accounts`](node/src/wallets/wallets.rs#L672-L717) automatically protect the merchant's own transaction history from ever being pruned while still pruning the rest of the global network:
1. **Explicitly Configured Accounts (`pruning_exempt_accounts`):** Any `nano_...` address listed in `pruning_exempt_accounts` under `[node.experimental]` in [`config.toml`](config.toml) is permanently exempt from pruning—ideal for offline Cold Vault, treasury, or accounting addresses whose keys are never loaded onto the node.
2. **Local Wallet & Watch-Only Accounts:** Every deterministic, ad-hoc, stealth-scalar, and watch-only account stored in any local wallet (plus any configured Cold Spend Public Key `spend_public_key`) is automatically exempt from pruning, even if the wallet is currently locked in RAM.
3. **Stateless Stealth Invoice Accounts (`0..deterministic_index`):** During each pruning pass, the node re-derives the wallet's stateless ephemeral/stealth invoice addresses (`0..deterministic_index`) on the fly, checks [`store.confirmation_height`](node/src/wallets/wallets.rs#L707), and exempts every paid invoice account so its complete `account_history` (`open`, partial payments, and sweep `send` blocks) is preserved indefinitely on NVMe without keeping unpaid invoice addresses in RAM.

### Representative Discovery on Bare-Metal

In a bare-metal environment with an incomplete ledger during initial sync, public representatives may appear to have 0 weight locally until their account chains are synced. Setting `representative_vote_weight_minimum = "0"` under `[node]` in [`config.toml`](config.toml) allows observing all voting representatives during initial sync.

## Fast Ledger Bootstrap: Direct LMDB to NVMe Migration (`tools/lmdb-to-nvme`)

Syncing 220M+ blocks from scratch over P2P bootstrap can take many hours. If you have a standard C++ `nano_node` or `rsnano` LMDB ledger snapshot (`data.ldb`), you can attach the 1 TB NVMe SSD to a Linux workstation (e.g. via a USB-C NVMe enclosure at `/dev/sda`) and use [`tools/lmdb-to-nvme`](tools/lmdb-to-nvme) ([`tools/lmdb_to_nvme.sh`](tools/lmdb_to_nvme.sh)) to convert and write the entire ledger directly onto the raw NVMe sectors in minutes.

### 1. How the Two-Pass Streaming Converter Works

Standard Nano nodes store their ledger in an LMDB file (`data.ldb`) with separate named sub-databases (`accounts`, `blocks`, `confirmation_height`, `pending`, `rep_weights`, `online_weight`, `peers`, `version`), whereas `barely-nano` uses a bare-metal 64-shard Slotted-Page B-Tree (`~15 GB` per shard on a 1 TB drive, `4 KB` pages, 1-byte `table_id` key prefix) plus a `64 MB` on-disk `BlockBloom` filter.

Instead of performing random 4 KB B-Tree insertions across 220M+ records (which would cause severe SSD write amplification), [`tools/lmdb-to-nvme/src/main.rs`](tools/lmdb-to-nvme/src/main.rs) executes a **2-pass parallel streaming pipeline**:

1. **Startup Self-Test (`verify_btree_builder_self_test`):** Before touching any disk sectors, the tool constructs an in-memory multi-level B-Tree of 5,000 entries and verifies that every page passes `barely-nano`'s exact `!page_is_full` invariant and binary-search lookup semantics.
2. **Pass 1 — Parallel Sequential `data.ldb` Leaf Scan:** Memory-maps `data.ldb` (`PROT_READ`, `MAP_PRIVATE`, `MADV_SEQUENTIAL`) and scans all 4 KB LMDB pages across all CPU cores (~6 GiB/s). Valid leaf pages (`mp_flags == 0x02`, `mp_pgno == pgno`) are parsed and each record is classified by its exact `(key_len, val_len)` and inner sub-tree root page numbers into:
   - `accounts` (`TABLE_ID = 1`, 32B key $\rightarrow$ 129B value)
   - `blocks` (`TABLE_ID = 2`, 32B key $\rightarrow$ self-contained bit-packed compact `SavedBlock` value (`107..=267 B`, `COMPACT_BLOCK_TAG = 0x80`) with 1-byte `compact_representatives` table index, omitted zero `previous`/`successor`/`link`, and variable-length balance/height/timestamp matching [`SavedBlock::serialize_compact`](core/src/blocks/mod.rs#L862))
   - `confirmation_height` (`TABLE_ID = 3`, 32B key $\rightarrow$ 40B value)
   - `pruned` (`TABLE_ID = 5`, 32B key $\rightarrow$ 0B value, populated when `--prune` is enabled or if the source `data.ldb` contains pre-existing pruned hashes)
   - `peers` (`TABLE_ID = 6`, 18B key $\rightarrow$ 8B value)
   - `pending` (`TABLE_ID = 7`, 64B key $\rightarrow$ 49B value)
   - `online_weight` (`TABLE_ID = 9`, 8B key $\rightarrow$ 16B value)
   - `rep_weights` (`TABLE_ID = 12`, 32B key $\rightarrow$ 16B value)
3. **Optional Full-History Pruning (`--prune` / `--prune-depth` / `--prune-age`):**
   - When `--prune` is passed, right after Pass 1 the tool builds a 4,096-bucket in-memory [`ConfHeightIndex`](tools/lmdb-to-nvme/src/main.rs) in parallel across all 64 shards (~1.5s).
   - During Pass 2, each block's `(account, height, timestamp, successor)` is inspected directly from its raw LMDB bytes in $O(1)$ time via [`should_prune_block`](tools/lmdb-to-nvme/src/main.rs).
   - The Genesis block, any account head block (`successor == 0`), any unconfirmed block (`height > confirmed_height`), and the top `--prune-depth` confirmed blocks (`height > confirmed_height - max_depth`, default `1` = confirmed frontier) are retained in `TABLE_BLOCKS (2)`.
   - All older confirmed blocks (`1 ..= confirmed_height - max_depth`) are omitted from `TABLE_BLOCKS (2)` and streamed in sorted order into `TABLE_PRUNED (5)` (`0`-byte value), reducing total NVMe B-Tree storage by **~65%** while recording exact `(account_count, block_count, confirmed_count, pruned_count)` in `CACHCNTS`.
4. **Pass 2 — Per-Shard 256-Bucket Parallel Radix Sort & Bottom-Up B-Tree Construction:**
   - Processes all 64 shards (`shard_idx = (key[1] >> 2)`), writing the active `compact_representatives` lookup table (`REPTBLC` / `REPTBLHD`, `REPTBL_MAGIC = 0x4E414E4F52455031`) into Shard `20` (`b'R' >> 2 == 20`)'s `TABLE_VERSION = 11` and running Shard `16` last (`b'C' >> 2 == 16`) so that `TABLE_VERSION = 11` receives the exact deduplicated global `CACHCNTS` (`COUNTS_MAGIC = 0x4E414E4F434E5431`: `account_count`, `block_count`, `confirmed_count`, `pruned_count`).
   - Within each `(shard, table)`, records are partitioned by their next key byte into 256 strictly ordered sub-buckets, sorted in parallel across all CPU threads, and deduplicated so only the newest LMDB Copy-On-Write record for each key survives.
   - [`StreamingBTreeBuilder`](tools/lmdb-to-nvme/src/main.rs) packs sorted records into 4 KB leaf and internal pages at an **~88% fill factor** (`PAGE_TARGET_USED = 3584` bytes, `dead_bytes = 0`, leaving headroom so runtime mutations never trigger an unsplit `page_is_full`) and flushes them in **16 MB sequential chunks** to `/dev/sda`.
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
5. Ensure `wipe_blocks = false` under `[bare_metal]` in [`config.toml`](config.toml), re-insert the NVMe SSD into the SpacemiT K3 Pico-ITX, and boot the board. The node will detect `STORAGE_MAGIC` (`"SHARDSH4"`) and `BLOOM_MAGIC` (`"BLOMV2"`), load the exact block/cemented/account/pruned counts from `CACHCNTS` (automatically enabling `enable_pruning` if `pruned_count > 0`), and immediately join live consensus at the snapshot frontier.

## Direct NVMe Ledger Snapshot & Restore (`tools/nvme-snapshot`)

Once your SpacemiT K3 Pico-ITX has finished syncing the live Nano ledger on its NVMe SSD, you can attach the NVMe SSD to a Linux host over USB-C (e.g. `/dev/sda`) and use [`tools/nvme-snapshot`](tools/nvme-snapshot) ([`tools/nvme_snapshot.sh`](tools/nvme_snapshot.sh)) to export a **compact, checksummed raw ledger snapshot file (`BNSNAPV1`)** or restore a snapshot file directly onto any target NVMe SSD in minutes.

### 1. How Sparse-Shard Snapshot Streaming & WAL Replay Works

On a 1 TB NVMe drive, the 64 B-Tree shards are spaced ~15 GB apart across the 1 TB LBA address space (`shard_size = 3,903,977` pages per shard), even though the allocated B-Tree pages (`0 .. next_free_page`) occupy only **~68–90 GiB total** for an unpruned ledger or **~24–30 GiB total** for a pruned ledger.

Instead of `dd`-ing 1,000 GB of mostly unallocated sectors, [`tools/nvme-snapshot/src/main.rs`](tools/nvme-snapshot/src/main.rs) operates directly on `barely-nano`'s native storage structures:

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
| `0x0000_0000` | `4 KB` (`1` page) | **`BNSNAPV1` Header** | Magic (`"BNSNAPV1"`), flags (`FLAG_HAS_BLOOM`, `FLAG_HAS_WALLETS`, `FLAG_PRUNED`), timestamp, source `shard_size` & `wal_epoch`, cached ledger counts (`accounts`, `blocks`, `confirmed`, `pruned`), 64-bit region checksums, and 64 × 32-byte [`ShardDescriptor`](tools/nvme-snapshot/src/main.rs) entries (`root_page_id`, `next_free_page`, `first_free_page`, `file_offset_bytes`, `checksum`) |
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
6. Re-insert the NVMe SSD into the SpacemiT K3 Pico-ITX (with `wipe_blocks = false` in [`config.toml`](config.toml)) and boot. The node will immediately mount all 64 shards and the 64 MB `BlockBloom` filter with zero WAL recovery needed.

## Network Stress Testing (`tools/nano-stress`)

The repository provides a high-throughput stress testing tool (`tools/nano-stress`) to test node stability, B-Tree storage performance, and queue behavior under heavy load by generating cryptographically valid blocks and streaming them directly over TCP to port **7075**.

### 1. Node Dev Network Setup

To prepare the node for stress testing, set the following in [`config.toml`](config.toml) in the `NANO_NODE` boot partition:

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
- [`spacemit_k3_migration.md`](spacemit_k3_migration.md) — SpacemiT K3 Pico-ITX (`16 GB LPDDR5`) 16-hart, 64-shard bare-metal architecture reference.
- [`config.toml`](config.toml) — Declarative TOML configuration for bare-metal network, node, bootstrap, queues, RPC, and WebSocket server.
- [`main/`](main) — Main bare-metal binary entry point (`entry.S`, `bin.rs`) and 16-hart SMP loops.
- [`common/`](common) — Core kernel library: S-mode trap handler, UART/SBI/U-Boot wrappers, Waveshare 5-inch HDMI SimpleFB driver (`hdmi_fb.rs`), 64-shard B-Tree + 12 GB High-DRAM BPM + atomic WAL storage engine, cooperative async executor, and `smoltcp` networking stack.
- [`core/`](core) — Core Nano primitives (blocks, accounts, keys, amounts, votes, difficulty).
- [`messages/`](messages) — Nano P2P wire protocol messages and serialization.
- [`ledger/`](ledger) & [`ledger_store/`](ledger_store) — Ledger state management, 3-stripe parallel block insertion/validation, and 64-shard B-Tree store tables.
- [`network/`](network) & [`network_protocol/`](network_protocol) — P2P channel management, handshakes, and message framing.
- [`node/`](node) — High-level Nano node (consensus, active elections, vote processor, bootstrapper, cementation, telemetry, TOML config).
- [`rpc_messages/`](rpc_messages), [`rpc_handler/`](rpc_handler), [`rpc_server/`](rpc_server) — Bare-metal `no_std` JSON-RPC message types, command handlers, and HTTP server.
- [`websocket_messages/`](websocket_messages), [`websocket_client/`](websocket_client), [`websocket_server/`](websocket_server) — Bare-metal `no_std` WebSocket envelopes, client, and real-time event broadcast server.
- [`nullables/`](nullables) — Hardware/OS abstraction crates (`sha` RVA23 / `Zvkb` `VLEN=256` & `VLEN=1024` crypto, `clock`, `random`, `tcp`, `http_client`, `output_tracker`) with nullable test doubles.
- [`stats/`](stats) & [`work/`](work) — Node statistics collector and PoW work generation/validation.
- [`tools/`](tools) — Host-side utilities: [`lmdb-to-nvme`](tools/lmdb-to-nvme) & [`lmdb_to_nvme.sh`](tools/lmdb_to_nvme.sh) (direct `data.ldb` to raw 64-shard NVMe B-Tree + Bloom migration tool), [`nvme-snapshot`](tools/nvme-snapshot) & [`nvme_snapshot.sh`](tools/nvme_snapshot.sh) (direct NVMe to compact raw 64-shard ledger snapshot dump & restore tool), [`nano-stress`](tools/nano-stress) (P2P block streamer), and [`ws_subscribe.py`](tools/ws_subscribe.py) (WebSocket topic subscriber).
- [`build_u-boot.sh`](build_u-boot.sh) — Builds `u-boot-pico-itx-k3` (`u-boot.itb`, `u-boot.bin`), `boot.scr`, and `barely-nano.bin`, and with `/dev/sdX --flash` automatically deletes old partitions, formats the `255 MiB` FAT32 `NANO_NODE` boot partition (`LBA 2048..524,287`), and copies all boot files.
- [`flash_spi_k3.sh`](flash_spi_k3.sh) & [`stage_k3.sh`](stage_k3.sh) — One-command USB Type-C utilities: `flash_spi_k3.sh` permanently flashes and byte-verifies custom `u-boot.itb` in SPI NOR (`mtd6` @ `0x210000`), while `stage_k3.sh` stages `barely-nano.bin.gz` + `boot.scr` into RAM and updates `nvme 0:1` automatically.
- [`u-boot-pico-itx-k3/`](u-boot-pico-itx-k3) — Custom SpacemiT K3 Pico-ITX (`spacemit_k3_defconfig`) U-Boot git submodule (`https://github.com/caiovini/u-boot-pico-itx-k3.git`) with 59-entry Standalone Jump Table (`eth_*`, `video_*`, `blk_*`), 4-lane M.2 NVMe PCIe (`pcie@80000000`, `phy@81d00000` + `phy@81e00000`) & 64-bit NVMe PRP DMA, factory-SPL-compatible external-data `/dev/mtd6` FIT layout (`conf_11` / `k3-pico-itx` + [`firmware/`](u-boot-pico-itx-k3/firmware)), onboard GbE + RTL8127 10GbE SFP+ drivers, and A100 Cluster 2/3 clock ungating.
- [`linker-smode.ld`](linker-smode.ld) — S-mode linker script mapping the `0x1_0400_0000` physical memory layout for the SpacemiT K3 Pico-ITX (`16 GB LPDDR5`).

## Authors & Credits

`barely-nano` is a bare-metal `no_std` RISC-V fork and hardware port of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node).

- **Caio Vinicius** (`<cvini@proton.me>`) — Bare-metal `no_std` RISC-V architecture, SpacemiT K3 / K1 SMP kernel, RVA23 / RVV 1.0 / `Zvkb` hardware-accelerated cryptography, 64-shard NVMe B-Tree + 12 GB High-DRAM Buffer Pool + WAL storage engine, U-Boot `smoltcp` networking, and embedded RPC/WebSocket/PoS stack.
- **Gustav Schauwecker** (`<service@simpago.com>`) — Creator and lead author of the original [RsNano](https://github.com/rsnano-node/rsnano-node) Rust implementation of the Nano node.
- **Gemini 4 Carbon** — Co-author on the `no_std` migration, storage engine, and RISC-V systems engineering.

