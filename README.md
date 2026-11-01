# Bare-Metal RISC-V Nano Node (barely-nano)

This project is a bare-metal `no_std` RISC-V Nano cryptocurrency node designed to run directly on the **Orange Pi RV2 (SpacemiT K1 / Ky X1 SoC)** with 8× X60 RISC-V harts, 8 GB RAM, and a 1 TB NVMe SSD.

It establishes a self-contained operating environment on top of OpenSBI and U-Boot (`CONFIG_API=y`), complete with a heap allocator, multi-core exception handling, hardware-accelerated cryptography, a 16-shard B-Tree + 5 GB DRAM Bank 1 Buffer Pool + atomic Write-Ahead Log (WAL) storage engine on NVMe, a full TCP/IP networking stack, an HTTP JSON-RPC server, and a real-time WebSocket server.

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
- **Bit-Manipulation (`Zbb`, `Zba`, `Zbs`, `Zbc`):** Scalar bit-manipulation, shifted address generation, single-bit operations, and carry-less multiplication (compiling 64-bit Blake2b rotations to single `rori` instructions).
- **Cache-Block Prefetch (`Zicbop`):** Inline `prefetch.r` hints during B-Tree binary searches to hide DRAM latency.
- **Cache-Block Zero (`Zicboz`):** Fast cache-line zeroing (`cbo.zero`).
- **Pause Hint (`Zihintpause`):** Adaptive `pause` backoff in spinloops to reduce interconnect contention.
- **Cache-Block Management (`Zicbom`):** Explicit cache maintenance (`cbo.clean`, `cbo.inval`) for cross-cluster coherency between Cluster 0 (Harts 0–3) and Cluster 1 (Harts 4–7).

### 6. U-Boot Networking, RPC & WebSocket Stack
- **Hardware Probing:** Discovers and initializes the board's Ethernet controller via the U-Boot Standalone API (`UbootNetDevice`) with TX stall recovery.
- **TCP/IP Stack (`smoltcp` 0.12):** Full-featured `no_std` TCP/IP/DNS/ICMP stack integrated with the cooperative async executor.
- **Nano Services:**
  - **Port `7075` (Peering):** Live Nano P2P protocol peering and bootstrap (`96` concurrent peers).
  - **Port `7076` (HTTP JSON-RPC):** Full `no_std` Nano RPC command handler (`rsnano_rpc_messages`, `rsnano_rpc_handler`, `rsnano_rpc_server`).
  - **Port `7078` (WebSocket Server):** Full `no_std` WebSocket server and client (`rsnano_websocket_messages`, `rsnano_websocket_client`, `rsnano_websocket_server`) streaming live node events (`confirmation`, `vote`, `started_election`, `stopped_election`, `telemetry`, `bootstrap`, `new_unconfirmed_block`, etc.).

### 7. Multi-Hart SMP Architecture
Each active hart on the SpacemiT K1 runs a dedicated role (`main/src/bin.rs`):
- **Hart 0:** Reserved / skipped (OpenSBI HSM quirk on SpacemiT K1).
- **Hart 1 (`Maint+Net`):** Boot initialization, `smoltcp` network polling, HTTP JSON-RPC server (`7076`), gateway ICMP keepalive, and watchdog kicking.
- **Hart 2 & Hart 3 (`Crypto-1` / `Crypto-2`):** Dedicated lock-free SIMD Ed25519 signature verification workers (`signature_checker.help_verify()`) and inbound `MessageProcessor::run_batch()` drainers.
- **Hart 4 (`Consensus`):** Drives `aec_voter` and `aec_ticker` every 300 ms and runs async executor tasks.
- **Hart 5 (`VoteProc`):** Runs `vote_cache_processor`, `vote_processor`, and async executor tasks.
- **Hart 6 (`Ledger+Boot`):** Single-writer ledger commit pipeline (`block_processor`, `unchecked`, `confirming_set`, `election_schedulers`, `backlog_scan`), atomic WAL checkpoints, and `bootstrap_server`.
- **Hart 7 (`Storage+Mon`):** Online weight calculation, 2-second I2C OLED status updates, 15-second telemetry/profiling reports, and async executor tasks.

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

1. **Physical Layer (`StorageBackend::UBoot`):** Transfers raw 4 KB pages (`8 × 512 B` sectors, batched up to 32 pages / 128 KB per command) over U-Boot's block syscall interface to the NVMe SSD using a 64-byte-aligned low-DRAM DMA bounce buffer when transferring frames from High DRAM Bank 1. (`STORAGE_BASE_LBA = 1,048,576` leaves a 512 MB safety margin at the start of the drive, with a 64 MB persistent Bloom filter at `BLOOM_HEADER_LBA = 524,288`.)
2. **5 GB High-DRAM Buffer Pool Manager (`common/src/storage/bpm.rs`):** Manages `81,920` × 4 KB frames per shard (`320 MB/shard` = `5.00 GB` total) in Ky X1 DRAM Bank 1 (`0x1_0000_0000`), paired with compact 24-byte `FrameMeta` entries in Bank 0 L2-friendly memory, an open-addressed page hash table, and a **Clock (second-chance)** eviction policy. Dirty frames are **never written during eviction**, preventing partial cross-shard updates from reaching disk.
3. **Atomic Dual-Area Write-Ahead Log (`common/src/storage/wal.rs`):** Reserves `256 MB` (`2 × 128 MB` alternating areas) on NVMe. Between block batches on Hart 6 (the single ledger writer), `wal::checkpoint()` snapshots dirty pages across all 16 shards, writes `[descriptor pages][data pages][commit page]` sequentially with a 64-bit checksum and boot epoch, and then writes the pages to their home shard locations sorted by LBA. On boot, `wal::recover()` idempotently replays the latest committed batch.
4. **16-Shard B-Tree Engine (`common/src/storage/btree.rs`):** Prefix-partitioned across 16 shards by the high nibble of `key[1]`, supporting `Zicbop`-prefetched binary search, proactive-split inserts, sibling borrow/merge deletes with embedded free-list page reclamation, and stack-cached range iterators.

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
- **Expanded Ethernet DMA Rings (`2048` RX / `2048` TX descriptors)** (`drivers/net/ky/x1_emac.c`) — Prevents packet drops under high P2P block/vote burst load.
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
```

### Representative Discovery on Bare-Metal

In a bare-metal environment with an incomplete ledger during initial sync, public representatives may appear to have 0 weight locally until their account chains are synced. Setting `representative_vote_weight_minimum = "0"` under `[node]` in [`config.toml`](file:///mnt/data/rust-workspace/barely-nano/config.toml) allows observing all voting representatives during initial sync.

## Network Stress Testing (`tools/nano-stress`)

The repository provides a high-throughput stress testing tool (`tools/nano-stress`) to test node stability, B-Tree storage performance, and queue behavior under heavy load by generating cryptographically valid blocks and streaming them directly over TCP to port **7075**.

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
- [`tools/`](file:///mnt/data/rust-workspace/barely-nano/tools) — Host-side utilities: [`nano-stress`](file:///mnt/data/rust-workspace/barely-nano/tools/nano-stress) (P2P block streamer) and [`ws_subscribe.py`](file:///mnt/data/rust-workspace/barely-nano/tools/ws_subscribe.py) (WebSocket topic subscriber).
- [`u-boot-orangepi-k1/`](file:///mnt/data/rust-workspace/barely-nano/u-boot-orangepi-k1) — Git submodule pointing to [`caiovini/u-boot-orangepi-k1`](https://github.com/caiovini/u-boot-orangepi-k1.git) (`CONFIG_API=y` U-Boot tree and `prebuilt/` bootloader images for Orange Pi RV2).
- [`linker-smode.ld`](file:///mnt/data/rust-workspace/barely-nano/linker-smode.ld) — S-mode linker script mapping physical memory layout for the Orange Pi RV2.
