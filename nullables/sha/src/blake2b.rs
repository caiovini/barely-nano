//! Hand-optimized Blake2b implementation for bare-metal RISC-V.
//!
//! Zero external dependencies. Implements RFC 7693.
//!
//! Hardware acceleration:
//! - **Zbb**: Every `.rotate_right()` compiles to a single `rori` instruction
//!   instead of the 3-instruction (srl + sll + or) sequence on standard RISC-V.
//! - **Zvkb**: The vectorized compression function uses `vror.vi`, `vadd.vv`,
//!   and `vxor.vv` to execute 4 G-function columns in parallel per instruction.

#[cfg(target_arch = "riscv64")]
use core::arch::asm;
use core::convert::TryInto;

/// Blake2b Initialization Vector (IV)
const IV: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// Message word permutation schedule (SIGMA)
const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

#[inline(always)]
#[allow(dead_code)]
fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

#[allow(dead_code)]
fn compress(h: &mut [u64; 8], block: &[u8; 128], t: u128, last: bool) {
    let mut v = [0u64; 16];
    v[0..8].copy_from_slice(h);
    v[8..12].copy_from_slice(&IV[0..4]);
    v[12] = IV[4] ^ (t as u64);
    v[13] = IV[5] ^ ((t >> 64) as u64);
    v[14] = if last { !IV[6] } else { IV[6] };
    v[15] = IV[7];

    let mut m = [0u64; 16];
    for i in 0..16 {
        let off = i * 8;
        m[i] = u64::from_le_bytes(block[off..off + 8].try_into().unwrap());
    }

    for round in 0..12 {
        let s = &SIGMA[round];
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }

    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

#[cfg(all(target_arch = "riscv64", any(feature = "simd", target_feature = "v")))]
fn compress_vector(h: &mut [u64; 8], block: &[u8; 128], t: u128, last: bool) {
    let mut v = [0u64; 16];
    v[0..8].copy_from_slice(h);
    v[8..12].copy_from_slice(&IV[0..4]);
    v[12] = IV[4] ^ (t as u64);
    v[13] = IV[5] ^ ((t >> 64) as u64);
    v[14] = if last { !IV[6] } else { IV[6] };
    v[15] = IV[7];

    let mut m = [0u64; 16];
    for i in 0..16 {
        m[i] = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
    }

    // Pre-calculate indices for message words in each round for vector loading.
    // Each round has 4 steps of 4 words each (col_x, col_y, diag_x, diag_y).
    let mut msg_indices = [0u64; 12 * 4 * 4];
    for round in 0..12 {
        let s = &SIGMA[round];
        let base = round * 16;
        // Column X
        msg_indices[base + 0] = s[0] as u64;
        msg_indices[base + 1] = s[2] as u64;
        msg_indices[base + 2] = s[4] as u64;
        msg_indices[base + 3] = s[6] as u64;
        // Column Y
        msg_indices[base + 4] = s[1] as u64;
        msg_indices[base + 5] = s[3] as u64;
        msg_indices[base + 6] = s[5] as u64;
        msg_indices[base + 7] = s[7] as u64;
        // Diagonal X
        msg_indices[base + 8] = s[8] as u64;
        msg_indices[base + 9] = s[10] as u64;
        msg_indices[base + 10] = s[12] as u64;
        msg_indices[base + 11] = s[14] as u64;
        // Diagonal Y
        msg_indices[base + 12] = s[9] as u64;
        msg_indices[base + 13] = s[11] as u64;
        msg_indices[base + 14] = s[13] as u64;
        msg_indices[base + 15] = s[15] as u64;
    }

    unsafe {
        let diag_v10 = [1u64, 2, 3, 0];
        let diag_v12 = [2u64, 3, 0, 1];
        let diag_v14 = [3u64, 0, 1, 2];
        let undiag_v10 = [3u64, 0, 1, 2];
        let undiag_v12 = [2u64, 3, 0, 1];
        let undiag_v14 = [1u64, 2, 3, 0];

        asm!(
            "vsetivli zero, 4, e64, m2, ta, ma",
            "vle64.v v8, ({v_ptr})",
            "addi {tmp}, {v_ptr}, 32",
            "vle64.v v10, ({tmp})",
            "addi {tmp}, {v_ptr}, 64",
            "vle64.v v12, ({tmp})",
            "addi {tmp}, {v_ptr}, 96",
            "vle64.v v14, ({tmp})",

            "vle64.v v22, ({dv10})",
            "vle64.v v24, ({dv12})",
            "vle64.v v26, ({dv14})",
            "vle64.v v28, ({uv10})",
            "vle64.v v30, ({uv12})",
            "vle64.v v0, ({uv14})",

            "li {round_idx}, 0",
            "4:",
            "vle64.v v20, ({idx_ptr})",
            "vsll.vi v20, v20, 3",
            "vluxei64.v v16, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v16",
            "vxor.vv v14, v14, v8",
            "vsrl.vx v20, v14, {sh32}",
            "vsll.vx v14, v14, {sh32}",
            "vor.vv v14, v14, v20",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vsrl.vi v20, v10, 24",
            "vsll.vx v10, v10, {sh40}",
            "vor.vv v10, v10, v20",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vsll.vi v20, v20, 3",
            "vluxei64.v v18, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v18",
            "vxor.vv v14, v14, v8",
            "vsrl.vi v20, v14, 16",
            "vsll.vx v14, v14, {sh48}",
            "vor.vv v14, v14, v20",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vsrl.vx v20, v10, {sh63}",
            "vsll.vi v10, v10, 1",
            "vor.vv v10, v10, v20",
            
            "vrgather.vv v2, v10, v22",
            "vrgather.vv v4, v12, v24",
            "vrgather.vv v6, v14, v26",
            "vmv.v.v v10, v2",
            "vmv.v.v v12, v4",
            "vmv.v.v v14, v6",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vsll.vi v20, v20, 3",
            "vluxei64.v v16, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v16",
            "vxor.vv v14, v14, v8",
            "vsrl.vx v20, v14, {sh32}",
            "vsll.vx v14, v14, {sh32}",
            "vor.vv v14, v14, v20",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vsrl.vi v20, v10, 24",
            "vsll.vx v10, v10, {sh40}",
            "vor.vv v10, v10, v20",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vsll.vi v20, v20, 3",
            "vluxei64.v v18, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v18",
            "vxor.vv v14, v14, v8",
            "vsrl.vi v20, v14, 16",
            "vsll.vx v14, v14, {sh48}",
            "vor.vv v14, v14, v20",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vsrl.vx v20, v10, {sh63}",
            "vsll.vi v10, v10, 1",
            "vor.vv v10, v10, v20",
            
            "vrgather.vv v2, v10, v28",
            "vrgather.vv v4, v12, v30",
            "vrgather.vv v6, v14, v0",
            "vmv.v.v v10, v2",
            "vmv.v.v v12, v4",
            "vmv.v.v v14, v6",

            "addi {idx_ptr}, {idx_ptr}, 32",
            "addi {round_idx}, {round_idx}, 1",
            "li {max_rounds}, 12",
            "blt {round_idx}, {max_rounds}, 4b",
            
            "vse64.v v8, ({v_ptr})",
            "addi {tmp}, {v_ptr}, 32",
            "vse64.v v10, ({tmp})",
            "addi {tmp}, {v_ptr}, 64",
            "vse64.v v12, ({tmp})",
            "addi {tmp}, {v_ptr}, 96",
            "vse64.v v14, ({tmp})",

            v_ptr = in(reg) v.as_ptr(),
            m_ptr = in(reg) m.as_ptr(),
            idx_ptr = inout(reg) msg_indices.as_ptr() => _,
            dv10 = in(reg) diag_v10.as_ptr(),
            dv12 = in(reg) diag_v12.as_ptr(),
            dv14 = in(reg) diag_v14.as_ptr(),
            uv10 = in(reg) undiag_v10.as_ptr(),
            uv12 = in(reg) undiag_v12.as_ptr(),
            uv14 = in(reg) undiag_v14.as_ptr(),
            sh32 = in(reg) 32usize,
            sh40 = in(reg) 40usize,
            sh48 = in(reg) 48usize,
            sh63 = in(reg) 63usize,
            tmp = out(reg) _,
            round_idx = out(reg) _,
            max_rounds = out(reg) _,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _, out("v25") _, out("v26") _, out("v27") _,
            out("v28") _, out("v29") _, out("v30") _, out("v31") _,
        );
    }

    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

#[inline(always)]
fn compress_block(h: &mut [u64; 8], block: &[u8; 128], t: u128, last: bool) {
    #[cfg(all(target_arch = "riscv64", any(feature = "simd", target_feature = "v")))]
    {
        compress_vector(h, block, t, last);
    }
    #[cfg(not(all(target_arch = "riscv64", any(feature = "simd", target_feature = "v"))))]
    {
        compress(h, block, t, last);
    }
}

/// Compress four independent Blake2b blocks simultaneously using RISC-V Vector extensions.
///
/// **Design Decisions & Optimizations:**
/// 1. **Transposed Layout:** Instead of running four separate hashes in series, we transpose the
///    states (`v`) and messages (`m`) across the vector registers. Element `v[i * 4 + j]` holds 
///    state word `i` for block `j`. This enables standard, high-speed unit-stride vector loads
///    (`vle64.v`) and stores (`vse64.v`) instead of expensive indexed gathers/strided loads.
/// 2. **State Register Caching:** Operating on a 4-block parallel width requires 256 bits of state
///    per transposed variable. On `VLEN=128` hardware (JH7110 / Orange Pi RV2), this mandates 
///    `LMUL=2` grouping. Storing all 16 state variables in registers simultaneously is not possible 
///    due to register exhaustion (16 variables * 2 registers = 32 registers, leaving no registers 
///    for temporary shifts/rotations and message loads). We resolve this elegantly by storing the
///    transposed state array in L1 cache (512 bytes total) and loading/storing only the active
///    four state registers (`v_a`, `v_b`, `v_c`, `v_d`) inside each vectorized G-step.
/// 3. **Offset Tables:** We pre-compute message word byte offsets (`msg_offsets`) for the 12 rounds
///    and pass them dynamically to load transposed message words from L1 cache with minimal latency.
#[cfg(all(target_arch = "riscv64", target_feature = "zvkb"))]
pub fn compress_four_blocks(
    h: &mut [&mut [u64; 8]; 4],
    blocks: &[&[u8; 128]; 4],
    t: &[u128; 4],
    last: &[bool; 4],
) {
    let mut v = [0u64; 16 * 4];
    
    // Initialize transposed state matrix.
    // v[i * 4 + j] maps state word `i` across block index `j`.
    for j in 0..4 {
        for i in 0..8 {
            v[i * 4 + j] = h[j][i];
        }
        v[8 * 4 + j] = IV[0];
        v[9 * 4 + j] = IV[1];
        v[10 * 4 + j] = IV[2];
        v[11 * 4 + j] = IV[3];
        v[12 * 4 + j] = IV[4] ^ (t[j] as u64);
        v[13 * 4 + j] = IV[5] ^ ((t[j] >> 64) as u64);
        v[14 * 4 + j] = if last[j] { !IV[6] } else { IV[6] };
        v[15 * 4 + j] = IV[7];
    }

    // Transpose and load 128-byte message blocks into a transposed buffer.
    // Each transposed message word `i` is stored contiguously in memory as 4 elements of 64-bit.
    let mut m = [0u64; 16 * 4];
    for j in 0..4 {
        for i in 0..16 {
            let off = i * 8;
            m[i * 4 + j] = u64::from_le_bytes(blocks[j][off..off + 8].try_into().unwrap());
        }
    }

    // Pre-calculate absolute message word pointers for all 12 rounds (plus 1 dummy round for lookahead safety).
    let mut msg_ptrs = [0u64; 13 * 16];
    let m_ptr_val = m.as_ptr() as u64;
    for round in 0..12 {
        let s = &SIGMA[round];
        for i in 0..16 {
            msg_ptrs[round * 16 + i] = m_ptr_val + (s[i] * 32) as u64;
        }
    }
    // Initialize dummy round 12 to safely point to m to prevent NULL pointer dereferences on lookahead loads at the end of the 12th round.
    for i in 0..16 {
        msg_ptrs[12 * 16 + i] = m_ptr_val;
    }

    unsafe {
        let offsets_ptr = msg_ptrs.as_ptr();
        let v_ptr = v.as_mut_ptr();

        asm!(
            "vsetivli zero, 4, e64, m2, ta, ma",
            
            // 1. Initial Load of v0..v26 (14 state variables)
            // Explicit LMUL=2 group bases are even: v0, v2, ..., v26
            "vle64.v v0, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v2, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v4, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v6, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v8, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v10, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v12, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v14, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v16, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v18, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v20, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v22, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v24, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v26, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, -416", // Restore v_ptr to base (13 * 32 = 416)

            // Initial lookahead setup for Step 1 of Round 0:
            "ld {x_ptr}, 0({offsets_ptr})",
            "ld {y_ptr}, 8({offsets_ptr})",
            "vle64.v v30, ({x_ptr})", // Load Step 1 x in background

            "li {round_idx}, 0",
            
            "2:", // Loop start
            
            // --- Step 1: g(0, 4, 8, 12) -> Registers: v0, v8, v16, v24 ---
            "ld {x_ptr}, 16({offsets_ptr})", // Pre-load Step 2 x_ptr
            
            "vadd.vv v0, v0, v8",
            "vadd.vv v0, v0, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 1 y in background!
            
            "ld {y_ptr}, 24({offsets_ptr})", // Pre-load Step 2 y_ptr
            
            "vxor.vv v24, v24, v0",
            "vror.vi v24, v24, 32",
            "vadd.vv v16, v16, v24",
            "vxor.vv v8, v8, v16",
            "vror.vi v8, v8, 24",
            
            "vadd.vv v0, v0, v8",
            "vadd.vv v0, v0, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 2 x in background!
            
            "vxor.vv v24, v24, v0",
            "vror.vi v24, v24, 16",
            "vadd.vv v16, v16, v24",
            "vxor.vv v8, v8, v16",
            "vror.vi v8, v8, 63",

            // --- Step 2: g(1, 5, 9, 13) -> Registers: v2, v10, v18, v26 ---
            "ld {x_ptr}, 32({offsets_ptr})", // Pre-load Step 3 x_ptr
            
            "vadd.vv v2, v2, v10",
            "vadd.vv v2, v2, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 2 y in background!
            
            "ld {y_ptr}, 40({offsets_ptr})", // Pre-load Step 3 y_ptr
            
            "vxor.vv v26, v26, v2",
            "vror.vi v26, v26, 32",
            "vadd.vv v18, v18, v26",
            "vxor.vv v10, v10, v18",
            "vror.vi v10, v10, 24",
            
            "vadd.vv v2, v2, v10",
            "vadd.vv v2, v2, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 3 x in background!
            
            "addi {x_ptr}, {v_ptr}, 448",
            "vle64.v v28, ({x_ptr})", // Trigger load of Step 3 v14 in background!
            
            "vxor.vv v26, v26, v2",
            "vror.vi v26, v26, 16",
            "vadd.vv v18, v18, v26",
            "vxor.vv v10, v10, v18",
            "vror.vi v10, v10, 63",

            // --- Step 3: g(2, 6, 10, 14) -> Registers: v4, v12, v20, v28 (uses v14) ---
            "ld {x_ptr}, 48({offsets_ptr})", // Pre-load Step 4 x_ptr
            
            "vadd.vv v4, v4, v12",
            "vadd.vv v4, v4, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 3 y in background!
            
            "ld {y_ptr}, 56({offsets_ptr})", // Pre-load Step 4 y_ptr
            
            "vxor.vv v28, v28, v4",
            "vror.vi v28, v28, 32",
            "vadd.vv v20, v20, v28",
            "vxor.vv v12, v12, v20",
            "vror.vi v12, v12, 24",
            
            "vadd.vv v4, v4, v12",
            "vadd.vv v4, v4, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 4 x in background!
            
            "vxor.vv v28, v28, v4",
            "vror.vi v28, v28, 16",
            "vadd.vv v20, v20, v28",
            
            "addi {x_ptr}, {v_ptr}, 448",
            "vse64.v v28, ({x_ptr})", // Save modified v14 to memory
            
            "addi {x_ptr}, {v_ptr}, 480",
            "vle64.v v28, ({x_ptr})", // Trigger load of Step 4 v15 in background!
            
            "vxor.vv v12, v12, v20",
            "vror.vi v12, v12, 63",

            // --- Step 4: g(3, 7, 11, 15) -> Registers: v6, v14, v22, v28 (uses v15) ---
            "ld {x_ptr}, 64({offsets_ptr})", // Pre-load Step 5 x_ptr
            
            "vadd.vv v6, v6, v14",
            "vadd.vv v6, v6, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 4 y in background!
            
            "ld {y_ptr}, 72({offsets_ptr})", // Pre-load Step 5 y_ptr
            
            "vxor.vv v28, v28, v6",
            "vror.vi v28, v28, 32",
            "vadd.vv v22, v22, v28",
            "vxor.vv v14, v14, v22",
            "vror.vi v14, v14, 24",
            
            "vadd.vv v6, v6, v14",
            "vadd.vv v6, v6, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 5 x in background!
            
            "vxor.vv v28, v28, v6",
            "vror.vi v28, v28, 16",
            "vadd.vv v22, v22, v28",
            "vxor.vv v14, v14, v22",
            "vror.vi v14, v14, 63",

            // --- Step 5: g(0, 5, 10, 15) -> Registers: v0, v10, v20, v28 (uses v15) ---
            "ld {x_ptr}, 80({offsets_ptr})", // Pre-load Step 6 x_ptr
            
            "vadd.vv v0, v0, v10",
            "vadd.vv v0, v0, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 5 y in background!
            
            "ld {y_ptr}, 88({offsets_ptr})", // Pre-load Step 6 y_ptr
            
            "vxor.vv v28, v28, v0",
            "vror.vi v28, v28, 32",
            "vadd.vv v20, v20, v28",
            "vxor.vv v10, v10, v20",
            "vror.vi v10, v10, 24",
            
            "vadd.vv v0, v0, v10",
            "vadd.vv v0, v0, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 6 x in background!
            
            "vxor.vv v28, v28, v0",
            "vror.vi v28, v28, 16",
            "vadd.vv v20, v20, v28",
            
            "addi {x_ptr}, {v_ptr}, 480",
            "vse64.v v28, ({x_ptr})", // Save modified v15 to memory
            
            "vxor.vv v10, v10, v20",
            "vror.vi v10, v10, 63",

            // --- Step 6: g(1, 6, 11, 12) -> Registers: v2, v12, v22, v24 ---
            "ld {x_ptr}, 96({offsets_ptr})", // Pre-load Step 7 x_ptr
            
            "vadd.vv v2, v2, v12",
            "vadd.vv v2, v2, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 6 y in background!
            
            "ld {y_ptr}, 104({offsets_ptr})", // Pre-load Step 7 y_ptr
            
            "vxor.vv v24, v24, v2",
            "vror.vi v24, v24, 32",
            "vadd.vv v22, v22, v24",
            "vxor.vv v12, v12, v22",
            "vror.vi v12, v12, 24",
            
            "vadd.vv v2, v2, v12",
            "vadd.vv v2, v2, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 7 x in background!
            
            "vxor.vv v24, v24, v2",
            "vror.vi v24, v24, 16",
            "vadd.vv v22, v22, v24",
            "vxor.vv v12, v12, v22",
            "vror.vi v12, v12, 63",

            // --- Step 7: g(2, 7, 8, 13) -> Registers: v4, v14, v16, v26 ---
            "ld {x_ptr}, 112({offsets_ptr})", // Pre-load Step 8 x_ptr
            
            "vadd.vv v4, v4, v14",
            "vadd.vv v4, v4, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 7 y in background!
            
            "ld {y_ptr}, 120({offsets_ptr})", // Pre-load Step 8 y_ptr
            
            "vxor.vv v26, v26, v4",
            "vror.vi v26, v26, 32",
            "vadd.vv v16, v16, v26",
            "vxor.vv v14, v14, v16",
            "vror.vi v14, v14, 24",
            
            "vadd.vv v4, v4, v14",
            "vadd.vv v4, v4, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Step 8 x in background!
            
            "addi {x_ptr}, {v_ptr}, 448",
            "vle64.v v28, ({x_ptr})", // Trigger load of Step 8 v14 in background!
            
            "vxor.vv v26, v26, v4",
            "vror.vi v26, v26, 16",
            "vadd.vv v16, v16, v26",
            "vxor.vv v14, v14, v16",
            "vror.vi v14, v14, 63",

            // --- Step 8: g(3, 4, 9, 14) -> Registers: v6, v8, v18, v28 (uses v14) ---
            "addi {offsets_ptr}, {offsets_ptr}, 128", // Move to next round offsets
            "ld {x_ptr}, 0({offsets_ptr})", // Pre-load Round N+1 Step 1 x_ptr
            
            "vadd.vv v6, v6, v8",
            "vadd.vv v6, v6, v30",
            
            "vle64.v v30, ({y_ptr})", // Trigger load of Step 8 y in background!
            
            "ld {y_ptr}, 8({offsets_ptr})", // Pre-load Round N+1 Step 1 y_ptr
            
            "vxor.vv v28, v28, v6",
            "vror.vi v28, v28, 32",
            "vadd.vv v18, v18, v28",
            "vxor.vv v8, v8, v18",
            "vror.vi v8, v8, 24",
            
            "vadd.vv v6, v6, v8",
            "vadd.vv v6, v6, v30",
            
            "vle64.v v30, ({x_ptr})", // Trigger load of Round N+1 Step 1 x in background!
            
            "vxor.vv v28, v28, v6",
            "vror.vi v28, v28, 16",
            "vadd.vv v18, v18, v28",
            
            "addi {x_ptr}, {v_ptr}, 448",
            "vse64.v v28, ({x_ptr})", // Save modified v14 to memory
            
            "vxor.vv v8, v8, v18",
            "vror.vi v8, v8, 63",
            
            // --- Loop Control ---
            "addi {round_idx}, {round_idx}, 1",
            "li {max_rounds}, 12",
            "blt {round_idx}, {max_rounds}, 2b",

            // --- Write Back permanent registers v0..v13 (14 state variables) ---
            "vse64.v v0, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v2, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v4, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v6, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v8, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v10, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v12, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v14, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v16, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v18, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v20, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v22, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v24, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v26, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, -416", // Restore v_ptr to base (13 * 32 = 416)

            v_ptr = inout(reg) v_ptr => _,
            offsets_ptr = inout(reg) offsets_ptr => _,
            round_idx = out(reg) _,
            max_rounds = out(reg) _,
            x_ptr = out(reg) _,
            y_ptr = out(reg) _,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _, out("v25") _, out("v26") _, out("v27") _,
            out("v28") _, out("v29") _, out("v30") _, out("v31") _,
        );
    }

    // XOR the final state back into the four independent state array references.
    for j in 0..4 {
        for i in 0..8 {
            h[j][i] ^= v[i * 4 + j] ^ v[(i + 8) * 4 + j];
        }
    }
}

/// Scalar fallback for non-vector architectures, executing standard sequential compression.
#[cfg(not(all(target_arch = "riscv64", target_feature = "zvkb")))]
pub fn compress_four_blocks(
    h: &mut [&mut [u64; 8]; 4],
    blocks: &[&[u8; 128]; 4],
    t: &[u128; 4],
    last: &[bool; 4],
) {
    for i in 0..4 {
        compress(h[i], blocks[i], t[i], last[i]);
    }
}

pub struct Blake2bHasher {
    h: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    t: u128,
}

impl Blake2bHasher {
    pub fn new(hash_len: usize) -> Self {
        assert!(hash_len > 0 && hash_len <= 64);
        let mut h = IV;
        h[0] ^= 0x01010000 ^ (hash_len as u64);
        Self { h, buf: [0u8; 128], buf_len: 0, t: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.buf_len < 128 {
                let space = 128 - self.buf_len;
                let n = core::cmp::min(data.len(), space);
                self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
                self.buf_len += n;
                data = &data[n..];
            }
            if self.buf_len == 128 && !data.is_empty() {
                self.t += 128;
                let block = self.buf;
                compress_block(&mut self.h, &block, self.t, false);
                self.buf_len = 0;
            }
        }
    }

    pub fn finalize(mut self) -> [u8; 64] {
        self.t += self.buf_len as u128;
        self.buf[self.buf_len..].fill(0);
        let block = self.buf;
        compress_block(&mut self.h, &block, self.t, true);

        let mut out = [0u8; 64];
        for i in 0..8 {
            let word = self.h[i];
            out[i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

pub fn hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Blake2bHasher::new(32);
    hasher.update(data);
    let full = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&full[..32]);
    out
}

pub fn hash64(data: &[u8]) -> [u8; 64] {
    let mut hasher = Blake2bHasher::new(64);
    hasher.update(data);
    hasher.finalize()
}

pub fn hash_batch_4(inputs: [&[u8]; 4]) -> [[u8; 32]; 4] {
    let mut h = [IV; 4];
    for i in 0..4 {
        h[i][0] ^= 0x01010020;
    }
    
    hash_batch_impl(&mut h, inputs);
    
    let mut out = [[0u8; 32]; 4];
    for j in 0..4 {
        for i in 0..4 {
            let word = h[j][i];
            out[j][i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
    }
    out
}

pub fn hash64_batch_4(inputs: [&[u8]; 4]) -> [[u8; 64]; 4] {
    let mut h = [IV; 4];
    for i in 0..4 {
        h[i][0] ^= 0x01010040;
    }
    
    hash_batch_impl(&mut h, inputs);
    
    let mut out = [[0u8; 64]; 4];
    for j in 0..4 {
        for i in 0..8 {
            let word = h[j][i];
            out[j][i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
    }
    out
}

/// Helper execution engine for batched Blake2b hashing of four independent payloads.
///
/// **Correctness & Safety Mechanisms:**
/// 1. **Lock-Step Parallelism:** When all 4 input buffers are of the exact same length (such as 144 bytes 
///    for Nano state blocks, or 96 bytes for Ed25519 signature checks), this function performs a 
///    highly optimized, zero-allocation lock-step parallel vector compression over 128-byte chunks.
/// 2. **Rust Borrow-Checker Splitting:** To avoid compilation errors under Rust's single mutable reference
///    rule, we destructure `h` into isolated variables (`let [h0, h1, h2, h3] = h`). This statically 
///    proves to the compiler that our mutable references point to completely non-overlapping memory blocks.
/// 3. **Re-usable Reference Array:** We collect the destructured references in `h_refs` once before the 
///    loop and pass them by mutable reference `&mut h_refs` to `compress_four_blocks`. This prevents 
///    moves/consumption of `h_refs` and permits infinite re-use within the loop.
/// 4. **Safe Length Fallback:** If the payloads are mismatched in length, we transparently fall back to 
///    standard, independent sequential scalar hashes for each stream. This guarantees perfect correctness 
///    under any unforeseen input scenario.
fn hash_batch_impl(h: &mut [[u64; 8]; 4], inputs: [&[u8]; 4]) {
    let len0 = inputs[0].len();
    if inputs[1].len() == len0 && inputs[2].len() == len0 && inputs[3].len() == len0 {
        let mut offset = 0;
        let mut t = [0u128; 4];
        let mut remaining = len0;
        
        let [h0, h1, h2, h3] = h;
        let mut h_refs = [h0, h1, h2, h3];
        
        while remaining > 128 {
            let mut blocks = [[0u8; 128]; 4];
            for j in 0..4 {
                blocks[j].copy_from_slice(&inputs[j][offset..offset + 128]);
                t[j] += 128;
            }
            
            let blocks_refs = [
                &blocks[0],
                &blocks[1],
                &blocks[2],
                &blocks[3],
            ];
            let last = [false, false, false, false];
            
            compress_four_blocks(&mut h_refs, &blocks_refs, &t, &last);
            
            offset += 128;
            remaining -= 128;
        }
        
        let mut blocks = [[0u8; 128]; 4];
        for j in 0..4 {
            blocks[j][..remaining].copy_from_slice(&inputs[j][offset..offset + remaining]);
            t[j] += remaining as u128;
        }
        
        let blocks_refs = [
            &blocks[0],
            &blocks[1],
            &blocks[2],
            &blocks[3],
        ];
        let last = [true, true, true, true];
        
        compress_four_blocks(&mut h_refs, &blocks_refs, &t, &last);
    } else {
        for j in 0..4 {
            let mut hasher = Blake2bHasher::new(if h[j][0] & 0x40 != 0 { 64 } else { 32 });
            hasher.update(inputs[j]);
            let full = hasher.finalize();
            if h[j][0] & 0x40 != 0 {
                for i in 0..8 {
                    h[j][i] = u64::from_le_bytes(full[i * 8..(i + 1) * 8].try_into().unwrap());
                }
            } else {
                for i in 0..4 {
                    h[j][i] = u64::from_le_bytes(full[i * 8..(i + 1) * 8].try_into().unwrap());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_batch_4() {
        let msg0 = b"hello world 0";
        let msg1 = b"hello world 1";
        let msg2 = b"hello world 2";
        let msg3 = b"hello world 3";
        
        let expected0 = hash(msg0);
        let expected1 = hash(msg1);
        let expected2 = hash(msg2);
        let expected3 = hash(msg3);
        
        let res = hash_batch_4([msg0, msg1, msg2, msg3]);
        
        assert_eq!(res[0], expected0);
        assert_eq!(res[1], expected1);
        assert_eq!(res[2], expected2);
        assert_eq!(res[3], expected3);
    }

    #[test]
    fn test_hash64_batch_4() {
        let msg0 = b"hello world 0";
        let msg1 = b"hello world 1";
        let msg2 = b"hello world 2";
        let msg3 = b"hello world 3";
        
        let expected0 = hash64(msg0);
        let expected1 = hash64(msg1);
        let expected2 = hash64(msg2);
        let expected3 = hash64(msg3);
        
        let res = hash64_batch_4([msg0, msg1, msg2, msg3]);
        
        assert_eq!(res[0], expected0);
        assert_eq!(res[1], expected1);
        assert_eq!(res[2], expected2);
        assert_eq!(res[3], expected3);
    }

    #[test]
    fn test_blake2b_zeros() {
        let mut data = [0u8; 36]; // 32 byte seed + 4 byte index
        let h32 = hash(&data);
        let h64 = hash64(&data);
        
        println!("Hash32 (36 bytes): {:02X?}", h32);
        println!("Hash64 truncated (36 bytes): {:02X?}", &h64[..32]);
    }

    #[test]
    fn test_blake2b_abc() {
        let data = b"abc";
        let expected = [
            0xBA, 0x80, 0xA5, 0x3F, 0x98, 0x1C, 0x4D, 0x0D, 
            0x6A, 0x27, 0x97, 0xB6, 0x9F, 0x12, 0xF6, 0xE9, 
            0x4C, 0x21, 0x2F, 0x14, 0x68, 0x5A, 0xC4, 0xB7, 
            0x4B, 0x12, 0xBB, 0x6F, 0xDB, 0xFF, 0xA2, 0xD1, 
            0x7D, 0x87, 0xC5, 0x39, 0x2A, 0xAB, 0x79, 0x2D, 
            0xC2, 0x52, 0xD5, 0xDE, 0x45, 0x33, 0xCC, 0x95, 
            0x18, 0xD3, 0x8A, 0xA8, 0xDB, 0xF1, 0x92, 0x5A, 
            0xB9, 0x23, 0x86, 0xED, 0xD4, 0x00, 0x99, 0x23
        ];
        assert_eq!(hash64(data), expected);
    }

    #[test]
    fn test_compress_four_blocks() {
        let mut h0 = IV;
        let mut h1 = IV;
        let mut h2 = IV;
        let mut h3 = IV;
        
        let block0 = [1u8; 128];
        let block1 = [2u8; 128];
        let block2 = [3u8; 128];
        let block3 = [4u8; 128];
        
        let mut h0_expected = h0;
        let mut h1_expected = h1;
        let mut h2_expected = h2;
        let mut h3_expected = h3;
        
        compress(&mut h0_expected, &block0, 128, false);
        compress(&mut h1_expected, &block1, 128, false);
        compress(&mut h2_expected, &block2, 128, false);
        compress(&mut h3_expected, &block3, 128, false);
        
        let mut h = [
            &mut h0,
            &mut h1,
            &mut h2,
            &mut h3,
        ];
        let blocks = [
            &block0,
            &block1,
            &block2,
            &block3,
        ];
        let t = [128, 128, 128, 128];
        let last = [false, false, false, false];
        
        compress_four_blocks(&mut h, &blocks, &t, &last);
        
        assert_eq!(h0, h0_expected);
        assert_eq!(h1, h1_expected);
        assert_eq!(h2, h2_expected);
        assert_eq!(h3, h3_expected);
    }
}

