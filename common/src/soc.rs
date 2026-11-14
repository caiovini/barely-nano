//! SoC-specific hardware constants for SpacemiT K3 Pico-ITX (16 GB LPDDR5, 8× X100 + 8× A100).

/// Human-readable SoC identifier logged at boot.
pub const SOC_NAME: &str = "SpacemiT K3 Pico-ITX (16 GB LPDDR5, 8x X100 + 8x A100)";

/// Maximum hardware hart count supported by per-hart arrays and SMP dispatch
/// (Cluster 0..1: 8× X100 @ 2.4 GHz VLEN=256; Cluster 2..3: 8× A100 @ 2.0 GHz VLEN=1024).
pub const MAX_HARTS: usize = 16;

/// Base physical address of the 3.00 GB global allocator heap (`0x1_1800_0000 .. 0x1_D800_0000`).
pub const HEAP_START: usize = 0x1_1800_0000;

/// End physical address of the 3.00 GB global allocator heap (`0x1_D800_0000`).
pub const HEAP_END: usize = 0x1_D800_0000;

/// Base physical address of the 64-Shard B-Tree Buffer Pool in K3 LPDDR5 DRAM (`0x1_E000_0000 .. 0x4_E000_0000`).
/// Located above the kernel (`0x1_0400_0000`), 256 MB per-hart stacks (`0x1_0800_0000`),
/// and 3.0 GB global heap (`0x1_1800_0000 .. 0x1_D800_0000`), while leaving the top 512 MB
/// (`0x4_E000_0000 .. 0x5_0000_0000`) untouched for U-Boot's relocated monitor, malloc arena,
/// PCIe DMA rings, and SimpleFB HDMI framebuffer.
pub const HIGH_DRAM_BASE: usize = 0x1_E000_0000;

/// Target resident 4 KB frames per shard in K3 LPDDR5 DRAM:
/// `49,152 * 4 KB = 192 MB/shard` = `12,288 MB (12.00 GB)` across 64 shards.
pub const HIGH_DRAM_TARGET_FRAMES: usize = 49_152;

/// Maximum 4 KB frames per shard in K3 LPDDR5 DRAM:
/// `51,200 * 4 KB = 200 MB/shard` = `12,800 MB (12.50 GB)` across 64 shards.
pub const HIGH_DRAM_MAX_FRAMES: usize = 51_200;

/// CLINT `rdtime` counter frequency in Hz on SpacemiT K3 (`24 MHz` `vctcxo_24m`).
pub const CLINT_FREQ: u64 = 24_000_000;

/// SpacemiT K3 Debug UART0 MMIO base address (`serial@d4017000`).
pub const UART0_BASE: usize = 0xD401_7000;

/// SpacemiT K3 SoC Watchdog MMIO base address (probed trap-safely at boot).
pub const WDT_BASE: usize = 0xD408_0000;

/// SpacemiT K3 PWR I2C8 MMIO base address (`i2c@d401d800`, connected to SpacemiT P1 PMIC at `0x41`).
pub const I2C8_BASE: usize = 0xD401_D800;

/// SpacemiT K3 I2C4 MMIO base address (`i2c@d4012800`, HDMI DDC/EDID bus).
pub const I2C4_BASE: usize = 0xD401_2800;
