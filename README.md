# Bare-Metal RISC-V Nano Node (`barely-nano`)

`barely-nano` is a bare-metal `no_std` RISC-V fork and hardware port of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node), designed to run a full Nano cryptocurrency node directly on RISC-V hardware in Supervisor Mode (`S-mode`) on top of OpenSBI and U-Boot (`CONFIG_API=y`) without an operating system, filesystem, or traditional database engine.

---

## Hardware Target Branches

Because bare-metal memory maps, SMP cluster topologies, cache hierarchies, and peripheral MMIO layouts are tightly coupled to each SoC and board, active code and build toolchains are maintained in board-specific branches:

| Branch | Target Board & SoC | CPU / Harts | Memory & Storage | Description |
| :--- | :--- | :--- | :--- | :--- |
| **[`orangepi-rv2`](https://github.com/caiovini/barely-nano/tree/orangepi-rv2)** | **Orange Pi RV2** (`SpacemiT K1 / Ky X1`) | 8× RISC-V X60 (`RV64GC` + `V` + `Zba/Zbb/Zbc/Zbs`) | 8 GB LPDDR4X (`5 GB` Bank 1 Buffer Pool) + 1 TB NVMe SSD | Full bare-metal node for Orange Pi RV2 with custom `u-boot-orangepi-k1` (`CONFIG_API=y`), 8-hart SMP pipeline, and I2C OLED telemetry |
| **[`pico-itx-k3`](https://github.com/caiovini/barely-nano/tree/pico-itx-k3)** | **SpacemiT Pico-ITX K3** (`SpacemiT K3`) | RISC-V X100 (`RVA23` + `V` + `Zvkb`) | 32 GB LPDDR5 + NVMe SSD | Bare-metal node architecture tailored for the SpacemiT K3 Pico-ITX platform |

### Switching to a Target Branch

```bash
# For Orange Pi RV2 (SpacemiT K1 / Ky X1, 8 GB RAM):
git checkout orangepi-rv2

# For SpacemiT Pico-ITX K3 (SpacemiT K3, 32 GB RAM):
git checkout pico-itx-k3
```

---

## Core Architecture Overview

Each target branch implements a self-contained bare-metal operating environment tailored to its RISC-V SoC:

1. **S-Mode Bare-Metal Execution (OpenSBI & U-Boot):**
   - Runs in RISC-V Supervisor Mode (`S-mode`) launched directly from U-Boot on top of OpenSBI firmware.
   - Uses SBI `ecall`s for Hart State Management (HSM) and inter-processor interrupts (IPIs), and U-Boot's Standalone Jump Table & Syscall API (`CONFIG_API=y`) for raw Ethernet and NVMe block I/O.
2. **RISC-V Hardware-Accelerated Cryptography:**
   - Leverages the RISC-V Vector Extension (`RVV 1.0`, `Zvl256b`) and Bit-Manipulation extensions (`Zba`, `Zbb`, `Zbc`, `Zbs`, `Zvkb`) for vectorized `Blake2b` hashing, 4-way batch Proof-of-Work validation, and SIMD `Ed25519` signature verification.
   - Uses RISC-V Cache-Block Management and Prefetch extensions (`Zicbom`, `Zicboz`, `Zicbop`, `Zihintpause`) for multi-cluster cache coherency and low-latency B-Tree traversal.
3. **Multi-Hart SMP Pipeline:**
   - Hardware threads (harts) are pinned to dedicated roles across CPU clusters: network/RPC polling, lock-free SIMD Ed25519 signature verification workers, active election consensus & voting, vote processing, single-writer ledger commits, and hardware monitoring.
4. **Bare-Metal NVMe B-Tree + High-DRAM Buffer Pool + Atomic WAL Storage Engine:**
   - Replaces LMDB/RocksDB with a custom **16-shard prefix-partitioned B-Tree engine** operating directly on NVMe sectors.
   - Pairs a multi-gigabyte **High-DRAM Buffer Pool** (using Clock second-chance eviction) and a 64 MB persistent **Bloom filter** with a **256 MB dual-area atomic Write-Ahead Log (WAL)** for power-loss crash recovery and minimal SSD write amplification.
   - Includes a bit-packed compact on-disk block format and Top-256 representative table to maximize DRAM cache residency.
5. **Hardware-Bound Wallet & Representative Key Security (`wallet_crypto`):**
   - Stores wallet seeds and representative private keys in a physically isolated NVMe B-Tree partition with its own atomic redo journal.
   - Encrypts keys on disk using a hardware root key derived from on-chip SoC eFuse / Die ID registers combined with 4,096 rounds of iterated `Blake2b-256`.
6. **Embedded Networking, JSON-RPC, WebSocket & Web Dashboard Stack:**
   - Full `no_std` TCP/IP/DNS/ICMP stack (`smoltcp`) driving:
     - **Port `7075`:** Live Nano P2P peering and bootstrap.
     - **Port `7076`:** HTTP JSON-RPC server (`rsnano_rpc_messages`, `rsnano_rpc_handler`, `rsnano_rpc_server`).
     - **Port `7078`:** Real-time WebSocket event broadcast server (`rsnano_websocket_server`).
     - **Port `7080`:** Embedded single-page Web Dashboard with live node metrics, representative wallet management, Point-of-Sale (PoS) stealth QR code generation, and an in-browser WebAssembly (`nano-stress-wasm`) block generator and stress tester.

---

## Authors & Credits

`barely-nano` is a bare-metal `no_std` RISC-V fork and hardware port of [RsNano (`rsnano-node/rsnano-node`)](https://github.com/rsnano-node/rsnano-node).

- **Caio Vinicius** (`<cvini@proton.me>`) — Bare-metal `no_std` RISC-V architecture, SMP kernel, RVV 1.0 / Zbb hardware-accelerated cryptography, 16-shard NVMe B-Tree + DRAM Buffer Pool + WAL storage engine, U-Boot `smoltcp` networking, and embedded RPC/WebSocket/PoS stack.
- **Gustav Schauwecker** (`<service@simpago.com>`) — Creator and lead author of the original [RsNano](https://github.com/rsnano-node/rsnano-node) Rust implementation of the Nano node.
- **Gemini 4 Carbon** — Co-author on the `no_std` migration, storage engine, and RISC-V systems engineering.
