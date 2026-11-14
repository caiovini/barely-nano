# SpacemiT K3 Pico-ITX (16 GB LPDDR5) Bare-Metal Architecture (`pico-itx-k3`)

This document describes the bare-metal `#![no_std]` Rust execution architecture for `barely-nano` on the **SpacemiT K3 Pico-ITX (16 GB 64-bit LPDDR5 @ 6400 MT/s, PCIe Gen3 ×4 NVMe, Realtek RTL8127 10GbE SFP+, Waveshare 5-inch HDMI SimpleFB)** implemented on the `pico-itx-k3` branch.

---

## 1. Hardware & Memory Map (`16 GB LPDDR5` at `0x1_0000_0000 .. 0x5_0000_0000`)

Unlike the SpacemiT K1 (`0x0000_0000` Low DRAM + `0x1_0000_0000` High DRAM), the SpacemiT K3 maps all **16 GB of LPDDR5** contiguously starting at `0x1_0000_0000`:

| Physical Address Range | Size | Allocation & Purpose |
| :--- | :--- | :--- |
| `0x1_0000_0000 .. 0x1_0020_0000` | `2 MB` | OpenSBI M-mode firmware (`FW_TEXT_START = 0x1_0000_0000`) |
| `0x1_0020_0000 .. 0x1_0400_0000` | `62 MB` | U-Boot (`CONFIG_TEXT_BASE = 0x1_0020_0000`), DTB (`0x1_1F00_0000`), and `simplefb` HDMI framebuffer (`800×480` RGB565/XRGB8888) |
| `0x1_0400_0000 .. 0x1_0800_0000` | `64 MB` | `barely-nano.bin` S-mode kernel (`.text`, `.rodata`, `.data`, `.bss` — `linker-smode.ld`) |
| `0x1_0800_0000 .. 0x1_1800_0000` | `256 MB` | **16-Hart Kernel Stacks** (`16 harts × 16 MB/hart`, `entry.S`: `slli t1, t1, 24`) |
| `0x1_1800_0000 .. 0x1_D800_0000` | **`3,072 MB` (`3.00 GB`)** | **Global Kernel Heap (`_heap_start .. HEAP_END`)**: `FrameMeta` + hash tables for 64 shards (`~110 MB`), 64 MB `BlockBloom`, `ActiveElections`, `VoteCache`, and `smoltcp` 10GbE socket buffers |
| `0x1_D800_0000 .. 0x1_E000_0000` | `128 MB` | Guard / DMA bounce & U-Boot upper relocation reserve |
| `0x1_E000_0000 .. 0x4_E000_0000` | **`12,288 MB` (`12.00 GB`)** | **64-Shard B-Tree Buffer Pool (`HIGH_DRAM_BASE = 0x1_E000_0000`)**: `49,152` × 4 KB frames per shard (`192 MB/shard × 64 shards = 12.00 GB` target; `51,200` frames = `12.50 GB` max) |
| `0x4_E000_0000 .. 0x5_0000_0000` | `512 MB` | Top-of-DRAM safety margin (`0x5_0000_0000` = 16 GB end) |

---

## 2. 16-Hart Hybrid Topology (`8× X100 @ 2.4 GHz` + `8× A100 @ 2.0 GHz`)

The SpacemiT K3 integrates 16 RVA23 harts across 4 clusters:
- **Cluster 0 — X100 Harts `0..3` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb`):**
  - **Hart 0 (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and cooperative async executor tasks (`WORKER_HARTS`).
  - **Hart 1 (`Maint+Net`):** Boot initialization, 10GbE RTL8127 `smoltcp` network polling, HTTP JSON-RPC (`7076`), WebSocket (`7078`), Live Web Dashboard (`7080`), and watchdog kicking.
  - **Harts 2 & 3 (`Crypto-X100`):** 4-way `VLEN=256` (`Zvkb` native `vror.vi`) Ed25519 signature verification (`signature_checker.help_verify()`) + inbound `MessageProcessor::run_batch()`.
- **Cluster 1 — X100 Harts `4..7` (`2.4 GHz`, 4-issue OoO, `VLEN=256`, `Zvkb`):**
  - **Hart 4 (`Consensus`):** Drives `aec_voter`, `aec_ticker`, `election_schedulers`, `backlog_scan`, `bootstrap_server`, and async executor tasks (`WORKER_HARTS`).
  - **Hart 5 (`Ledger-B`):** Parallel B-Tree mutations for **Shards `21..42`** (`Ledger::try_run_ledger_b_job`), WAL segments, and `unchecked` dependency drain.
  - **Hart 6 (`Ledger-A`):** Primary `block_processor` batch coordinator (`FAST_BATCH_SLOTS = 2048`), parallel B-Tree mutations for **Shards `0..21`**, WAL checkpointing, and `confirming_set` cementation.
  - **Hart 7 (`Ledger-C + Storage`):** Parallel B-Tree mutations for **Shards `42..64`** (`Ledger::try_run_ledger_c_job`), online representative weight calculation, NVMe Free-List GC/pruning, and async tasks.
- **Clusters 2 & 3 — A100 Harts `8..15` (`2.0 GHz`, `VLEN=1024`, `Zvkb`):**
  - **Harts `8..14` (`Crypto-A100`):** 7 dedicated **16-way `VLEN=1024` SIMD** Ed25519 + Blake2b batch verifiers (`verify_batch_16` / `compress_16_blocks`, auto-selected at runtime via `csrr vlenb >= 128`) + inbound `MessageProcessor::run_batch()` drainers.
  - **Hart `15` (`HDMI+Mon`):** Waveshare 5-inch HDMI SimpleFB graphical dashboard (`common/src/hdmi_fb.rs`) and 15-second hardware telemetry & lock-contention profiler.

---

## 3. 64-Shard NVMe B-Tree + 12 GB Buffer Pool + Zero-Copy DMA

1. **64-Shard Prefix Partitioning (`SHARD_COUNT = 64`):**
   - [`get_shard_index`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs) routes keys using the top 6 bits of `key[1]` (`(key[1] >> 2) as usize`, `0..64`).
   - Reduces per-shard B-Tree depth so interior branch pages for all 64 shards stay hot in L1/L2 cache and cuts shard lock contention by `4×`.
   - Both host tools ([`tools/lmdb-to-nvme`](file:///mnt/data/rust-workspace/barely-nano/tools/lmdb-to-nvme/src/main.rs) and [`tools/nvme-snapshot`](file:///mnt/data/rust-workspace/barely-nano/tools/nvme-snapshot/src/main.rs)) use `SHARD_COUNT = 64` and `>> 2` routing.
2. **Zero-Copy 64-Bit NVMe PRP DMA & Deep Queueing (`/mnt/data/rust-workspace/u-boot`):**
   - U-Boot exports `blk_get_capacity`, `blk_read_lba`, and `blk_write_lba` at JumpTable indices `56..58` (`0x1c0..0x1d0`), configured with `NVME_Q_DEPTH = 64` and `MAX_PRP_POOL = 4096`.
   - All 4 KB frames in `BufferPool` (`0x1_E000_0000+`) and `AlignedPage` stack buffers are 64-byte cache-line aligned, bypassing bounce buffers in `StorageContext::read_pages` / `write_pages`.
3. **Realtek RTL8127 10GbE SFP+ Driver (`/mnt/data/rust-workspace/u-boot/drivers/net/rtl8169.c`):**
   - Upgraded to `NUM_TX_DESC = 64` and `NUM_RX_DESC = 512`, full 64-bit PCIe DMA descriptor addressing (`>= 0x1_0000_0000`), batched 4-descriptor cache-line RX re-arming, and zero-`udelay` TX doorbell polling.

---

## 4. Build & Validation Commands

```bash
# 1. Build U-Boot (/mnt/data/rust-workspace/u-boot)
./build_u-boot.sh

# 2. Build bare-metal SpacemiT K3 firmware image (barely-nano.bin)
make build

# 3. Run unit & integration test suites
make run-tests

# 4. Build host ledger migration & snapshot utilities (64 shards)
make build-lmdb-to-nvme
make build-nvme-snapshot
```
