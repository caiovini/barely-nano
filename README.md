# Bare-Metal RISC-V Nano Node (barely-nano)

This project is a bare-metal `no_std` RISC-V Nano cryptocurrency node designed to run directly on the **Orange Pi RV2 (SpacemiT K1 / Ky X1 SoC)** with 8× X60 RISC-V harts, 8 GB RAM, and a 1 TB NVMe SSD.

It establishes a self-contained operating environment on top of OpenSBI and U-Boot (`CONFIG_API=y`), complete with a heap allocator, multi-core exception handling, hardware-accelerated cryptography, a 16-shard B-Tree storage engine on NVMe, and a full TCP/IP networking stack.

## Features Implemented

### 1. S-Mode Execution on OpenSBI & U-Boot
The kernel runs in **Supervisor Mode (S-mode)** launched from U-Boot on top of OpenSBI firmware, utilizing SBI `ecall`s for multi-hart management (HSM) and IPIs, and U-Boot's Standalone Jump Table & Syscall API (`CONFIG_API=y`) for raw Ethernet and NVMe block device I/O.

### 2. Memory Management
- **Boot sequence:** Written in custom assembly (`main/src/entry.S`) to set up stack pointers for all 8 harts, capture U-Boot's Jump Table pointer, zero BSS, and jump into Rust.
- **Heap Allocator:** Uses `linked_list_allocator` wrapped in `InterruptSafeAllocator` to provide a global dynamic memory allocator (`ALLOCATOR`).
- **Configuration:** Configured in `linker-smode.ld` with a **~1.45 GB heap** and **256 MB stack** (32 MB per hart) mapped starting at `0x11000000`.

### 3. Exception and Trap Handling
- **Trap Vector:** Custom S-mode assembly trap vector (`trap_vector`) saves and restores the full register context upon interrupts and exceptions.
- **Trap Dispatcher:** The Rust-based trap handler evaluates the `scause` CSR to differentiate between asynchronous interrupts and synchronous exceptions (like access faults).
- **Diagnostics:** Exceptions automatically print out the `scause`, `sepc`, and `ra` registers over the SpacemiT K1 UART0 (`0xD401_7000`) to ease debugging.

### 4. Hardware Watchdogs & Timers
- **CLINT Timer:** Reads the 24 MHz hardware `time` CSR for cooperative async scheduling and timestamps.
- **Watchdog Management:** Disables and periodically kicks both the SpacemiT K1 SoC watchdog (`0xD408_0000`) and the SPM8821 PMIC watchdog over PWR I2C bus 8 (`0xD401_D800`).

### 5. Hardware Accelerated Cryptography & Optimizations
To achieve high transaction throughput and maximize CPU efficiency, the node leverages a comprehensive suite of advanced RISC-V architecture extensions:
- **Vector Extension (V):** SIMD processing for vectorized implementations of Blake2b compression and high-throughput cryptography.
- **Bit-Manipulation (Zbb, Zba, Zbs, Zbc):** Comprehensive scalar bit-manipulation, address generation, single-bit operations, and carry-less multiplication, which dramatically speed up cryptographic primitives (e.g., compiling 64-bit rotations to single `rori` instructions) and index calculations.
- **Cache-Block Zero (Zicboz):** Fast memory clearing instructions used to efficiently zeroize memory pages and large structures.
- **Pause Hint (Zihintpause):** Crucial for wait-free spinlocks. It reduces power consumption and bus contention while harts are spinning on atomic variables.
- **Cache-Block Management (Zicbom):** Explicit cache maintenance operations (`cbo.clean`, `cbo.inval`), essential for managing cross-cluster cache coherency between Cluster 0 (harts 0–3) and Cluster 1 (harts 4–7).

These extensions allow the bare-metal node to perform hashing, signature verification, and lock-free concurrency significantly faster than standard scalar architectures.

### 6. U-Boot Networking & TCP/IP Stack
- **Hardware Probing:** Discovers and initializes the board's Ethernet controller via the U-Boot Standalone API (`UbootNetDevice`).
- **TCP/IP Stack (`smoltcp`):** A full-featured `no_std` network stack integrated with the U-Boot Ethernet driver.
- **Nano Node Integration:** Powers a functional **Nano cryptocurrency node**, handling real-time block synchronization, peer discovery, RPC requests (`7076`), and peering (`7075`).

### 7. Multi-Hart SMP & Hardware Cache Coherency
- **SpacemiT K1 SoC Support:** Supports multi-hart execution across both CPU clusters on the Orange Pi RV2 hardware (Harts 1–7).
- **Cross-Cluster Coherency:** Uses explicit `Zicbom` cacheline flushes (`cbo.clean` / `cbo.inval`) and `SeqCst` memory barriers to ensure heap and BSS visibility across both CPU clusters.
- **Task Dispatch:** Dedicated hart loops assign specific node workloads (RPC, Vote Processing, Block Processing, Consensus/AEC, Heavy I/O, Monitoring) to individual harts, while idle cycles assist with parallel Ed25519 signature verification.

### 8. I2C OLED Status Display (`common/src/oled.rs`)
- **Zero-Allocation `no_std` Driver:** Supports 4-pin I2C **SSD1306** and **SH1106** 128×64 OLED displays (at `0x3C` or `0x3D`) over hardware `I2C4` (`0xD401_2800`), with automatic GPIO I2C fallback if `SCK`/`SDA` are swapped or wired to `I2C3` (Pins 8 & 10).
- **Differential Refresh on Hart 7:** Refreshes every 2 seconds on the Monitor hart (Hart 7), transmitting only text rows that have changed.

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
- **Line 4:** `<bps>/S DB:<size>` — Block processing rate (blocks/sec) and NVMe B-Tree database size (`K`/`M`)

## Storage Architecture: Bare-Metal Raw Block Database

Traditional filesystems (like FAT32 or EXT4) introduce substantial overhead and cannot easily handle massive ledger databases natively in a pure `no_std` environment. To address this, `barely-nano` employs a custom **16-Shard Buffer Pool + B-Tree** stack.

### The Stack
1. **Physical Layer (`StorageBackend::UBoot`):** Exposes raw 4 KB blocks (8 × 512-byte sectors) over the U-Boot block storage syscall interface directly to the NVMe SSD (starting at a 512 MB LBA offset to preserve boot partitions), with a 128 MB RAM disk fallback.
2. **Buffer Pool Manager:** A software cache residing in RAM that manages 128 × 4 KB 64-byte-aligned frames per shard. It minimizes hardware I/O by caching frequently accessed blocks and persists modifications using an **LRU (Least Recently Used)** eviction policy.
3. **B-Tree Engine:** A specialized B-Tree implementation built directly on the Buffer Pool across 16 prefix-partitioned shards. It maps keys and values to persistent 4 KB pages, providing an efficient, ordered key-value store without any underlying OS filesystem.

### 9. Build & Deployment System
- The `Makefile` handles cross-compilation (`riscv64gc-unknown-none-elf`) with all Orange Pi RV2 target features enabled (`+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom`).
- Converts the ELF binary into a raw bootable image (`barely-nano.bin`) and automatically copies it to a mounted SD card (`/run/media/cvini/NANO_NODE`) if present.

### Build Targets

| Target | Feature flag | Purpose |
|---|---|---|
| `make build` / `make build-smode` | `simd` | **Live network mode** — builds `barely-nano.bin` for the Orange Pi RV2 |
| `make build-spammer` | `spammer,simd` | **Test mode** — builds `barely-nano.bin` with local block spammer enabled |
| `make run-crypto-tests` | `simd` | Runs host-side cryptographic unit tests |
| `make run-picocom` | — | Captures serial console output from `/dev/ttyUSB0` to `orpi.log` |
| `make clean` | — | Cleans Cargo build artifacts |

## Deployment & Configuration

Unlike standard applications that use TOML/YAML files or environment variables, `barely-nano` uses **Static Hardcoded Configuration**.

In a bare-metal `no_std` environment:
- There is no OS filesystem to parse `.toml` files from.
- There is no shell to pass command-line arguments.

As a result, all node parameters—such as **Peer IP addresses**, **Network type (Live/Beta/Test)**, and **Resource limits**—are defined directly in [main/src/bin.rs](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs). Any change to the deployment environment (e.g., moving to a different hardware node with a different IP) requires updating the source code and recompiling the binary. (Note: The RPC server port is currently managed directly in the `handle_rpc_server` loop within `bin.rs`).

### Configuration Example

Modify `main/src/bin.rs` to adjust settings before the node starts:

```rust
// In main/src/bin.rs
let mut config = NodeConfig::default_for(Networks::NanoLiveNetwork, 8);

// Change Peering port
config.network.listening_port = 7075;

// Add static peers (essential for bare-metal discovery)
config.preconfigured_peers.clear();
config.preconfigured_peers.push(Peer::new("192.168.0.245".to_string(), 7075));

// Tune TCP limits for hardware constraints
config.tcp.max_inbound_connections = 128;
config.tcp.max_outbound_connections = 128;
```


Pruning allows the node to delete old, confirmed blocks to save disk space. In this bare-metal environment, the node uses a custom **B-Tree with Free-List reclamation** to ensure that deleted space is reused physically on the disk.

#### How to Activate Pruning

To enable pruning, modify the `NodeFlags` in `main/src/bin.rs`:

```rust
let mut flags = NodeFlags::new();
flags.enable_pruning = true; // Enables background pruning workers
```

#### Configuration

Pruning is guided by age and depth limits in `NodeConfig`:

```rust
// Retain 48 hours of history (in seconds)
config.max_pruning_age_s = 2 * 24 * 60 * 60; 

// Maximum blocks per account (0 = unlimited depth, age-only)
config.max_pruning_depth = 0;
```

#### Important: The Safe Wipe
When `enable_pruning` is set to `true`, the node is programmed to **automatically wipe the existing database** on startup. This is done to:
1.  **Ensure Format Compatibility**: Pruning requires a specific B-Tree metadata format (Page 0) to manage the free-list.
2.  **Optimize Storage**: Starting from a clean pruned state (0 GB) is significantly more efficient than trying to prune a full 100GB+ ledger in place on bare-metal hardware.

Once initialized, the B-Tree will automatically reuse space from pruned blocks on the NVMe SSD indefinitely.

### Representative Discovery on Bare-Metal

In a bare-metal environment with an incomplete ledger (e.g., during the initial sync or when running with a limited history), you may observe very few representatives being registered by the node.

**Why this happens:**
- **Weight Thresholding:** By default, the node only "counts" a representative as online if it has at least **10 Nano** of weight according to the local ledger.
- **Ledger Lag:** If your ledger only contains the first few thousand blocks, it does not yet contain the transaction history that establishes the current weights of public representatives. To your node, these representatives appear to have **0 weight**.
- **Discovery Gate:** Consequently, even if the node receives valid votes from these representatives via the `RepCrawler` or network rebroadcasts, it will filter them out of the `OnlineReps` list because they fall below the weight threshold.

**To temporarily observe all representatives (even those with 0 weight):**
Set `representative_vote_weight_minimum` to `Amount::zero()` in `node/src/config/node_config.rs`. This is useful for debugging network connectivity but should be reverted once the ledger is sufficiently synced to provide accurate weight data.

## Structure
- `/main/` - The main executable binary, entry points, and static node configuration.
- `/common/` - The core kernel library, housing the trap handler, UART/SBI/U-Boot wrappers, storage engine, and networking stack.
- `/core/` - Core Nano cryptocurrency logic (blocks, accounts, amounts, voting).
- `/node/` - The high-level Nano node implementation (consensus, block processing, telemetry).
- `/nullables/` - Shared infrastructure components (clock, random, storage, networking) with nullable support for testing.
- `/ledger/` & `/ledger_store/` - Ledger state management and the BPM-backed storage implementation.
- `/network/` & `/network_protocol/` - Nano-specific network message handling and peer management.
- `/rpc_server/` & `/rpc_client/` - Bare-metal Nano RPC interface.
- `linker-smode.ld` - Linker script mapping the physical memory layout for the Orange Pi RV2.
