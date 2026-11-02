//! Hardware-Bound & Password-Derived Wallet Key Encryption (`wallet_crypto`)
//!
//! Combines:
//! 1. **SoC Hardware Root Key (Ky X1 eFuse / Die ID):** Extracted at boot from the SpacemiT K1 /
//!    Ky X1 on-chip 4K-bit eFuse banks (`0xD401_F000`), Chip ID Unit (`0xD428_2C00`), EMAC0
//!    factory MAC registers (`0xCAC8_0040`), and U-Boot's eFuse-derived `serial#` / `ethaddr` /
//!    optional `wallet_hw_key` environment variables.
//! 2. **Iterated `Blake2b-256` Key Derivation (4,096 rounds):** Mixes the SoC hardware root key,
//!    a per-wallet 256-bit random salt, and the wallet password (`""` by default for unattended
//!    reboot auto-voting, or a custom passphrase set via `wallet_rekey`).
//! 3. **Domain-Separated `Blake2b-256` Stream Cipher + Authentication Tag:** Encrypts the wallet
//!    `seed` and every account `priv_key` before they touch the NVMe SSD (`WALLET_SHARD`), and
//!    stores a 256-bit authentication tag (`check_tag`) instead of ever persisting the plaintext
//!    password on disk.

use rsnano_sha::blake2b::Blake2bHasher;
use spin::Mutex;

const KDF_ROUNDS: u32 = 4096;

static HW_WALLET_KEY: Mutex<Option<[u8; 32]>> = Mutex::new(None);

#[cfg(target_arch = "riscv64")]
fn read_cstr_bounded(ptr: *const u8, max_len: usize) -> &'static [u8] {
    if ptr.is_null() {
        return &[];
    }
    let mut len = 0usize;
    unsafe {
        while len < max_len && *ptr.add(len) != 0 {
            len += 1;
        }
        core::slice::from_raw_parts(ptr, len)
    }
}

fn derive_soc_hardware_key() -> [u8; 32] {
    let mut hasher = Blake2bHasher::new(32);
    hasher.update(b"BARELY_NANO_KY_X1_EFUSE_HW_KEY_V1");

    #[cfg(target_arch = "riscv64")]
    {
        // 1. Probe Ky X1 4K-bit eFuse shadow registers (16 banks × 32 bytes = 512 bytes)
        //    and Chip ID Unit (CIU) using trap-safe S-mode loads so PMP restrictions never fault.
        const KY_X1_EFUSE_BASE: usize = 0xD401_F000;
        const KY_X1_CIU_BASE: usize = 0xD428_2C00;
        const KY_X1_EMAC0_MAC_HI: usize = 0xCAC8_0040;
        const KY_X1_EMAC0_MAC_LO: usize = 0xCAC8_0044;

        let mut efuse_words = 0usize;
        for off in (0..0x200usize).step_by(4) {
            if let Some(w) = crate::trap::probe_read_u32(KY_X1_EFUSE_BASE + off) {
                hasher.update(&w.to_le_bytes());
                if w != 0 {
                    efuse_words += 1;
                }
            } else {
                break;
            }
        }

        for off in (0..0x10usize).step_by(4) {
            if let Some(w) = crate::trap::probe_read_u32(KY_X1_CIU_BASE + off) {
                hasher.update(&w.to_le_bytes());
            } else {
                break;
            }
        }

        if let Some(mac_hi) = crate::trap::probe_read_u32(KY_X1_EMAC0_MAC_HI) {
            hasher.update(&mac_hi.to_le_bytes());
        }
        if let Some(mac_lo) = crate::trap::probe_read_u32(KY_X1_EMAC0_MAC_LO) {
            hasher.update(&mac_lo.to_le_bytes());
        }

        // 2. Mix U-Boot eFuse-derived hardware identifiers (`serial#`, `ethaddr`, and optional
        //    operator-configured `wallet_hw_key` in SPI NOR flash).
        for var in ["serial#", "ethaddr", "wallet_hw_key"] {
            if let Some(ptr) = crate::uboot::env_get(var) {
                let bytes = read_cstr_bounded(ptr, 128);
                if !bytes.is_empty() {
                    hasher.update(var.as_bytes());
                    hasher.update(bytes);
                }
            }
        }

        let _ = efuse_words;
    }

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..32]);
    out
}

/// Returns the cached 256-bit SoC hardware root key derived from the Ky X1 eFuse / hardware IDs.
pub fn hardware_wallet_key() -> [u8; 32] {
    let mut guard = HW_WALLET_KEY.lock();
    if let Some(k) = *guard {
        return k;
    }
    let k = derive_soc_hardware_key();
    *guard = Some(k);
    k
}

/// Overrides the cached SoC hardware root key (used by unit tests to simulate moving an NVMe SSD
/// to a different OrangePi RV2 board with a different Ky X1 SoC eFuse key).
pub fn set_hardware_wallet_key(key: [u8; 32]) {
    *HW_WALLET_KEY.lock() = Some(key);
}

/// Resets the cached SoC hardware root key back to the default derived key.
pub fn reset_hardware_wallet_key() {
    *HW_WALLET_KEY.lock() = None;
}

/// Derives the 256-bit wallet wrapping key from:
/// - The Ky X1 SoC hardware eFuse root key (`hardware_wallet_key()`),
/// - The wallet's 256-bit random `salt`, and
/// - The operator's `password` (`""` by default for unattended reboot auto-voting, or a custom
///   passphrase set via `wallet_rekey`).
pub fn derive_wrapping_key(salt: &[u8; 32], password: &str) -> [u8; 32] {
    let hw_key = hardware_wallet_key();

    let mut h = Blake2bHasher::new(32);
    h.update(b"BARELY_NANO_WALLET_KDF_V2");
    h.update(&hw_key);
    h.update(salt);
    h.update(&(password.len() as u32).to_le_bytes());
    h.update(password.as_bytes());
    let first = h.finalize();

    let mut key = [0u8; 32];
    key.copy_from_slice(&first[..32]);

    for round in 0..KDF_ROUNDS {
        let mut r = Blake2bHasher::new(32);
        r.update(&key);
        r.update(salt);
        r.update(&round.to_le_bytes());
        let d = r.finalize();
        key.copy_from_slice(&d[..32]);
    }
    key
}

/// Computes the 256-bit authentication tag verifying both the SoC eFuse key and the wallet password.
pub fn compute_check_tag(
    wrapping_key: &[u8; 32],
    salt: &[u8; 32],
    representative: &[u8; 32],
    encrypted_seed: &[u8; 32],
    has_password: bool,
) -> [u8; 32] {
    let mut h = Blake2bHasher::new(32);
    h.update(b"BARELY_NANO_WALLET_VERIFY_V2");
    h.update(wrapping_key);
    h.update(salt);
    h.update(representative);
    h.update(encrypted_seed);
    h.update(&[u8::from(has_password)]);
    let d = h.finalize();
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&d[..32]);
    tag
}

/// Constant-time verification of the 256-bit wallet authentication tag.
#[inline]
pub fn verify_check_tag(expected: &[u8; 32], actual: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= expected[i] ^ actual[i];
    }
    diff == 0
}

/// Encrypts or decrypts the 32-byte wallet `seed` under `(wrapping_key, salt)`.
pub fn crypt_seed(wrapping_key: &[u8; 32], salt: &[u8; 32], seed_in: &[u8; 32]) -> [u8; 32] {
    let mut h = Blake2bHasher::new(32);
    h.update(b"BARELY_NANO_WALLET_SEED_PAD_V2");
    h.update(wrapping_key);
    h.update(salt);
    let pad = h.finalize();
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = seed_in[i] ^ pad[i];
    }
    out
}

/// Encrypts or decrypts a 32-byte account `priv_key` under `(wrapping_key, salt, pub_key)`.
pub fn crypt_priv_key(
    wrapping_key: &[u8; 32],
    salt: &[u8; 32],
    pub_key: &[u8; 32],
    priv_in: &[u8; 32],
) -> [u8; 32] {
    let mut h = Blake2bHasher::new(32);
    h.update(b"BARELY_NANO_WALLET_KEY_PAD_V2");
    h.update(wrapping_key);
    h.update(salt);
    h.update(pub_key);
    let pad = h.finalize();
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = priv_in[i] ^ pad[i];
    }
    out
}
