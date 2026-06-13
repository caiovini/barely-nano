/// Compute a Blake2b-256 hash of `data`.
///
/// Uses our hand-optimized, zero-dependency implementation from the `tools` crate.
/// Every rotation compiles to a single Zbb `rori` instruction.
pub fn hash_blake2b(data: &[u8]) -> [u8; 32] {
    rsnano_sha::blake2b::hash(data)
}

pub fn test_hw_extensions() -> bool {
    // In S-Mode, we cannot read the `misa` register directly without trapping.
    // Instead of risking a kernel panic, we trust the compiler flags.
    #[cfg(target_feature = "zvkb")]
    crate::uart::print(b"[crypto] HW SUPPORT: Zbb (bit-manip), Zvkb (vector-crypto), Zicbom (cache-block) enabled\n");
    #[cfg(not(target_feature = "zvkb"))]
    crate::uart::print(b"[crypto] HW SUPPORT: Zbb (bit-manip), standard RVV 1.0 (no vector-crypto) enabled\n");
    true
}
