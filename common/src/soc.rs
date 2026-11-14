//! SoC-specific hardware constants for SpacemiT K1 / Ky X1 (Orange Pi RV2, 8× X60 harts, 8 GB LPDDR4X).

/// Human-readable SoC identifier logged at boot.
pub const SOC_NAME: &str = "SpacemiT K1 / Ky X1 (8 GB LPDDR4X)";

/// Maximum hardware hart count on SpacemiT K1 (8× X60 harts across 2 clusters).
pub const MAX_HARTS: usize = 8;

/// Base physical address of Ky X1 DRAM Bank 1 (6 GB region: `0x1_0000_0000 .. 0x2_8000_0000`).
pub const HIGH_DRAM_BASE: usize = 0x1_0000_0000;

/// Target resident 4 KB frames per shard in DRAM Bank 1:
/// `81,920 * 4 KB = 320 MB/shard` = `5,120 MB (5.00 GB)` across 16 shards.
pub const HIGH_DRAM_TARGET_FRAMES: usize = 81_920;

/// Maximum 4 KB frames per shard in DRAM Bank 1:
/// `86,016 * 4 KB = 336 MB/shard` = `5,376 MB (5.25 GB)` across 16 shards.
pub const HIGH_DRAM_MAX_FRAMES: usize = 86_016;

/// CLINT `rdtime` counter frequency in Hz on SpacemiT K1 (24 MHz).
pub const CLINT_FREQ: u64 = 24_000_000;

/// SpacemiT K1 UART0 MMIO base address.
pub const UART0_BASE: usize = 0xD401_7000;

/// SpacemiT K1 SoC Watchdog MMIO base address.
pub const WDT_BASE: usize = 0xD408_0000;

/// SpacemiT K1 PWR I2C8 MMIO base address (connected to SPM8821 PMIC).
pub const I2C8_BASE: usize = 0xD401_D800;

/// SpacemiT K1 I2C4 MMIO base address (26-pin GPIO header Pins 3 & 5).
pub const I2C4_BASE: usize = 0xD401_2800;
