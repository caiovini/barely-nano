# Bare-Metal RISC-V OS (barely-nano)

This project is a bare-metal RISC-V application designed to run on the QEMU `virt` machine, with the ultimate goal of porting network applications (specifically a high-performance Nano cryptocurrency node) to custom RISC-V hardware like the **Orange Pi RV2 (JH7110)**.

Currently, it establishes a foundational operating environment, complete with a massive heap allocator, multi-core interrupt handling, hardware-accelerated cryptography, and a full TCP/IP networking stack.

## Features Implemented

### 1. Dual Execution Modes
The kernel can be built to run in two distinct privilege modes using Cargo features (`mmode` or `smode`):
- **Machine Mode (M-mode):** Runs directly on the hardware without any underlying firmware.
- **Supervisor Mode (S-mode):** Runs on top of OpenSBI firmware. This mirrors the boot chain of the JH7110 SoC used on the Orange Pi, utilizing SBI `ecall`s for console and timer configuration.

### 2. Memory Management
- **Boot sequence:** Written in custom assembly to set up stack pointers for all 8 harts, zero BSS, and jump into Rust.
- **Heap Allocator:** Uses `linked_list_allocator` to provide a global dynamic memory allocator (`ALLOCATOR`).
- **Configuration:** Currently configured with a **1.5GB heap** and **256MB stack** (32MB per hart). The physical hardware target (Orange Pi RV2) supports up to **8GB LPDDR4 RAM**.

### 3. Exception and Trap Handling
- **Trap Vector:** Custom assembly trap vector (`trap_vector`) saves and restores the full register context upon interrupts and exceptions.
- **Trap Dispatcher:** The Rust-based trap handler evaluates the `xcause` CSR to differentiate between asynchronous interrupts (like timers) and synchronous exceptions (like access faults).
- **Diagnostics:** Exceptions automatically print out the `xcause`, `sepc`/`mepc`, and `stval`/`mtval` registers over UART to ease debugging.

### 4. Timer Interrupts
- Configured to issue a hardware interrupt every 1 second.
- In M-mode, accesses the hardware `CLINT` MMIO registers to set `mtimecmp`.
- In S-mode, issues an `sbi::set_timer` ecall to request the firmware to trigger the next timer interrupt.

### 5. Hardware Accelerated Cryptography & Optimizations
To achieve high transaction throughput and maximize CPU efficiency, the node leverages a comprehensive suite of advanced RISC-V architecture extensions:
- **Vector Extension (V):** SIMD processing for vectorized implementations of Blake2b compression and high-throughput cryptography.
- **Vector-Crypto (Zvkb):** Optimized vector rotation and logic instructions to accelerate Blake2b and Ed25519 routines.
- **Bit-Manipulation (Zbb, Zba, Zbs, Zbc):** Comprehensive scalar bit-manipulation, address generation, single-bit operations, and carry-less multiplication, which dramatically speed up cryptographic primitives and index calculations.
- **Cache-Block Zero (Zicboz):** Fast memory clearing instructions used to efficiently zeroize memory pages and large structures.
- **Pause Hint (Zihintpause):** Crucial for our wait-free spinlocks. It reduces power consumption and bus contention while harts are spinning on atomic variables.
- **Cache-Block Management (Zicbom):** Explicit cache maintenance operations, essential for managing VirtIO DMA consistency between the CPU and hardware devices.

These extensions allow the bare-metal node to perform hashing, signature verification, and lock-free concurrency significantly faster than standard scalar architectures.

### 6. VirtIO Networking & TCP/IP Stack
- **Hardware Probing:** Discovers and initializes VirtIO Network devices via MMIO.
- **TCP/IP Stack (`smoltcp`):** A full-featured network stack integrated with the VirtIO driver.
- **Nano Node Integration:** Instead of just simple pings, the stack powers a functional **Nano cryptocurrency node**, handling real-time block synchronization, peer discovery, and vote processing.

### 7. Multi-Hart SMP & Hardware Cache Coherency
- **SpacemiT K1 SoC Support:** Fully supports all 8 harts across both CPU clusters on the Orange Pi RV2 hardware.
- **Cache Coherent Interconnect (CCI):** The OpenSBI platform initialization has been patched to correctly configure the CCI hardware snoop domains for all clusters. This guarantees L2 cache coherency between harts without the need for manual software flushing, preventing cross-cluster lockups.
- **Task Dispatch:** A dedicated heartbeat loop assigns specific node workloads (RPC, Vote Processing, Block Processing, HeavyIO) to individual harts, utilizing wait-free spinlocks and cross-cluster `Arc` sharing for highly efficient concurrent execution.

## Storage Architecture: Bare-Metal Raw Block Database

Traditional filesystems (like FAT32 or EXT4) introduce substantial overhead and cannot easily handle massive 80GB databases natively in a pure `no_std` environment. To address this, `barely-nano` employs a custom **Buffer Pool + B-Tree** stack.

### The Stack
1. **Physical Layer (`virtio-blk`):** The driver exposes raw sectors over the VirtIO Block device interface (representing NVMe or SSD storage).
2. **Buffer Pool Manager:** A software cache residing in RAM that manages 4KB "frames". It minimizes hardware I/O by caching frequently accessed blocks and persists modifications using an **LRU (Least Recently Used)** eviction policy.
3. **B-Tree Engine:** A specialized B-Tree implementation built directly on the Buffer Pool. It maps keys and values to persistent 4KB pages, providing an efficient, ordered key-value store without any underlying OS filesystem.

### 8. Automated Build & Run System
- A comprehensive `Makefile` handles cross-compilation (`riscv64gc-unknown-none-elf`).
- Launches the QEMU emulator with the correct configurations for either M-mode or S-mode.
- Configured with **8 CPUs** and **4GB RAM** by default to simulate high-end RISC-V hardware.
- Automatically connects the host network to the VM via `-netdev user` and sets up TCP port forwarding for Nano RPC (`7076`) and Peering (`7075`).

### Build & Run Targets

| Target | Feature flag | Purpose |
|---|---|---|
| `make run` | _(none)_ | **Live network mode** — connects to real Nano peers, no local block generation |
| `make build` | _(none)_ | Builds the live-network binary |
| `make run-spammer` | `spammer` | **Test mode** — runs a local block spammer to stress-test the B-Tree storage |
| `make build-spammer` | `spammer` | Builds the spammer binary |
| `make run-mmode` | `mmode` | M-mode (no OpenSBI), direct CLINT access |
| `make kill` | — | Kills the running QEMU instance |

The `spammer` feature is **off by default**. Switching between live network and test mode requires no code changes — just a different `make` target:

```sh
# Live network (default)
make kill && make run

# Local spammer (B-Tree / throughput testing)
make kill && rm disk.img && make run-spammer
```


## Deployment & Configuration

Unlike standard applications that use TOML/YAML files or environment variables, `barely-nano` uses **Static Hardcoded Configuration**.

In a bare-metal `no_std` environment:
- There is no OS filesystem to parse `.toml` files from.
- There is no shell to pass command-line arguments.

As a result, all node parameters—such as **Peer IP addresses**, **Network type (Live/Beta/Test)**, and **Resource limits**—are defined directly in [main/src/bin.rs](file:///home/cvini/rust-workspace/barely-nano/main/src/bin.rs). Any change to the deployment environment (e.g., moving to a different hardware node with a different IP) requires updating the source code and recompiling the binary. (Note: The RPC server port is currently managed directly in the `handle_rpc_server` loop within `bin.rs`).

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

Once initialized, the B-Tree will automatically reuse space from pruned blocks to keep the `disk.img` compact indefinitely.

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
- `/common/` - The core kernel library, housing the trap handler, UART/SBI wrappers, and the networking stack.
- `/core/` - Core Nano cryptocurrency logic (blocks, accounts, amounts, voting).
- `/node/` - The high-level Nano node implementation (consensus, block processing, telemetry).
- `/nullables/` - Shared infrastructure components (clock, random, storage, networking) with nullable support for testing.
- `/ledger/` & `/ledger_store/` - Ledger state management and the BPM-backed storage implementation.
- `/network/` & `/network_protocol/` - Nano-specific network message handling and peer management.
- `/rpc_server/` & `/rpc_client/` - Bare-metal Nano RPC interface.
- `linker-mmode.ld` / `linker-smode.ld` - Linker scripts mapping the memory layout for the respective privilege modes.
