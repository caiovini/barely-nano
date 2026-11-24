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

#[allow(dead_code)]
#[cfg(all(target_arch = "riscv64", feature = "simd"))]
pub fn compress_vector(h: &mut [u64; 8], block: &[u8; 128], t: u128, last: bool) {
    #[repr(align(64))]
    struct AlignedBuffer<const N: usize>([u64; N]);

    let mut v_buf = AlignedBuffer([0u64; 16]);
    let v = &mut v_buf.0;
    v[0..8].copy_from_slice(h);
    v[8..12].copy_from_slice(&IV[0..4]);
    v[12] = IV[4] ^ (t as u64);
    v[13] = IV[5] ^ ((t >> 64) as u64);
    v[14] = if last { !IV[6] } else { IV[6] };
    v[15] = IV[7];

    let mut m_buf = AlignedBuffer([0u64; 16]);
    let m = &mut m_buf.0;
    for i in 0..16 {
        m[i] = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
    }

    // Pre-calculate indices for message words in each round for vector loading.
    // Each round has 4 steps of 4 words each (col_x, col_y, diag_x, diag_y).
    // vluxei64.v indices are BYTE offsets (not scaled by element size), and `m` is a plain
    // [u64; 16], so each index is word_index * 8. (Using * 4 produced misaligned loads.)
    let mut msg_indices = [0u64; 12 * 4 * 4];
    for round in 0..12 {
        let s = &SIGMA[round];
        let base = round * 16;
        // Column X: indices 0, 2, 4, 6 -> multiply by 4 to get 0, 8, 16, 24 (index units)
        msg_indices[base + 0] = (s[0] * 8) as u64;
        msg_indices[base + 1] = (s[2] * 8) as u64;
        msg_indices[base + 2] = (s[4] * 8) as u64;
        msg_indices[base + 3] = (s[6] * 8) as u64;
        // Column Y
        msg_indices[base + 4] = (s[1] * 8) as u64;
        msg_indices[base + 5] = (s[3] * 8) as u64;
        msg_indices[base + 6] = (s[5] * 8) as u64;
        msg_indices[base + 7] = (s[7] * 8) as u64;
        // Diagonal X
        msg_indices[base + 8] = (s[8] * 8) as u64;
        msg_indices[base + 9] = (s[10] * 8) as u64;
        msg_indices[base + 10] = (s[12] * 8) as u64;
        msg_indices[base + 11] = (s[14] * 8) as u64;
        // Diagonal Y
        msg_indices[base + 12] = (s[9] * 8) as u64;
        msg_indices[base + 13] = (s[11] * 8) as u64;
        msg_indices[base + 14] = (s[13] * 8) as u64;
        msg_indices[base + 15] = (s[15] * 8) as u64;
    }

    unsafe {
        let diag_v10 = [1u64, 2, 3, 0];
        let diag_v12 = [2u64, 3, 0, 1];
        let diag_v14 = [3u64, 0, 1, 2];
        let undiag_v10 = [3u64, 0, 1, 2];
        let undiag_v12 = [2u64, 3, 0, 1];
        let undiag_v14 = [1u64, 2, 3, 0];


        asm!(
            "vsetivli zero, 4, e64, m2",
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
            "vluxei64.v v16, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v16",
            "vxor.vv v14, v14, v8",
            "vror.vi v14, v14, 32",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vror.vi v10, v10, 24",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vluxei64.v v18, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v18",
            "vxor.vv v14, v14, v8",
            "vror.vi v14, v14, 16",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vror.vi v10, v10, 63",
            
            "vrgather.vv v2, v10, v22",
            "vrgather.vv v4, v12, v24",
            "vrgather.vv v6, v14, v26",
            "vmv.v.v v10, v2",
            "vmv.v.v v12, v4",
            "vmv.v.v v14, v6",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vluxei64.v v16, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v16",
            "vxor.vv v14, v14, v8",
            "vror.vi v14, v14, 32",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vror.vi v10, v10, 24",
            
            "addi {idx_ptr}, {idx_ptr}, 32",
            "vle64.v v20, ({idx_ptr})",
            "vluxei64.v v18, ({m_ptr}), v20",
            
            "vadd.vv v8, v8, v10",
            "vadd.vv v8, v8, v18",
            "vxor.vv v14, v14, v8",
            "vror.vi v14, v14, 16",
            "vadd.vv v12, v12, v14",
            "vxor.vv v10, v10, v12",
            "vror.vi v10, v10, 63",
            
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
    #[cfg(all(target_arch = "riscv64", feature = "simd"))]
    {
        compress_vector(h, block, t, last);
    }
    #[cfg(not(all(target_arch = "riscv64", feature = "simd")))]
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
#[cfg(all(target_arch = "riscv64", feature = "simd"))]
pub fn compress_four_blocks(
    h: &mut [&mut [u64; 8]; 4],
    blocks: &[&[u8; 128]; 4],
    t: &[u128; 4],
    last: &[bool; 4],
) {
    static mut CALLED: bool = false;
    unsafe {
        if !CALLED {
            // We can't easily use uart here without imports, but we can use a volatile write
            // or just rely on the difficulty.rs print we added earlier.
            // For now, let's just ensure the function is actually reachable.
            CALLED = true;
        }
    }
    #[repr(align(64))]
    struct AlignedBuffer<const N: usize>([u64; N]);

    let mut v_buf = AlignedBuffer([0u64; 16 * 4]);
    let v = &mut v_buf.0;
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

    let mut m_buf = AlignedBuffer([0u64; 16 * 4]);
    let m = &mut m_buf.0;
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
            "vsetivli zero, 4, e64, m1",
            
            // 1. Load all 16 transposed state variables into v0..v15 (LMUL=1 on VLEN >= 256)
            "vle64.v v0, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v1, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v2, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v3, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v4, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v5, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v6, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v7, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v8, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v9, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v10, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v11, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v12, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v13, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v14, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vle64.v v15, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, -480",

            // Initial lookahead setup for Step 1 of Round 0:
            "ld {x_ptr}, 0({offsets_ptr})",
            "ld {y_ptr}, 8({offsets_ptr})",
            "vle64.v v30, ({x_ptr})",

            "li {round_idx}, 0",
            
            "2:", // Loop start
            
            // --- Step 1: g(0, 4, 8, 12) ---
            "ld {x_ptr}, 16({offsets_ptr})",
            "vadd.vv v0, v0, v4",
            "vadd.vv v0, v0, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 24({offsets_ptr})",
            "vxor.vv v12, v12, v0",
            "vror.vi v12, v12, 32",
            "vadd.vv v8, v8, v12",
            "vxor.vv v4, v4, v8",
            "vror.vi v4, v4, 24",
            "vadd.vv v0, v0, v4",
            "vadd.vv v0, v0, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v12, v12, v0",
            "vror.vi v12, v12, 16",
            "vadd.vv v8, v8, v12",
            "vxor.vv v4, v4, v8",
            "vror.vi v4, v4, 63",

            // --- Step 2: g(1, 5, 9, 13) ---
            "ld {x_ptr}, 32({offsets_ptr})",
            "vadd.vv v1, v1, v5",
            "vadd.vv v1, v1, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 40({offsets_ptr})",
            "vxor.vv v13, v13, v1",
            "vror.vi v13, v13, 32",
            "vadd.vv v9, v9, v13",
            "vxor.vv v5, v5, v9",
            "vror.vi v5, v5, 24",
            "vadd.vv v1, v1, v5",
            "vadd.vv v1, v1, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v13, v13, v1",
            "vror.vi v13, v13, 16",
            "vadd.vv v9, v9, v13",
            "vxor.vv v5, v5, v9",
            "vror.vi v5, v5, 63",

            // --- Step 3: g(2, 6, 10, 14) ---
            "ld {x_ptr}, 48({offsets_ptr})",
            "vadd.vv v2, v2, v6",
            "vadd.vv v2, v2, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 56({offsets_ptr})",
            "vxor.vv v14, v14, v2",
            "vror.vi v14, v14, 32",
            "vadd.vv v10, v10, v14",
            "vxor.vv v6, v6, v10",
            "vror.vi v6, v6, 24",
            "vadd.vv v2, v2, v6",
            "vadd.vv v2, v2, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v14, v14, v2",
            "vror.vi v14, v14, 16",
            "vadd.vv v10, v10, v14",
            "vxor.vv v6, v6, v10",
            "vror.vi v6, v6, 63",

            // --- Step 4: g(3, 7, 11, 15) ---
            "ld {x_ptr}, 64({offsets_ptr})",
            "vadd.vv v3, v3, v7",
            "vadd.vv v3, v3, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 72({offsets_ptr})",
            "vxor.vv v15, v15, v3",
            "vror.vi v15, v15, 32",
            "vadd.vv v11, v11, v15",
            "vxor.vv v7, v7, v11",
            "vror.vi v7, v7, 24",
            "vadd.vv v3, v3, v7",
            "vadd.vv v3, v3, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v15, v15, v3",
            "vror.vi v15, v15, 16",
            "vadd.vv v11, v11, v15",
            "vxor.vv v7, v7, v11",
            "vror.vi v7, v7, 63",

            // --- Step 5: g(0, 5, 10, 15) ---
            "ld {x_ptr}, 80({offsets_ptr})",
            "vadd.vv v0, v0, v5",
            "vadd.vv v0, v0, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 88({offsets_ptr})",
            "vxor.vv v15, v15, v0",
            "vror.vi v15, v15, 32",
            "vadd.vv v10, v10, v15",
            "vxor.vv v5, v5, v10",
            "vror.vi v5, v5, 24",
            "vadd.vv v0, v0, v5",
            "vadd.vv v0, v0, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v15, v15, v0",
            "vror.vi v15, v15, 16",
            "vadd.vv v10, v10, v15",
            "vxor.vv v5, v5, v10",
            "vror.vi v5, v5, 63",

            // --- Step 6: g(1, 6, 11, 12) ---
            "ld {x_ptr}, 96({offsets_ptr})",
            "vadd.vv v1, v1, v6",
            "vadd.vv v1, v1, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 104({offsets_ptr})",
            "vxor.vv v12, v12, v1",
            "vror.vi v12, v12, 32",
            "vadd.vv v11, v11, v12",
            "vxor.vv v6, v6, v11",
            "vror.vi v6, v6, 24",
            "vadd.vv v1, v1, v6",
            "vadd.vv v1, v1, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v12, v12, v1",
            "vror.vi v12, v12, 16",
            "vadd.vv v11, v11, v12",
            "vxor.vv v6, v6, v11",
            "vror.vi v6, v6, 63",

            // --- Step 7: g(2, 7, 8, 13) ---
            "ld {x_ptr}, 112({offsets_ptr})",
            "vadd.vv v2, v2, v7",
            "vadd.vv v2, v2, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 120({offsets_ptr})",
            "vxor.vv v13, v13, v2",
            "vror.vi v13, v13, 32",
            "vadd.vv v8, v8, v13",
            "vxor.vv v7, v7, v8",
            "vror.vi v7, v7, 24",
            "vadd.vv v2, v2, v7",
            "vadd.vv v2, v2, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v13, v13, v2",
            "vror.vi v13, v13, 16",
            "vadd.vv v8, v8, v13",
            "vxor.vv v7, v7, v8",
            "vror.vi v7, v7, 63",

            // --- Step 8: g(3, 4, 9, 14) ---
            "addi {offsets_ptr}, {offsets_ptr}, 128",
            "ld {x_ptr}, 0({offsets_ptr})",
            "vadd.vv v3, v3, v4",
            "vadd.vv v3, v3, v30",
            "vle64.v v30, ({y_ptr})",
            "ld {y_ptr}, 8({offsets_ptr})",
            "vxor.vv v14, v14, v3",
            "vror.vi v14, v14, 32",
            "vadd.vv v9, v9, v14",
            "vxor.vv v4, v4, v9",
            "vror.vi v4, v4, 24",
            "vadd.vv v3, v3, v4",
            "vadd.vv v3, v3, v30",
            "vle64.v v30, ({x_ptr})",
            "vxor.vv v14, v14, v3",
            "vror.vi v14, v14, 16",
            "vadd.vv v9, v9, v14",
            "vxor.vv v4, v4, v9",
            "vror.vi v4, v4, 63",

            // --- Loop Control ---
            "addi {round_idx}, {round_idx}, 1",
            "li {max_rounds}, 12",
            "blt {round_idx}, {max_rounds}, 2b",

            // --- Write Back all 16 state variables v0..v15 ---
            "vse64.v v0, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v1, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v2, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v3, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v4, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v5, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v6, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v7, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v8, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v9, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v10, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v11, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v12, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v13, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v14, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, 32",
            "vse64.v v15, ({v_ptr})",
            "addi {v_ptr}, {v_ptr}, -480",

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
            out("v30") _,
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
#[cfg(not(all(target_arch = "riscv64", feature = "simd")))]
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

/// Returns `true` when the executing RISC-V hart has `VLEN >= 1024` (`vlenb >= 128`),
/// which is true on SpacemiT K3 Cluster 2 & 3 (A100 cores, Harts 8..15).
#[inline(always)]
pub fn has_vlen_1024() -> bool {
    #[cfg(all(target_arch = "riscv64", feature = "simd"))]
    {
        let vlenb: usize;
        // SAFETY: Reading `vlenb` CSR is side-effect-free on RVV 1.0 harts with VS enabled.
        unsafe {
            core::arch::asm!("csrr {}, vlenb", out(reg) vlenb, options(nomem, nostack, preserves_flags));
        }
        vlenb >= 128
    }
    #[cfg(not(all(target_arch = "riscv64", feature = "simd")))]
    {
        false
    }
}

/// 16-way lock-step parallel Blake2b compression.
/// On SpacemiT K3 A100 cores (`VLEN=1024`, `vlenb=128`), a single `LMUL=1` vector register
/// holds `16 x 64-bit` lanes (`v0..v15`), compressing 16 independent 128-byte blocks
/// simultaneously with `Zvkb` `vror.vi` rotations and zero register spills in the exact
/// same instruction count as `compress_four_blocks`.
/// On `VLEN=256` cores or host targets, transparently executes 4 calls to `compress_four_blocks`.
pub fn compress_16_blocks(
    h: &mut [&mut [u64; 8]; 16],
    blocks: &[&[u8; 128]; 16],
    t: &[u128; 16],
    last: &[bool; 16],
) {
    #[cfg(all(target_arch = "riscv64", feature = "simd"))]
    if has_vlen_1024() {
        #[repr(align(64))]
        struct AlignedBuffer16([u64; 16 * 16]);

        let mut v_buf = AlignedBuffer16([0u64; 16 * 16]);
        let v = &mut v_buf.0;
        for j in 0..16 {
            for i in 0..8 {
                v[i * 16 + j] = h[j][i];
            }
            v[8 * 16 + j] = IV[0];
            v[9 * 16 + j] = IV[1];
            v[10 * 16 + j] = IV[2];
            v[11 * 16 + j] = IV[3];
            v[12 * 16 + j] = IV[4] ^ (t[j] as u64);
            v[13 * 16 + j] = IV[5] ^ ((t[j] >> 64) as u64);
            v[14 * 16 + j] = if last[j] { !IV[6] } else { IV[6] };
            v[15 * 16 + j] = IV[7];
        }

        let mut m_buf = AlignedBuffer16([0u64; 16 * 16]);
        let m = &mut m_buf.0;
        for j in 0..16 {
            for i in 0..16 {
                let off = i * 8;
                m[i * 16 + j] = u64::from_le_bytes(blocks[j][off..off + 8].try_into().unwrap());
            }
        }

        let mut msg_ptrs = [0u64; 13 * 16];
        let m_ptr_val = m.as_ptr() as u64;
        for round in 0..12 {
            let s = &SIGMA[round];
            for i in 0..16 {
                msg_ptrs[round * 16 + i] = m_ptr_val + (s[i] * 128) as u64;
            }
        }
        for i in 0..16 {
            msg_ptrs[12 * 16 + i] = m_ptr_val;
        }

        // SAFETY: `has_vlen_1024()` verified `vlenb >= 128`, so `vsetivli zero, 16, e64, m1`
        // configures `VL = 16` (`16 * 8 = 128` bytes per `LMUL=1` vector register `v0..v15`).
        // All `v_ptr` and `msg_ptrs` accesses are in-bounds of `AlignedBuffer16`.
        unsafe {
            let offsets_ptr = msg_ptrs.as_ptr();
            let v_ptr = v.as_mut_ptr();

            asm!(
                "vsetivli zero, 16, e64, m1",

                "vle64.v v0, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v1, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v2, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v3, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v4, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v5, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v6, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v7, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v8, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v9, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v10, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v11, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v12, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v13, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v14, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vle64.v v15, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, -1920",

                "ld {x_ptr}, 0({offsets_ptr})",
                "ld {y_ptr}, 8({offsets_ptr})",
                "vle64.v v30, ({x_ptr})",

                "li {round_idx}, 0",

                "2:",
                // --- Step 1: g(0, 4, 8, 12) ---
                "ld {x_ptr}, 16({offsets_ptr})",
                "vadd.vv v0, v0, v4",
                "vadd.vv v0, v0, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 24({offsets_ptr})",
                "vxor.vv v12, v12, v0",
                "vror.vi v12, v12, 32",
                "vadd.vv v8, v8, v12",
                "vxor.vv v4, v4, v8",
                "vror.vi v4, v4, 24",
                "vadd.vv v0, v0, v4",
                "vadd.vv v0, v0, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v12, v12, v0",
                "vror.vi v12, v12, 16",
                "vadd.vv v8, v8, v12",
                "vxor.vv v4, v4, v8",
                "vror.vi v4, v4, 63",

                // --- Step 2: g(1, 5, 9, 13) ---
                "ld {x_ptr}, 32({offsets_ptr})",
                "vadd.vv v1, v1, v5",
                "vadd.vv v1, v1, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 40({offsets_ptr})",
                "vxor.vv v13, v13, v1",
                "vror.vi v13, v13, 32",
                "vadd.vv v9, v9, v13",
                "vxor.vv v5, v5, v9",
                "vror.vi v5, v5, 24",
                "vadd.vv v1, v1, v5",
                "vadd.vv v1, v1, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v13, v13, v1",
                "vror.vi v13, v13, 16",
                "vadd.vv v9, v9, v13",
                "vxor.vv v5, v5, v9",
                "vror.vi v5, v5, 63",

                // --- Step 3: g(2, 6, 10, 14) ---
                "ld {x_ptr}, 48({offsets_ptr})",
                "vadd.vv v2, v2, v6",
                "vadd.vv v2, v2, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 56({offsets_ptr})",
                "vxor.vv v14, v14, v2",
                "vror.vi v14, v14, 32",
                "vadd.vv v10, v10, v14",
                "vxor.vv v6, v6, v10",
                "vror.vi v6, v6, 24",
                "vadd.vv v2, v2, v6",
                "vadd.vv v2, v2, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v14, v14, v2",
                "vror.vi v14, v14, 16",
                "vadd.vv v10, v10, v14",
                "vxor.vv v6, v6, v10",
                "vror.vi v6, v6, 63",

                // --- Step 4: g(3, 7, 11, 15) ---
                "ld {x_ptr}, 64({offsets_ptr})",
                "vadd.vv v3, v3, v7",
                "vadd.vv v3, v3, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 72({offsets_ptr})",
                "vxor.vv v15, v15, v3",
                "vror.vi v15, v15, 32",
                "vadd.vv v11, v11, v15",
                "vxor.vv v7, v7, v11",
                "vror.vi v7, v7, 24",
                "vadd.vv v3, v3, v7",
                "vadd.vv v3, v3, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v15, v15, v3",
                "vror.vi v15, v15, 16",
                "vadd.vv v11, v11, v15",
                "vxor.vv v7, v7, v11",
                "vror.vi v7, v7, 63",

                // --- Step 5: g(0, 5, 10, 15) ---
                "ld {x_ptr}, 80({offsets_ptr})",
                "vadd.vv v0, v0, v5",
                "vadd.vv v0, v0, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 88({offsets_ptr})",
                "vxor.vv v15, v15, v0",
                "vror.vi v15, v15, 32",
                "vadd.vv v10, v10, v15",
                "vxor.vv v5, v5, v10",
                "vror.vi v5, v5, 24",
                "vadd.vv v0, v0, v5",
                "vadd.vv v0, v0, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v15, v15, v0",
                "vror.vi v15, v15, 16",
                "vadd.vv v10, v10, v15",
                "vxor.vv v5, v5, v10",
                "vror.vi v5, v5, 63",

                // --- Step 6: g(1, 6, 11, 12) ---
                "ld {x_ptr}, 96({offsets_ptr})",
                "vadd.vv v1, v1, v6",
                "vadd.vv v1, v1, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 104({offsets_ptr})",
                "vxor.vv v12, v12, v1",
                "vror.vi v12, v12, 32",
                "vadd.vv v11, v11, v12",
                "vxor.vv v6, v6, v11",
                "vror.vi v6, v6, 24",
                "vadd.vv v1, v1, v6",
                "vadd.vv v1, v1, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v12, v12, v1",
                "vror.vi v12, v12, 16",
                "vadd.vv v11, v11, v12",
                "vxor.vv v6, v6, v11",
                "vror.vi v6, v6, 63",

                // --- Step 7: g(2, 7, 8, 13) ---
                "ld {x_ptr}, 112({offsets_ptr})",
                "vadd.vv v2, v2, v7",
                "vadd.vv v2, v2, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 120({offsets_ptr})",
                "vxor.vv v13, v13, v2",
                "vror.vi v13, v13, 32",
                "vadd.vv v8, v8, v13",
                "vxor.vv v7, v7, v8",
                "vror.vi v7, v7, 24",
                "vadd.vv v2, v2, v7",
                "vadd.vv v2, v2, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v13, v13, v2",
                "vror.vi v13, v13, 16",
                "vadd.vv v8, v8, v13",
                "vxor.vv v7, v7, v8",
                "vror.vi v7, v7, 63",

                // --- Step 8: g(3, 4, 9, 14) ---
                "addi {offsets_ptr}, {offsets_ptr}, 128",
                "ld {x_ptr}, 0({offsets_ptr})",
                "vadd.vv v3, v3, v4",
                "vadd.vv v3, v3, v30",
                "vle64.v v30, ({y_ptr})",
                "ld {y_ptr}, 8({offsets_ptr})",
                "vxor.vv v14, v14, v3",
                "vror.vi v14, v14, 32",
                "vadd.vv v9, v9, v14",
                "vxor.vv v4, v4, v9",
                "vror.vi v4, v4, 24",
                "vadd.vv v3, v3, v4",
                "vadd.vv v3, v3, v30",
                "vle64.v v30, ({x_ptr})",
                "vxor.vv v14, v14, v3",
                "vror.vi v14, v14, 16",
                "vadd.vv v9, v9, v14",
                "vxor.vv v4, v4, v9",
                "vror.vi v4, v4, 63",

                "addi {round_idx}, {round_idx}, 1",
                "li {max_rounds}, 12",
                "blt {round_idx}, {max_rounds}, 2b",

                "vse64.v v0, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v1, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v2, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v3, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v4, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v5, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v6, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v7, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v8, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v9, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v10, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v11, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v12, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v13, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v14, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, 128",
                "vse64.v v15, ({v_ptr})",
                "addi {v_ptr}, {v_ptr}, -1920",

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
                out("v30") _,
            );
        }

        for j in 0..16 {
            for i in 0..8 {
                h[j][i] ^= v[i * 16 + j] ^ v[(i + 8) * 16 + j];
            }
        }
        return;
    }

    for chunk in 0..4 {
        let base = chunk * 4;
        let [h0, h1, h2, h3] = &mut h[base..base + 4] else { unreachable!() };
        let mut h_sub = [&mut **h0, &mut **h1, &mut **h2, &mut **h3];
        let b_sub = [blocks[base], blocks[base + 1], blocks[base + 2], blocks[base + 3]];
        let t_sub = [t[base], t[base + 1], t[base + 2], t[base + 3]];
        let l_sub = [last[base], last[base + 1], last[base + 2], last[base + 3]];
        compress_four_blocks(&mut h_sub, &b_sub, &t_sub, &l_sub);
    }
}

pub fn hash_batch_16(inputs: [&[u8]; 16]) -> [[u8; 32]; 16] {
    let mut h = [IV; 16];
    for i in 0..16 {
        h[i][0] ^= 0x01010020;
    }

    hash_batch_16_impl(&mut h, inputs);

    let mut out = [[0u8; 32]; 16];
    for j in 0..16 {
        for i in 0..4 {
            let word = h[j][i];
            out[j][i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
    }
    out
}

pub fn hash64_batch_16(inputs: [&[u8]; 16]) -> [[u8; 64]; 16] {
    let mut h = [IV; 16];
    for i in 0..16 {
        h[i][0] ^= 0x01010040;
    }

    hash_batch_16_impl(&mut h, inputs);

    let mut out = [[0u8; 64]; 16];
    for j in 0..16 {
        for i in 0..8 {
            let word = h[j][i];
            out[j][i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
    }
    out
}

fn hash_batch_16_impl(h: &mut [[u64; 8]; 16], inputs: [&[u8]; 16]) {
    let len0 = inputs[0].len();
    let all_same_len = inputs.iter().all(|inp| inp.len() == len0);
    if all_same_len {
        let mut offset = 0;
        let mut t = [0u128; 16];
        let mut remaining = len0;

        let [
            h0, h1, h2, h3, h4, h5, h6, h7,
            h8, h9, h10, h11, h12, h13, h14, h15,
        ] = h;
        let mut h_refs = [
            h0, h1, h2, h3, h4, h5, h6, h7,
            h8, h9, h10, h11, h12, h13, h14, h15,
        ];

        while remaining > 128 {
            let mut blocks = [[0u8; 128]; 16];
            for j in 0..16 {
                blocks[j].copy_from_slice(&inputs[j][offset..offset + 128]);
                t[j] += 128;
            }
            let blocks_refs = [
                &blocks[0], &blocks[1], &blocks[2], &blocks[3],
                &blocks[4], &blocks[5], &blocks[6], &blocks[7],
                &blocks[8], &blocks[9], &blocks[10], &blocks[11],
                &blocks[12], &blocks[13], &blocks[14], &blocks[15],
            ];
            let last = [false; 16];
            compress_16_blocks(&mut h_refs, &blocks_refs, &t, &last);
            offset += 128;
            remaining -= 128;
        }

        let mut blocks = [[0u8; 128]; 16];
        for j in 0..16 {
            blocks[j][..remaining].copy_from_slice(&inputs[j][offset..offset + remaining]);
            t[j] += remaining as u128;
        }
        let blocks_refs = [
            &blocks[0], &blocks[1], &blocks[2], &blocks[3],
            &blocks[4], &blocks[5], &blocks[6], &blocks[7],
            &blocks[8], &blocks[9], &blocks[10], &blocks[11],
            &blocks[12], &blocks[13], &blocks[14], &blocks[15],
        ];
        let last = [true; 16];
        compress_16_blocks(&mut h_refs, &blocks_refs, &t, &last);
    } else {
        for j in 0..16 {
            let outlen = ((h[j][0] ^ IV[0]) & 0xff) as usize;
            let mut hasher = Blake2bHasher::new(outlen);
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

/// Diagnostic for the 4-way kernel: returns (lane mismatch mask, lane-0 scalar result,
/// lane-0 vector result) for four distinct blocks compressed via `compress_four_blocks`.
#[cfg(all(target_arch = "riscv64", feature = "simd"))]
pub fn four_way_debug() -> (u8, [u64; 8], [u64; 8]) {
    let mut blocks = [[0u8; 128]; 4];
    for j in 0..4 {
        for i in 0..128 {
            blocks[j][i] = (i as u8).wrapping_mul(17).wrapping_add(j as u8 * 41 + 3);
        }
    }
    let mut exp = [IV; 4];
    for j in 0..4 {
        exp[j][0] ^= 0x01010020;
    }
    let mut got = exp;
    let t = [40u128; 4];
    let last = [true; 4];
    for j in 0..4 {
        compress(&mut exp[j], &blocks[j], t[j], last[j]);
    }
    let [g0, g1, g2, g3] = &mut got;
    let mut h_refs = [g0, g1, g2, g3];
    let blocks_refs = [&blocks[0], &blocks[1], &blocks[2], &blocks[3]];
    compress_four_blocks(&mut h_refs, &blocks_refs, &t, &last);
    let mut mask = 0u8;
    for j in 0..4 {
        if exp[j] != got[j] {
            mask |= 1 << j;
        }
    }
    (mask, exp[0], got[0])
}

/// Batched Blake2b with an 8-byte digest (Nano work/PoW). Returns the digest as a
/// little-endian u64 per input, identical to `Blake2bHasher::new(8)` + first 8 bytes.
pub fn hash8_batch_4(inputs: [&[u8]; 4]) -> [u64; 4] {
    let mut h = [IV; 4];
    for i in 0..4 {
        h[i][0] ^= 0x01010008;
    }

    hash_batch_impl(&mut h, inputs);

    [h[0][0], h[1][0], h[2][0], h[3][0]]
}

/// 16-way batched Blake2b with an 8-byte digest (Nano work/PoW) for SpacemiT K3 A100
/// (`VLEN=1024`) cores. Returns the 8-byte digest as a little-endian `u64` per input.
pub fn hash8_batch_16(inputs: [&[u8]; 16]) -> [u64; 16] {
    let mut h = [IV; 16];
    for i in 0..16 {
        h[i][0] ^= 0x01010008;
    }

    hash_batch_16_impl(&mut h, inputs);

    core::array::from_fn(|i| h[i][0])
}

/// Specialized zero-allocation 16-lane Nano PoW nonce search (`Blake2b-64(nonce_le || root)`).
/// Keeps the 32-byte `root` pre-packed across all 16 lanes and tests `16 * iterations`
/// consecutive nonces starting at `start_nonce`, returning the first nonce `>= min_difficulty`.
pub fn pow_search_16(
    root: &[u8; 32],
    start_nonce: u64,
    min_difficulty: u64,
    iterations: u32,
) -> Option<u64> {
    let mut blocks = [[0u8; 128]; 16];
    for j in 0..16 {
        blocks[j][8..40].copy_from_slice(root);
    }
    let t = [40u128; 16];
    let last = [true; 16];
    let init_h0 = IV[0] ^ 0x01010008;

    let mut base_nonce = start_nonce;
    for _ in 0..iterations {
        let mut h = [IV; 16];
        for j in 0..16 {
            h[j][0] = init_h0;
            blocks[j][0..8].copy_from_slice(&base_nonce.wrapping_add(j as u64).to_le_bytes());
        }
        let [
            h0, h1, h2, h3, h4, h5, h6, h7,
            h8, h9, h10, h11, h12, h13, h14, h15,
        ] = &mut h;
        let mut h_refs = [
            h0, h1, h2, h3, h4, h5, h6, h7,
            h8, h9, h10, h11, h12, h13, h14, h15,
        ];
        let blocks_refs = [
            &blocks[0], &blocks[1], &blocks[2], &blocks[3],
            &blocks[4], &blocks[5], &blocks[6], &blocks[7],
            &blocks[8], &blocks[9], &blocks[10], &blocks[11],
            &blocks[12], &blocks[13], &blocks[14], &blocks[15],
        ];
        compress_16_blocks(&mut h_refs, &blocks_refs, &t, &last);

        for j in 0..16 {
            if h_refs[j][0] >= min_difficulty {
                return Some(base_nonce.wrapping_add(j as u64));
            }
        }
        base_nonce = base_nonce.wrapping_add(16);
    }
    None
}

/// On-device consistency check of the vector kernels against the scalar reference.
/// Returns a bitmask: bit0 = single-block vector mismatch, bit1 = 4-way batch mismatch,
/// bit2 = 8-byte batch mismatch, bit3 = 16-way batch mismatch. 0 means all kernels match.
#[cfg(all(target_arch = "riscv64", feature = "simd"))]
pub fn self_test() -> u8 {
    let mut fail = 0u8;

    let mut block = [0u8; 128];
    for (i, b) in block.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    let mut h_ref = IV;
    h_ref[0] ^= 0x01010020;
    let mut h_vec = h_ref;
    compress(&mut h_ref, &block, 128, true);
    compress_vector(&mut h_vec, &block, 128, true);
    if h_ref != h_vec {
        fail |= 1;
    }

    let mut data = [[0u8; 40]; 4];
    for j in 0..4 {
        for i in 0..40 {
            data[j][i] = (i as u8).wrapping_mul(13).wrapping_add(j as u8 * 59 + 1);
        }
    }
    let refs = [&data[0][..], &data[1][..], &data[2][..], &data[3][..]];
    let batch = hash_batch_4(refs);
    let batch8 = hash8_batch_4(refs);
    for j in 0..4 {
        if batch[j] != hash(refs[j]) {
            fail |= 2;
        }
        let mut hasher = Blake2bHasher::new(8);
        hasher.update(refs[j]);
        let full = hasher.finalize();
        if batch8[j] != u64::from_le_bytes(full[..8].try_into().unwrap()) {
            fail |= 4;
        }
    }

    let mut data16 = [[0u8; 40]; 16];
    for j in 0..16 {
        for i in 0..40 {
            data16[j][i] = (i as u8).wrapping_mul(19).wrapping_add(j as u8 * 37 + 5);
        }
    }
    let refs16: [&[u8]; 16] = core::array::from_fn(|j| &data16[j][..]);
    let batch16 = hash_batch_16(refs16);
    let batch8_16 = hash8_batch_16(refs16);
    for j in 0..16 {
        if batch16[j] != hash(refs16[j]) {
            fail |= 8;
        }
        let mut hasher = Blake2bHasher::new(8);
        hasher.update(refs16[j]);
        let full = hasher.finalize();
        if batch8_16[j] != u64::from_le_bytes(full[..8].try_into().unwrap()) {
            fail |= 8;
        }
    }
    fail
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
            // Digest length is encoded in the low byte of the parameter-block XOR on h[0].
            let outlen = ((h[j][0] ^ IV[0]) & 0xff) as usize;
            let mut hasher = Blake2bHasher::new(outlen);
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
    fn test_hash64_batch_16() {
        let mut payloads = [[0u8; 96]; 16];
        for i in 0..16 {
            for b in 0..96 {
                payloads[i][b] = ((i * 37 + b * 13 + 5) & 0xff) as u8;
            }
        }
        let refs = [
            &payloads[0][..], &payloads[1][..], &payloads[2][..], &payloads[3][..],
            &payloads[4][..], &payloads[5][..], &payloads[6][..], &payloads[7][..],
            &payloads[8][..], &payloads[9][..], &payloads[10][..], &payloads[11][..],
            &payloads[12][..], &payloads[13][..], &payloads[14][..], &payloads[15][..],
        ];
        let got64 = hash64_batch_16(refs);
        let got32 = hash_batch_16(refs);
        let got8 = hash8_batch_16(refs);
        for i in 0..16 {
            assert_eq!(got64[i], hash64(refs[i]));
            assert_eq!(got32[i], hash(refs[i]));
            let mut h8 = Blake2bHasher::new(8);
            h8.update(refs[i]);
            let full8 = h8.finalize();
            assert_eq!(got8[i], u64::from_le_bytes(full8[..8].try_into().unwrap()));
        }

        let root = [0x42u8; 32];
        let threshold = 0xff00_0000_0000_0000u64;
        let found = pow_search_16(&root, 1, threshold, 512).expect("pow_search_16 should find nonce");
        let mut check = Blake2bHasher::new(8);
        check.update(&found.to_le_bytes());
        check.update(&root);
        let digest = u64::from_le_bytes(check.finalize()[..8].try_into().unwrap());
        assert!(digest >= threshold);
    }

    #[test]
    fn test_blake2b_zeros() {
        let data = [0u8; 36]; // 32 byte seed + 4 byte index
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

