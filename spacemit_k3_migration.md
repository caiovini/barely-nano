# SpacemiT K3 (32 GB LPDDR5) Migration Guide & Checklist

This document tracks the architecture, compile-time Cargo feature scaffolding (`--features k1` vs. `--features k3`), and hardware bring-up checklist for running `barely-nano` on a **SpacemiT K3 (32 GB LPDDR5)** board.

---

## 1. What Has Already Been Scaffolded

The codebase now supports compiling for either **SpacemiT K1 (8 GB LPDDR4X)** or **SpacemiT K3 (32 GB LPDDR5)** via Cargo features:

### Build Targets ([`Makefile`](file:///mnt/data/rust-workspace/barely-nano/Makefile))
- **SpacemiT K1 (default, Orange Pi RV2 8 GB LPDDR4X):**
  ```bash
  make build
  # or explicitly:
  make build-k1
  ```
- **SpacemiT K3 (32 GB LPDDR5):**
  ```bash
  make build-k3
  ```

### SoC Configuration Module ([`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs))
All platform-specific constants are centralized in [`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs) and selected via `#[cfg(feature = "k3")]` vs. `#[cfg(not(feature = "k3"))]`:

| Constant | `k1` (8 GB LPDDR4X) | `k3` (32 GB LPDDR5) | Purpose |
| :--- | :--- | :--- | :--- |
| `SOC_NAME` | `"SpacemiT K1 / Ky X1 (8 GB LPDDR4X)"` | `"SpacemiT K3 (32 GB LPDDR5)"` | Boot banner & storage log identification |
| `MAX_HARTS` | `8` | `16` | Maximum hardware harts supported |
| `HIGH_DRAM_BASE` | `0x1_0000_0000` | `0x1_0000_0000` | Physical base of Upper DRAM Bank 1 |
| `HIGH_DRAM_TARGET_FRAMES` | `81,920` (`320 MB/shard` = **`5.00 GB`**) | `409,600` (`1,600 MB/shard` = **`25.00 GB`**) | Target 4 KB frames per shard in DRAM Bank 1 before Clock eviction |
| `HIGH_DRAM_MAX_FRAMES` | `86,016` (`336 MB/shard` = **`5.25 GB`**) | `425,984` (`1,664 MB/shard` = **`26.00 GB`**) | Hard upper bound on 4 KB frames per shard |
| `CLINT_FREQ` | `24_000_000` (24 MHz) | `24_000_000` (24 MHz) | `rdtime` timer frequency |
| `UART0_BASE` | `0xD401_7000` | `0xD401_7000` *(verify on K3)* | Debug UART0 MMIO base |
| `WDT_BASE` | `0xD408_0000` | `0xD408_0000` *(verify on K3)* | SoC hardware watchdog MMIO base |
| `I2C8_BASE` | `0xD401_D800` | `0xD401_D800` *(verify on K3)* | PMIC power I2C bus MMIO base |
| `I2C4_BASE` | `0xD401_2800` | `0xD401_2800` *(verify on K3)* | External GPIO header I2C4 MMIO base for OLED display |

---

## 2. Checklist of Items to Verify & Modify When the K3 Board Arrives

When the SpacemiT K3 hardware and its vendor U-Boot / Device Tree (`k3-*.dts`) are available, verify and update the following 7 items in order:

### Item 1: Custom U-Boot for K3 (`CONFIG_API=y` & Ethernet DMA Rings)
- **Why:** `barely-nano` relies on U-Boot's Standalone API (`CONFIG_API=y`) for raw Ethernet packet TX/RX and NVMe sector reads/writes.
- **What to do in K3's U-Boot repository (mirroring [`u-boot-orangepi-k1`](https://github.com/caiovini/u-boot-orangepi-k1.git)):**
  1. Enable `CONFIG_API=y` in K3's `defconfig` (same as `configs/x1_defconfig:298` in `u-boot-orangepi-k1`).
  2. In K3's Ethernet driver (`drivers/net/...`), increase `EQOS_DESCRIPTORS_TX` and `EQOS_DESCRIPTORS_RX` to `2048` (same as `drivers/net/ky/x1_emac.c:97-98` in `u-boot-orangepi-k1`).
  3. Verify `struct global_data` (`gd_t`) offset of `jt` (Jump Table pointer) in [`main/src/entry.S:71`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S#L71) (`ld s11, 264(s10)`). Note that [`common/src/uboot.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/uboot.rs) also scans dynamically around `gp` for the Jump Table magic signature, so minor `gd_t` layout shifts are already auto-detected.

### Item 2: Verify Early UART0 MMIO Address & `timebase-frequency`
- **Where:**
  - [`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs) (`UART0_BASE`, `CLINT_FREQ`)
  - [`main/src/entry.S:94`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S#L94) (`early_trap` assembly UART address `lui t0, 0xd4017`)
- **What to check in K3 DTB (`k3-*.dts`):**
  1. Check the `serial@d4017000` (or equivalent `uart0`) node reg base in the K3 device tree. If K3 keeps `0xD401_7000`, no change is needed. If it moved, update `UART0_BASE` in [`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs) and `early_trap` in [`main/src/entry.S`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S).
  2. Check `cpus { timebase-frequency = <...>; }` in the K3 device tree (24 MHz = `24000000` on SpacemiT). Update `CLINT_FREQ` in [`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs) if different.

### Item 3: Verify DRAM Physical Memory Map (32 GB LPDDR5)
- **Where:**
  - [`common/src/soc.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/soc.rs) (`HIGH_DRAM_BASE`, `HIGH_DRAM_TARGET_FRAMES`, `HIGH_DRAM_MAX_FRAMES`)
  - [`linker-smode.ld`](file:///mnt/data/rust-workspace/barely-nano/linker-smode.ld)
- **What to check in U-Boot (`bdinfo` command on K3):**
  1. Run `bdinfo` at the K3 U-Boot prompt (`=> bdinfo`) to inspect `DRAM bank #0` and `DRAM bank #1` start addresses and sizes:
     - On K1 (8 GB), Bank 0 is `0x0000_0000 .. 0x8000_0000` (2 GB) and Bank 1 is `0x1_0000_0000 .. 0x2_8000_0000` (6 GB).
     - On K3 (32 GB), verify whether Bank 1 starts at `0x1_0000_0000` (with 30 GB at `0x1_0000_0000 .. 0x8_8000_0000`).
  2. Note that [`init_shards()`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs#L580-L590) already uses trap-safe `probe_rw_u64()` to verify the entire `HIGH_DRAM_BASE .. high_dram_end` window at boot before enabling the 25.00 GB Buffer Pool.
  3. On K3, `FrameMeta` (`425,984 × 24 B × 16 shards`) uses `163.5 MB` of the Bank 0 heap (`1,456 MB`), leaving `~1.1 GB` free in Bank 0 while hosting **`25.00 GB` of 4 KB B-Tree pages in Bank 1**.

### Item 4: Multi-Hart Count & SMP Worker Dispatch (8 vs. 16 Harts)
- **Where:**
  - [`main/src/entry.S:28-32`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S#L28-L32) & [`main/src/entry.S:74-78`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S#L74-L78)
  - [`main/src/bin.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs) (hart waking loop and `rust_main` hart role dispatch)
  - [`common/src/storage/mod.rs:53-66`](file:///mnt/data/rust-workspace/barely-nano/common/src/storage/mod.rs#L53-L66) (`LOCK_WAIT`, `LOCK_HOLD`, `LOCK_ACQ` per-hart arrays)
- **What to modify if K3 has > 8 harts (e.g., 16 harts):**
  1. In [`main/src/entry.S:29`](file:///mnt/data/rust-workspace/barely-nano/main/src/entry.S#L29), change `li t0, 7; and t1, a0, t0` to mask with `15` (`0xF`), and adjust per-hart stack size (`16 MB × 16 harts = 256 MB`, shift `24` instead of `25`).
  2. In [`main/src/bin.rs`](file:///mnt/data/rust-workspace/barely-nano/main/src/bin.rs), wake harts `1..MAX_HARTS` via `sbi::hart_start` and route `hart_id >= 8` into the lock-free SIMD Ed25519 signature verification + message processor worker loop (same role as Harts 2 & 3).
  3. Check whether Hart 0 on K3 can be used normally or should remain skipped like on K1.

### Item 5: Watchdog & PMIC I2C Servicing
- **Where:**
  - [`common/src/watchdog.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/watchdog.rs)
  - [`common/src/pmic_wdt.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/pmic_wdt.rs)
- **What to check:**
  1. Check K3's U-Boot boot log for `WDT: Started ...` messages.
  2. Verify the SoC WDT base (`WDT_BASE`) and PMIC model/I2C bus (`SPM8821` or newer PMIC on K3) in K3's U-Boot tree (`drivers/watchdog/` and `include/power/`).

### Item 6: RVA23 / X100 Compiler Target Features (`Zvkb`)
- **Where:** [`Makefile`](file:///mnt/data/rust-workspace/barely-nano/Makefile) (`build-k3` target) & [`nullables/sha`](file:///mnt/data/rust-workspace/barely-nano/nullables/sha)
- **What to check:**
  - SpacemiT K3's X100 cores implement RVA23, which includes `+zvkb` (Vector Cryptography Bit-Manipulation: native `vror.vv` / `vror.vi` vector rotates and `vandn.vv`).
  - Once on K3 hardware, add `,+zvkb` to `RUSTFLAGS` in `build-k3` to replace multi-instruction shift-XOR vector rotates in Blake2b with single-instruction `vror.vi`.

### Item 7: I2C OLED Display Pinmux (`I2C4`)
- **Where:** [`common/src/oled.rs`](file:///mnt/data/rust-workspace/barely-nano/common/src/oled.rs)
- **What to check:**
  - Verify the 26-pin / 40-pin GPIO header I2C bus number and pinmux register offsets on the K3 board (`I2C4_BASE`, `PINCTRL_BASE`, `GPIO_BASE`).
