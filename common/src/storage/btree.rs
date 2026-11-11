//! Slotted-Page B-Tree Storage Engine
//!
//! Built directly on top of our bare-metal Buffer Pool Manager (BPM), where every node
//! corresponds to one 4 KiB (`BLOCK_SIZE = 4096`) hardware sector block.
//!
//! ### Slotted-Page Node Layout (4096 bytes)
//! - `data[0]`: `is_leaf` (`1` = leaf, `0` = internal)
//! - `data[1..3]`: `num_keys: u16` (LE)
//! - `data[3..5]`: `free_end: u16` (LE, downward-growing cell heap start offset; `4096` when empty)
//! - `data[5..7]`: `dead_bytes: u16` (LE, fragmented bytes inside `free_end..4096`)
//! - `data[7]`: `flags: u8` (`0` = legacy slotted page, `1` = `PAGE_FLAG_SOA` with inline SoA `u32` key heads)
//! - Directory starting at byte `8`:
//!   - Leaf (`is_leaf == true`): `slot[i]: u16` at `8 + i * 2` (`0 <= i < num_keys`), followed by
//!     4-byte-aligned `heads[i]: u32` at `((8 + num_keys * 2 + 3) & !3) + i * 4`.
//!   - Internal (`is_leaf == false`): interleaved `child[i]: u32` at `8 + i * 6` (`0 <= i <= num_keys`)
//!     and `slot[i]: u16` at `12 + i * 6` (`0 <= i < num_keys`), followed by 4-byte-aligned
//!     `heads[i]: u32` at `((12 + num_keys * 6 + 3) & !3) + i * 4`.
//! - Cell at `cell_off` (`free_end <= cell_off < 4096`):
//!   - `data[cell_off]`: `key_len: u8` (`33` when `key[33..65] == [0; 32]`, else `65`)
//!   - `data[cell_off + 1..cell_off + 3]`: `val_len: u16` (LE, `0..=VALUE_SIZE`)
//!   - `data[cell_off + 3..cell_off + 3 + key_len]`: key bytes (`33` or `65` bytes)
//!   - `data[cell_off + 3 + key_len..cell_off + 3 + key_len + val_len]`: value bytes (`val_len` bytes)

use crate::storage::bpm::{BufferPool, BLOCK_SIZE};
extern crate alloc;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Maximum number of keys a single slotted B-Tree node may hold.
pub const MAX_KEYS: usize = 64;
/// Logical key size passed by callers (`1` byte table ID + `64` bytes key).
pub const KEY_SIZE: usize = 65;
/// Maximum value size in bytes (`SavedBlock` max 235B + 32B embedded successor = 267B).
pub const VALUE_SIZE: usize = 272;

/// Worst-case bytes required to insert one maximum-sized entry into a node:
/// `12` bytes max internal directory + alignment + `u32` head + `3` bytes cell header + `65`B key + `272`B value.
const MAX_INSERT_BYTES: usize = 12 + 3 + KEY_SIZE + VALUE_SIZE; // 352 bytes

const BTREE_META_MAGIC: u32 = 0xBEEFCB03;

/// Flag stored in `data[7]` indicating that the 4-byte-aligned SoA `u32` key-head array
/// (`heads[0..num_keys]`) immediately following the slot directory is populated.
const PAGE_FLAG_SOA: u8 = 1;

/// Extracts the first 8 bytes of a key as a big-endian `u64` (`ld` + `rev8` on RISC-V `Zbb`),
/// preserving lexicographical byte ordering in a single 64-bit register.
#[inline(always)]
fn key_head(k: &[u8]) -> u64 {
    u64::from_be_bytes([k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7]])
}

/// Extracts the first 4 bytes of a key as a big-endian `u32` (`table_id: u8` + 24-bit key prefix),
/// stored in the page's SoA `heads` directory for single-instruction RVV `e32` vector search.
#[inline(always)]
fn key_head32(k: &[u8]) -> u32 {
    u32::from_be_bytes([k[0], k[1], k[2], k[3]])
}

/// Losslessly compacts a 65-byte key to 33 bytes when bytes `33..65` are all zero.
#[inline(always)]
fn key_compact_slice(key: &[u8; KEY_SIZE]) -> &[u8] {
    if key[33..KEY_SIZE] == [0u8; 32] {
        &key[..33]
    } else {
        &key[..KEY_SIZE]
    }
}

/// Expands a stored cell key (`33` or `65` bytes) back into a full `[u8; KEY_SIZE]`.
#[inline(always)]
fn expand_cell_key(cell_key: &[u8]) -> [u8; KEY_SIZE] {
    let mut k = [0u8; KEY_SIZE];
    let n = cell_key.len().min(KEY_SIZE);
    k[..n].copy_from_slice(&cell_key[..n]);
    k
}

/// Compares a stored cell key (`33` or `65` bytes) against a 65-byte target key and its precomputed head.
#[inline(always)]
fn cmp_cell_key(cell_key: &[u8], target: &[u8; KEY_SIZE], target_head: u64) -> Ordering {
    let ch = key_head(cell_key);
    if ch != target_head {
        return ch.cmp(&target_head);
    }
    let prefix_cmp = cell_key[8..33].cmp(&target[8..33]);
    if prefix_cmp != Ordering::Equal {
        return prefix_cmp;
    }
    if cell_key.len() == KEY_SIZE {
        cell_key[33..KEY_SIZE].cmp(&target[33..KEY_SIZE])
    } else if target[33..KEY_SIZE] == [0u8; 32] {
        Ordering::Equal
    } else {
        Ordering::Less
    }
}

// ── Slotted-Page Low-Level Accessors ─────────────────────────────────────────

#[inline(always)]
fn page_init(data: &mut [u8; BLOCK_SIZE], is_leaf: bool) {
    data[0] = if is_leaf { 1 } else { 0 };
    data[1..3].copy_from_slice(&0u16.to_le_bytes());
    data[3..5].copy_from_slice(&(BLOCK_SIZE as u16).to_le_bytes());
    data[5..16].fill(0);
    data[7] = PAGE_FLAG_SOA;
}

#[inline(always)]
fn page_is_leaf(data: &[u8; BLOCK_SIZE]) -> bool {
    data[0] != 0
}

#[inline(always)]
fn page_num_keys(data: &[u8; BLOCK_SIZE]) -> usize {
    (u16::from_le_bytes([data[1], data[2]]) as usize).min(MAX_KEYS)
}

#[inline(always)]
fn page_set_num_keys(data: &mut [u8; BLOCK_SIZE], nk: usize) {
    data[1..3].copy_from_slice(&(nk as u16).to_le_bytes());
}

#[inline(always)]
fn page_free_end(data: &[u8; BLOCK_SIZE]) -> usize {
    let fe = u16::from_le_bytes([data[3], data[4]]) as usize;
    if fe == 0 || fe > BLOCK_SIZE {
        BLOCK_SIZE
    } else {
        fe
    }
}

#[inline(always)]
fn page_set_free_end(data: &mut [u8; BLOCK_SIZE], fe: usize) {
    data[3..5].copy_from_slice(&(fe as u16).to_le_bytes());
}

#[inline(always)]
fn page_dead_bytes(data: &[u8; BLOCK_SIZE]) -> usize {
    u16::from_le_bytes([data[5], data[6]]) as usize
}

#[inline(always)]
fn page_set_dead_bytes(data: &mut [u8; BLOCK_SIZE], dead: usize) {
    data[5..7].copy_from_slice(&(dead as u16).to_le_bytes());
}

/// Returns the 4-byte-aligned starting offset of the SoA `heads: [u32; num_keys]` array,
/// located immediately after the slot/child directory.
#[inline(always)]
fn page_heads_base(is_leaf: bool, num_keys: usize) -> usize {
    let raw = if is_leaf {
        8 + num_keys * 2
    } else {
        12 + num_keys * 6
    };
    (raw + 3) & !3
}

#[inline(always)]
fn page_free_start(is_leaf: bool, num_keys: usize) -> usize {
    page_heads_base(is_leaf, num_keys) + num_keys * 4
}

#[inline(always)]
fn page_slot_off(is_leaf: bool, idx: usize) -> usize {
    if is_leaf {
        8 + idx * 2
    } else {
        12 + idx * 6
    }
}

#[inline(always)]
fn page_get_slot(data: &[u8; BLOCK_SIZE], is_leaf: bool, idx: usize) -> usize {
    let o = page_slot_off(is_leaf, idx);
    u16::from_le_bytes([data[o], data[o + 1]]) as usize
}

#[inline(always)]
fn page_set_slot(data: &mut [u8; BLOCK_SIZE], is_leaf: bool, idx: usize, cell_off: usize) {
    let o = page_slot_off(is_leaf, idx);
    data[o..o + 2].copy_from_slice(&(cell_off as u16).to_le_bytes());
}

#[inline(always)]
fn page_get_child(data: &[u8; BLOCK_SIZE], idx: usize) -> u32 {
    let o = 8 + idx * 6;
    u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
}

#[inline(always)]
fn page_set_child(data: &mut [u8; BLOCK_SIZE], idx: usize, child_id: u32) {
    let o = 8 + idx * 6;
    data[o..o + 4].copy_from_slice(&child_id.to_le_bytes());
}

#[inline(always)]
fn page_cell(data: &[u8; BLOCK_SIZE], cell_off: usize) -> (&[u8], &[u8]) {
    let klen = data[cell_off] as usize;
    let vlen = u16::from_le_bytes([data[cell_off + 1], data[cell_off + 2]]) as usize;
    let kstart = cell_off + 3;
    let vstart = kstart + klen;
    (&data[kstart..vstart], &data[vstart..vstart + vlen])
}

#[inline(always)]
fn page_cell_len(data: &[u8; BLOCK_SIZE], cell_off: usize) -> usize {
    let klen = data[cell_off] as usize;
    let vlen = u16::from_le_bytes([data[cell_off + 1], data[cell_off + 2]]) as usize;
    3 + klen + vlen
}

#[inline(always)]
fn page_usable_free(data: &[u8; BLOCK_SIZE]) -> usize {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let fs = page_free_start(is_leaf, nk);
    let fe = page_free_end(data);
    let dead = page_dead_bytes(data);
    (fe + dead).saturating_sub(fs)
}

#[inline(always)]
fn page_is_full(data: &[u8; BLOCK_SIZE]) -> bool {
    page_num_keys(data) >= MAX_KEYS || page_usable_free(data) < MAX_INSERT_BYTES
}

/// Ensures that `data` has its inline SoA `u32` key-head directory (`PAGE_FLAG_SOA`) populated.
/// Legacy pages (`data[7] == 0`) loaded from NVMe are upgraded in place in RAM on first access.
#[inline]
fn page_ensure_soa(data: &mut [u8; BLOCK_SIZE]) -> bool {
    if data[7] == PAGE_FLAG_SOA {
        return true;
    }
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let fs = page_free_start(is_leaf, nk);
    if page_free_end(data) < fs {
        if page_free_end(data) + page_dead_bytes(data) >= fs {
            page_compact_skip(data, None);
        }
        if page_free_end(data) < fs {
            return false;
        }
    }
    let hb = page_heads_base(is_leaf, nk);
    for i in 0..nk {
        let cell_off = page_get_slot(data, is_leaf, i);
        let h32 = key_head32(&data[cell_off + 3..cell_off + 7]);
        let o = hb + i * 4;
        data[o..o + 4].copy_from_slice(&h32.to_le_bytes());
    }
    data[7] = PAGE_FLAG_SOA;
    true
}

/// RVV 1.0 SIMD key-head search across `heads[0..nk]` (`e32, m8`).
/// Returns `(lo, hi)` where `lo` is the count of keys with `head32 < target_head32`
/// and `hi` is the count of keys with `head32 <= target_head32`.
///
/// # Safety
/// `heads_ptr` must be 4-byte aligned and valid for `nk * 4` bytes of read access (`1 <= nk <= MAX_KEYS`).
#[cfg(target_arch = "riscv64")]
#[inline(always)]
unsafe fn rvv_search_heads32(
    heads_ptr: *const u8,
    nk: usize,
    target_head32: u32,
) -> (usize, usize) {
    let mut lo = 0usize;
    let mut hi = 0usize;
    let th = target_head32 as usize;
    core::arch::asm!(
        "2:",
        "vsetvli {vl}, {rem}, e32, m8, ta, ma",
        "vle32.v v8, ({ptr})",
        "vmsltu.vx v0, v8, {th}",
        "vcpop.m {tmp}, v0",
        "add {lo}, {lo}, {tmp}",
        "vmsleu.vx v0, v8, {th}",
        "vcpop.m {tmp}, v0",
        "add {hi}, {hi}, {tmp}",
        "sub {rem}, {rem}, {vl}",
        "sh2add {ptr}, {vl}, {ptr}",
        "bnez {rem}, 2b",
        ptr = inout(reg) heads_ptr => _,
        rem = inout(reg) nk => _,
        lo = inout(reg) lo,
        hi = inout(reg) hi,
        th = in(reg) th,
        vl = out(reg) _,
        tmp = out(reg) _,
        out("v0") _,
        out("v8") _,
        out("v9") _,
        out("v10") _,
        out("v11") _,
        out("v12") _,
        out("v13") _,
        out("v14") _,
        out("v15") _,
        options(nostack, readonly),
    );
    (lo, hi)
}

/// Searches `data` (`0..num_keys`) for `key`. Returns `(lower_bound_idx, is_equal)`.
#[inline]
fn page_lower_bound(data: &[u8; BLOCK_SIZE], key: &[u8; KEY_SIZE], target_head: u64) -> (usize, bool) {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    if nk == 0 {
        return (0, false);
    }
    let (mut lo, mut hi) = if data[7] == PAGE_FLAG_SOA {
        let hb = page_heads_base(is_leaf, nk);
        let target_head32 = (target_head >> 32) as u32;
        #[cfg(target_arch = "riscv64")]
        // SAFETY: `hb` is 4-byte aligned and `hb + nk * 4 <= BLOCK_SIZE`.
        let (l, h) = unsafe { rvv_search_heads32(data.as_ptr().add(hb), nk, target_head32) };
        #[cfg(not(target_arch = "riscv64"))]
        let (l, h) = {
            let mut l = 0usize;
            let mut h = 0usize;
            for i in 0..nk {
                let o = hb + i * 4;
                let v = u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
                l += (v < target_head32) as usize;
                h += (v <= target_head32) as usize;
            }
            (l, h)
        };
        if l == h {
            return (l, false);
        }
        if l + 1 == h {
            let cell_off = page_get_slot(data, is_leaf, l);
            let (ck, _) = page_cell(data, cell_off);
            return match cmp_cell_key(ck, key, target_head) {
                Ordering::Less => (l + 1, false),
                Ordering::Equal => (l, true),
                Ordering::Greater => (l, false),
            };
        }
        (l, h)
    } else {
        (0usize, nk)
    };
    let base = data.as_ptr();
    while lo < hi {
        let mid = (lo + hi) >> 1;
        let cell_off = page_get_slot(data, is_leaf, mid);
        if hi - lo > 2 {
            let left_mid = (lo + mid) >> 1;
            let right_mid = (mid + 1 + hi) >> 1;
            let left_off = page_get_slot(data, is_leaf, left_mid) & (BLOCK_SIZE - 1);
            // SAFETY: `left_off` and `right_off` are masked to `< BLOCK_SIZE`, and `prefetch_read`
            // is a pure hardware cache hint that never traps.
            unsafe {
                crate::storage::prefetch_read(base.add(left_off));
                if right_mid < hi {
                    let right_off = page_get_slot(data, is_leaf, right_mid) & (BLOCK_SIZE - 1);
                    crate::storage::prefetch_read(base.add(right_off));
                }
            }
        }
        let (ck, _) = page_cell(data, cell_off);
        match cmp_cell_key(ck, key, target_head) {
            Ordering::Less => lo = mid + 1,
            Ordering::Equal => return (mid, true),
            Ordering::Greater => hi = mid,
        }
    }
    (lo, false)
}

/// Compacts all live cells (`0..num_keys`, optionally skipping `skip_idx`) tightly to the top of `data`.
fn page_compact_skip(data: &mut [u8; BLOCK_SIZE], skip_idx: Option<usize>) {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let mut cell_buf = core::mem::MaybeUninit::<[u8; BLOCK_SIZE]>::uninit();
    // SAFETY: Every byte in `buf[fe..BLOCK_SIZE]` is written before being copied back to `data[fe..BLOCK_SIZE]`.
    let buf = unsafe { &mut *cell_buf.as_mut_ptr() };
    let mut fe = BLOCK_SIZE;
    for i in 0..nk {
        if Some(i) == skip_idx {
            continue;
        }
        let old_off = page_get_slot(data, is_leaf, i);
        let clen = page_cell_len(data, old_off);
        fe -= clen;
        buf[fe..fe + clen].copy_from_slice(&data[old_off..old_off + clen]);
        page_set_slot(data, is_leaf, i, fe);
    }
    data[fe..BLOCK_SIZE].copy_from_slice(&buf[fe..BLOCK_SIZE]);
    page_set_free_end(data, fe);
    page_set_dead_bytes(data, 0);
}

#[inline]
fn page_alloc_cell(
    data: &mut [u8; BLOCK_SIZE],
    min_free_start: usize,
    key_slice: &[u8],
    value: &[u8],
    skip_idx: Option<usize>,
) -> usize {
    let klen = key_slice.len();
    let vlen = value.len().min(VALUE_SIZE);
    let clen = 3 + klen + vlen;
    let mut fe = page_free_end(data);
    if fe < min_free_start + clen {
        page_compact_skip(data, skip_idx);
        fe = page_free_end(data);
    }
    debug_assert!(fe >= min_free_start + clen, "Slotted page overflow");
    let new_fe = fe - clen;
    data[new_fe] = klen as u8;
    data[new_fe + 1..new_fe + 3].copy_from_slice(&(vlen as u16).to_le_bytes());
    data[new_fe + 3..new_fe + 3 + klen].copy_from_slice(key_slice);
    data[new_fe + 3 + klen..new_fe + clen].copy_from_slice(&value[..vlen]);
    page_set_free_end(data, new_fe);
    new_fe
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TailPreserveMode {
    None,
    Fixed32,
    CompactBlock32,
}

/// Overwrites an existing slot `pos` with `(key_slice, value)`, optionally preserving the last 32 bytes
/// of the previous value according to `preserve_mode`.
fn page_overwrite_slot(
    data: &mut [u8; BLOCK_SIZE],
    is_leaf: bool,
    pos: usize,
    key_slice: &[u8],
    value: &[u8],
    preserve_mode: TailPreserveMode,
) {
    let has_soa = page_ensure_soa(data);
    let old_off = page_get_slot(data, is_leaf, pos);
    let old_klen = data[old_off] as usize;
    let old_vlen = u16::from_le_bytes([data[old_off + 1], data[old_off + 2]]) as usize;
    let old_clen = 3 + old_klen + old_vlen;
    let old_val_off = old_off + 3 + old_klen;

    let old_is_compact = old_vlen >= 1 && (data[old_val_off] & 0x80) != 0;
    let new_is_compact =
        preserve_mode == TailPreserveMode::CompactBlock32 && !value.is_empty() && (value[0] & 0x80) != 0;

    let mut saved_tail = [0u8; 32];
    let has_saved_tail = match preserve_mode {
        TailPreserveMode::None => false,
        TailPreserveMode::Fixed32 => {
            if old_vlen >= 32 {
                let tail_off = old_val_off + old_vlen - 32;
                saved_tail.copy_from_slice(&data[tail_off..tail_off + 32]);
                true
            } else {
                false
            }
        }
        TailPreserveMode::CompactBlock32 => {
            if old_is_compact {
                if (data[old_val_off] & 0x40) != 0 && old_vlen > 32 {
                    let tail_off = old_val_off + old_vlen - 32;
                    saved_tail.copy_from_slice(&data[tail_off..tail_off + 32]);
                    saved_tail != [0u8; 32]
                } else {
                    false
                }
            } else if old_vlen >= 32 {
                let tail_off = old_val_off + old_vlen - 32;
                saved_tail.copy_from_slice(&data[tail_off..tail_off + 32]);
                saved_tail != [0u8; 32]
            } else {
                false
            }
        }
    };

    // If the new value is a compact block without a successor (`(value[0] & 0xC0) == 0x80`)
    // and the old cell had a non-zero 32-byte successor that must be preserved, append `saved_tail`.
    let mut expanded_buf = [0u8; VALUE_SIZE];
    let eff_val: &[u8] = if new_is_compact && has_saved_tail && (value[0] & 0x40) == 0 {
        let base_len = value.len().min(VALUE_SIZE - 32);
        expanded_buf[..base_len].copy_from_slice(&value[..base_len]);
        expanded_buf[0] |= 0x40;
        expanded_buf[base_len..base_len + 32].copy_from_slice(&saved_tail);
        &expanded_buf[..base_len + 32]
    } else {
        value
    };

    let klen = key_slice.len();
    let vlen = eff_val.len().min(VALUE_SIZE);
    let new_clen = 3 + klen + vlen;

    let cell_off = if new_clen <= old_clen {
        data[old_off] = klen as u8;
        data[old_off + 1..old_off + 3].copy_from_slice(&(vlen as u16).to_le_bytes());
        data[old_off + 3..old_off + 3 + klen].copy_from_slice(key_slice);
        data[old_off + 3 + klen..old_off + new_clen].copy_from_slice(&eff_val[..vlen]);
        if old_clen > new_clen {
            let dead = page_dead_bytes(data) + (old_clen - new_clen);
            page_set_dead_bytes(data, dead);
        }
        old_off
    } else {
        let nk = page_num_keys(data);
        let fs = page_free_start(is_leaf, nk);
        let dead = page_dead_bytes(data) + old_clen;
        page_set_dead_bytes(data, dead);
        let new_off = page_alloc_cell(data, fs, key_slice, eff_val, Some(pos));
        page_set_slot(data, is_leaf, pos, new_off);
        new_off
    };

    if has_soa {
        let hb = page_heads_base(is_leaf, page_num_keys(data));
        let h32 = key_head32(key_slice);
        let o = hb + pos * 4;
        data[o..o + 4].copy_from_slice(&h32.to_le_bytes());
    }

    if has_saved_tail && (!new_is_compact || (value[0] & 0x40) != 0) && vlen >= 32 {
        let tail_off = cell_off + 3 + klen + vlen - 32;
        data[tail_off..tail_off + 32].copy_from_slice(&saved_tail);
    }
}

/// Fast-path overwrite when `pos` already holds the identical key (`equal == true`).
/// When the stored key and value lengths are unchanged, overwrites only the value bytes in place.
#[inline]
fn page_overwrite_same_key(
    data: &mut [u8; BLOCK_SIZE],
    is_leaf: bool,
    pos: usize,
    key_slice: &[u8],
    value: &[u8],
    preserve_mode: TailPreserveMode,
) {
    let old_off = page_get_slot(data, is_leaf, pos);
    let old_klen = data[old_off] as usize;
    let old_vlen = u16::from_le_bytes([data[old_off + 1], data[old_off + 2]]) as usize;
    let vlen = value.len().min(VALUE_SIZE);
    if preserve_mode != TailPreserveMode::CompactBlock32
        && key_slice.len() == old_klen
        && vlen == old_vlen
    {
        let val_off = old_off + 3 + old_klen;
        let copy_len = if preserve_mode == TailPreserveMode::Fixed32 && vlen >= 32 {
            vlen - 32
        } else {
            vlen
        };
        data[val_off..val_off + copy_len].copy_from_slice(&value[..copy_len]);
    } else {
        page_overwrite_slot(data, is_leaf, pos, key_slice, value, preserve_mode);
    }
}

/// Inserts a new entry `(key_slice, value)` at index `pos` (`0 <= pos <= num_keys`).
/// For internal nodes, `right_child` is stored at `child[pos + 1]`.
fn page_insert_at(
    data: &mut [u8; BLOCK_SIZE],
    pos: usize,
    key_slice: &[u8],
    value: &[u8],
    right_child: u32,
) {
    let has_soa = page_ensure_soa(data);
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let new_fs = page_free_start(is_leaf, nk + 1);
    let cell_off = page_alloc_cell(data, new_fs, key_slice, value, None);

    if has_soa {
        let old_hb = page_heads_base(is_leaf, nk);
        let new_hb = page_heads_base(is_leaf, nk + 1);
        if pos < nk {
            data.copy_within(old_hb + pos * 4..old_hb + nk * 4, new_hb + (pos + 1) * 4);
        }
        if new_hb != old_hb && pos > 0 {
            data.copy_within(old_hb..old_hb + pos * 4, new_hb);
        }
        let h32 = key_head32(key_slice);
        let o = new_hb + pos * 4;
        data[o..o + 4].copy_from_slice(&h32.to_le_bytes());
    }

    if is_leaf {
        if pos < nk {
            data.copy_within(8 + pos * 2..8 + nk * 2, 8 + (pos + 1) * 2);
        }
        page_set_slot(data, true, pos, cell_off);
    } else {
        if pos < nk {
            data.copy_within(12 + pos * 6..12 + nk * 6, 12 + (pos + 1) * 6);
        }
        page_set_slot(data, false, pos, cell_off);
        page_set_child(data, pos + 1, right_child);
    }
    page_set_num_keys(data, nk + 1);
}

/// Removes the entry at index `pos` (`0 <= pos < num_keys`).
/// For internal nodes, removes `slot[pos]` and `child[pos + 1]`.
fn page_remove_at(data: &mut [u8; BLOCK_SIZE], pos: usize) {
    let has_soa = page_ensure_soa(data);
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    let old_off = page_get_slot(data, is_leaf, pos);
    let clen = page_cell_len(data, old_off);

    if nk <= 1 {
        if is_leaf {
            page_init(data, true);
        } else {
            let c0 = page_get_child(data, 0);
            page_init(data, false);
            page_set_child(data, 0, c0);
        }
        return;
    }

    let old_hb = page_heads_base(is_leaf, nk);
    let new_hb = page_heads_base(is_leaf, nk - 1);

    if is_leaf {
        if pos + 1 < nk {
            data.copy_within(8 + (pos + 1) * 2..8 + nk * 2, 8 + pos * 2);
        }
        data[8 + (nk - 1) * 2..8 + nk * 2].fill(0);
    } else {
        if pos + 1 < nk {
            data.copy_within(12 + (pos + 1) * 6..12 + nk * 6, 12 + pos * 6);
        }
        data[12 + (nk - 1) * 6..12 + nk * 6].fill(0);
    }

    if has_soa {
        if new_hb != old_hb && pos > 0 {
            data.copy_within(old_hb..old_hb + pos * 4, new_hb);
        }
        if pos + 1 < nk {
            data.copy_within(old_hb + (pos + 1) * 4..old_hb + nk * 4, new_hb + pos * 4);
        }
    }

    page_set_num_keys(data, nk - 1);
    let dead = page_dead_bytes(data) + clen;
    page_set_dead_bytes(data, dead);
}

/// Chooses the byte-balanced split index `t` (`1 <= t < nk - 1`) for a full node.
fn page_choose_split_index(data: &[u8; BLOCK_SIZE]) -> usize {
    let is_leaf = page_is_leaf(data);
    let nk = page_num_keys(data);
    if nk <= 3 {
        return nk / 2;
    }
    let dir_entry_size = if is_leaf { 6usize } else { 10usize };
    let mut total_bytes = 0usize;
    for i in 0..nk {
        let off = page_get_slot(data, is_leaf, i);
        total_bytes += dir_entry_size + page_cell_len(data, off);
    }
    let half = total_bytes / 2;
    let mut acc = 0usize;
    for i in 0..nk {
        let off = page_get_slot(data, is_leaf, i);
        acc += dir_entry_size + page_cell_len(data, off);
        if acc >= half {
            return i.clamp(1, nk - 2);
        }
    }
    nk / 2
}

/// Splits `child_data` at its byte-balanced median key `t`, populating `new_child_data` with `t + 1..nk`,
/// truncating/compacting `child_data` to `0..t`, and inserting the median key + `new_child_id` into `parent_data` at `parent_idx`.
/// Returns the promoted median key expanded to `[u8; KEY_SIZE]`.
fn split_pages(
    parent_data: &mut [u8; BLOCK_SIZE],
    parent_idx: usize,
    child_data: &mut [u8; BLOCK_SIZE],
    new_child_id: u32,
    new_child_data: &mut [u8; BLOCK_SIZE],
) -> [u8; KEY_SIZE] {
    let child_has_soa = page_ensure_soa(child_data);
    let is_leaf = page_is_leaf(child_data);
    let nk = page_num_keys(child_data);
    let t = page_choose_split_index(child_data);

    page_init(new_child_data, is_leaf);
    if !is_leaf {
        page_set_child(new_child_data, 0, page_get_child(child_data, t + 1));
    }
    for j in (t + 1)..nk {
        let off = page_get_slot(child_data, is_leaf, j);
        let (ck, cv) = page_cell(child_data, off);
        let rc = if is_leaf { 0 } else { page_get_child(child_data, j + 1) };
        page_insert_at(new_child_data, j - (t + 1), ck, cv, rc);
    }

    let mid_off = page_get_slot(child_data, is_leaf, t);
    let (mid_ck, mid_cv) = page_cell(child_data, mid_off);
    let mid_expanded = expand_cell_key(mid_ck);
    let mid_klen = mid_ck.len();
    let mid_vlen = mid_cv.len();
    let mut mid_k_buf = [0u8; KEY_SIZE];
    let mut mid_v_buf = [0u8; VALUE_SIZE];
    mid_k_buf[..mid_klen].copy_from_slice(mid_ck);
    mid_v_buf[..mid_vlen].copy_from_slice(mid_cv);

    // Truncate `child_data` to `0..t`, shifting its SoA heads to `page_heads_base(is_leaf, t)` and compacting in place.
    if child_has_soa {
        let old_hb = page_heads_base(is_leaf, nk);
        let new_hb = page_heads_base(is_leaf, t);
        if new_hb != old_hb && t > 0 {
            child_data.copy_within(old_hb..old_hb + t * 4, new_hb);
        }
    }
    page_set_num_keys(child_data, t);
    page_compact_skip(child_data, None);
    if !child_has_soa {
        let _ = page_ensure_soa(child_data);
    }

    page_insert_at(
        parent_data,
        parent_idx,
        &mid_k_buf[..mid_klen],
        &mid_v_buf[..mid_vlen],
        new_child_id,
    );

    mid_expanded
}

// ── Metadata & BTree Struct ──────────────────────────────────────────────────

/// Global metadata for the B-Tree, stored in Page 0 of each shard.
pub struct MetaData {
    /// Page ID of the root node of the B-Tree.
    pub root_page_id: u32,
    /// Counter used for sequential page allocation.
    pub next_free_page: u32,
    /// Head of the linked list of free pages that can be reused.
    pub first_free_page: u32,
}

impl MetaData {
    pub fn from_bytes(data: &[u8; BLOCK_SIZE]) -> Self {
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != BTREE_META_MAGIC {
            return Self {
                root_page_id: 0,
                next_free_page: 1,
                first_free_page: 0,
            };
        }
        let root_page_id = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let next_free_page = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let first_free_page = u32::from_le_bytes(data[12..16].try_into().unwrap());
        Self {
            root_page_id,
            next_free_page,
            first_free_page,
        }
    }

    pub fn to_bytes(&self, data: &mut [u8; BLOCK_SIZE]) {
        data[0..4].copy_from_slice(&BTREE_META_MAGIC.to_le_bytes());
        data[4..8].copy_from_slice(&self.root_page_id.to_le_bytes());
        data[8..12].copy_from_slice(&self.next_free_page.to_le_bytes());
        data[12..16].copy_from_slice(&self.first_free_page.to_le_bytes());
    }
}

pub struct BTree {
    pub bpm: BufferPool,
    meta: MetaData,
    max_pages: usize,
}

impl BTree {
    pub fn new(bpm: BufferPool) -> Self {
        let max_pages = crate::storage::SHARD_SIZE_PAGES.load(core::sync::atomic::Ordering::SeqCst);
        Self::new_with_max_pages(bpm, max_pages)
    }

    pub fn new_with_max_pages(mut bpm: BufferPool, max_pages: usize) -> Self {
        crate::watchdog::kick();
        crate::storage::STORAGE_GEN.fetch_add(1, core::sync::atomic::Ordering::Release);

        let meta = {
            let guard = bpm.fetch_page(0).unwrap();
            MetaData::from_bytes(guard.data())
        };

        let mut tree = Self {
            bpm,
            meta,
            max_pages,
        };

        tracing::debug!(
            "[btree] Init: root={} next={}",
            tree.meta.root_page_id,
            tree.meta.next_free_page
        );

        if tree.meta.root_page_id == 0 {
            if tree.meta.next_free_page > 1 {
                tracing::info!(
                    "[storage] BTree metadata re-initializing for slotted-page layout..."
                );
            }
            tree.meta.next_free_page = 1;
            tree.meta.first_free_page = 0;
            let root_id = tree.allocate_page();
            {
                let mut guard = tree.bpm.new_page(root_id as usize).unwrap();
                page_init(guard.data_mut(), true);
            }
            tree.meta.root_page_id = root_id;
            tree.save_meta();
            let _ = tree.flush();
        }
        tree
    }

    fn save_meta(&mut self) {
        let mut guard = self.bpm.fetch_page(0).unwrap();
        self.meta.to_bytes(guard.data_mut());
    }

    pub fn flush(&mut self) -> Result<(), crate::storage::StorageError> {
        self.bpm.flush_all()
    }

    pub fn wipe(&mut self) {
        crate::storage::STORAGE_GEN.fetch_add(1, core::sync::atomic::Ordering::Release);
        self.meta = MetaData {
            root_page_id: 0,
            next_free_page: 1,
            first_free_page: 0,
        };
        let root_id = self.allocate_page();
        {
            let mut guard = self.bpm.new_page(root_id as usize).unwrap();
            page_init(guard.data_mut(), true);
        }
        self.meta.root_page_id = root_id;
        self.save_meta();
        let _ = self.flush();
    }

    pub fn allocate_page(&mut self) -> u32 {
        if self.meta.first_free_page != 0 {
            let reused_id = self.meta.first_free_page;
            let next_free = {
                let guard = self.bpm.fetch_page(reused_id as usize).unwrap();
                u32::from_le_bytes(guard.data()[0..4].try_into().unwrap())
            };
            self.meta.first_free_page = next_free;
            self.save_meta();
            reused_id
        } else {
            if self.meta.next_free_page == 0 {
                self.meta.next_free_page = 1;
            }
            if self.max_pages != 0 && self.meta.next_free_page as usize >= self.max_pages {
                panic!("BTree shard capacity exhausted");
            }
            let id = self.meta.next_free_page;
            self.meta.next_free_page += 1;
            self.save_meta();
            id
        }
    }

    /// Adds a page to the free list for future reuse.
    pub fn free_page(&mut self, page_id: u32) {
        if page_id == 0 || page_id == self.meta.root_page_id {
            return;
        }
        let first_free = self.meta.first_free_page;
        {
            let mut guard = self.bpm.new_page(page_id as usize).unwrap();
            guard.data_mut()[0..4].copy_from_slice(&first_free.to_le_bytes());
        }
        self.meta.first_free_page = page_id;
        self.save_meta();
    }

    pub fn read_page(&mut self, page_id: u32) -> [u8; BLOCK_SIZE] {
        let guard = self.bpm.fetch_page(page_id as usize).unwrap();
        let _ = page_ensure_soa(guard.data_cache_mut());
        *guard.data()
    }

    pub fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) {
        let mut guard = self.bpm.fetch_page(page_id as usize).unwrap();
        guard.data_mut().copy_from_slice(data);
    }

    fn write_new_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) {
        let mut guard = self.bpm.new_page(page_id as usize).unwrap();
        guard.data_mut().copy_from_slice(data);
    }

    /// Searches for a key in the B-Tree and returns the associated value if found.
    pub fn search(&mut self, key: &[u8; KEY_SIZE]) -> Option<Vec<u8>> {
        self.get_with(key, |v| v.to_vec())
    }

    /// Zero-copy existence check.
    pub fn contains_key(&mut self, key: &[u8; KEY_SIZE]) -> bool {
        self.get_with(key, |_| ()).is_some()
    }

    /// Zero-copy lookup: walks the cached page bytes in place and passes the value slice
    /// directly from the pinned 4KB page to `f` without any heap allocation.
    pub fn get_with<R>(&mut self, key: &[u8; KEY_SIZE], f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let t0 = crate::clint::mtime();
        let r = self.get_with_impl(key, f);
        crate::storage::bpm::STORAGE_PROF[6].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        crate::storage::bpm::STORAGE_PROF[7]
            .fetch_add(crate::clint::mtime() - t0, core::sync::atomic::Ordering::Relaxed);
        r
    }

    fn get_with_impl<R>(&mut self, key: &[u8; KEY_SIZE], f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let target_head = key_head(key);
        let mut current_id = self.meta.root_page_id;
        loop {
            let guard = self
                .bpm
                .fetch_page(current_id as usize)
                .expect("Failed to fetch node");
            let _ = page_ensure_soa(guard.data_cache_mut());
            let data = guard.data();
            let is_leaf = page_is_leaf(data);
            let (lo, equal) = page_lower_bound(data, key, target_head);
            if equal {
                let cell_off = page_get_slot(data, is_leaf, lo);
                let (_, val) = page_cell(data, cell_off);
                return Some(f(val));
            }
            if is_leaf {
                return None;
            }
            current_id = page_get_child(data, lo);
            drop(guard);
            self.bpm.prefetch_page(current_id as usize);
        }
    }

    /// In-place overwrite of the last 32 bytes of `key`'s value (used for fixed-tail 32B suffixes).
    /// Returns `true` if `key` was found and updated.
    pub fn update_suffix_32(&mut self, key: &[u8; KEY_SIZE], suffix: &[u8; 32]) -> bool {
        self.update_suffix_32_mode(key, suffix, false)
    }

    /// Updates the 32-byte block successor for a compact (`0x80` / `0x40`) or legacy block cell.
    /// Returns `true` if `key` was found and updated.
    pub fn update_compact_successor_32(&mut self, key: &[u8; KEY_SIZE], suffix: &[u8; 32]) -> bool {
        self.update_suffix_32_mode(key, suffix, true)
    }

    fn update_suffix_32_mode(
        &mut self,
        key: &[u8; KEY_SIZE],
        suffix: &[u8; 32],
        compact_mode: bool,
    ) -> bool {
        let target_head = key_head(key);
        let mut current_id = self.meta.root_page_id;
        loop {
            let mut guard = match self.bpm.fetch_page(current_id as usize) {
                Ok(g) => g,
                Err(_) => return false,
            };
            let _ = page_ensure_soa(guard.data_cache_mut());
            let (is_leaf, lo, equal, next_child) = {
                let data = guard.data();
                let is_leaf = page_is_leaf(data);
                let (lo, equal) = page_lower_bound(data, key, target_head);
                let next_child = if !equal && !is_leaf {
                    page_get_child(data, lo)
                } else {
                    0
                };
                (is_leaf, lo, equal, next_child)
            };
            if equal {
                let data = guard.data_mut();
                let cell_off = page_get_slot(data, is_leaf, lo);
                let klen = data[cell_off] as usize;
                let vlen = u16::from_le_bytes([data[cell_off + 1], data[cell_off + 2]]) as usize;
                let val_off = cell_off + 3 + klen;

                if compact_mode && vlen >= 1 && (data[val_off] & 0x80) != 0 {
                    let has_succ = (data[val_off] & 0x40) != 0 && vlen > 32;
                    let suffix_nonzero = *suffix != [0u8; 32];
                    match (has_succ, suffix_nonzero) {
                        (true, true) => {
                            let tail_off = val_off + vlen - 32;
                            data[tail_off..tail_off + 32].copy_from_slice(suffix);
                            return true;
                        }
                        (false, false) => {
                            return true;
                        }
                        (true, false) => {
                            data[val_off] &= !0x40;
                            let new_vlen = vlen - 32;
                            data[cell_off + 1..cell_off + 3]
                                .copy_from_slice(&(new_vlen as u16).to_le_bytes());
                            let dead = page_dead_bytes(data) + 32;
                            page_set_dead_bytes(data, dead);
                            return true;
                        }
                        (false, true) => {
                            if vlen + 32 > VALUE_SIZE {
                                return false;
                            }
                            let mut new_val = [0u8; VALUE_SIZE];
                            new_val[..vlen].copy_from_slice(&data[val_off..val_off + vlen]);
                            new_val[0] |= 0x40;
                            new_val[vlen..vlen + 32].copy_from_slice(suffix);
                            if page_usable_free(data) >= 32 {
                                let mut k_buf = [0u8; KEY_SIZE];
                                k_buf[..klen]
                                    .copy_from_slice(&data[cell_off + 3..cell_off + 3 + klen]);
                                page_overwrite_slot(
                                    data,
                                    is_leaf,
                                    lo,
                                    &k_buf[..klen],
                                    &new_val[..vlen + 32],
                                    TailPreserveMode::None,
                                );
                                return true;
                            } else {
                                drop(guard);
                                self.insert(*key, &new_val[..vlen + 32]);
                                return true;
                            }
                        }
                    }
                }

                if vlen >= 32 {
                    let tail_off = val_off + vlen - 32;
                    data[tail_off..tail_off + 32].copy_from_slice(suffix);
                    return true;
                }
                return false;
            }
            if is_leaf {
                return false;
            }
            drop(guard);
            self.bpm.prefetch_page_write(next_child as usize);
            current_id = next_child;
        }
    }

    /// Walks resident internal nodes from the root without pinning or I/O and issues a
    /// `Zicbop` write-intent prefetch (`prefetch.w`) on the deepest resident child/leaf page.
    #[inline]
    pub fn prefetch_key_write(&self, key: &[u8; KEY_SIZE]) {
        let target_head = key_head(key);
        let mut cur = self.meta.root_page_id;
        for _ in 0..4 {
            let Some(data) = self.bpm.peek_resident_page(cur as usize) else {
                break;
            };
            if page_is_leaf(data) {
                self.bpm.prefetch_page_write(cur as usize);
                break;
            }
            let (lb, equal) = page_lower_bound(data, key, target_head);
            if equal {
                self.bpm.prefetch_page_write(cur as usize);
                break;
            }
            let child = page_get_child(data, lb);
            self.bpm.prefetch_page_write(child as usize);
            cur = child;
        }
    }

    /// Zero-copy lower-bound key lookup: returns the smallest key `>= start_key` in the B-Tree.
    pub fn first_key_from(&mut self, start_key: &[u8; KEY_SIZE]) -> Option<[u8; KEY_SIZE]> {
        let start_head = key_head(start_key);
        let mut current_id = self.meta.root_page_id;
        let mut candidate: Option<[u8; KEY_SIZE]> = None;

        loop {
            let guard = self
                .bpm
                .fetch_page(current_id as usize)
                .expect("Failed to fetch node");
            let _ = page_ensure_soa(guard.data_cache_mut());
            let data = guard.data();
            let is_leaf = page_is_leaf(data);
            let nk = page_num_keys(data);
            let (lo, equal) = page_lower_bound(data, start_key, start_head);
            if equal {
                return Some(*start_key);
            }
            if lo < nk {
                let cell_off = page_get_slot(data, is_leaf, lo);
                let (ck, _) = page_cell(data, cell_off);
                candidate = Some(expand_cell_key(ck));
            }
            if is_leaf {
                return candidate;
            }
            current_id = page_get_child(data, lo);
        }
    }

    pub fn insert(&mut self, key: [u8; KEY_SIZE], value: &[u8]) -> bool {
        self.insert_with_mode(key, value, TailPreserveMode::None)
    }

    pub fn insert_preserving_suffix_32(
        &mut self,
        key: [u8; KEY_SIZE],
        value: &[u8],
        preserve_tail_32: bool,
    ) -> bool {
        self.insert_with_mode(
            key,
            value,
            if preserve_tail_32 {
                TailPreserveMode::Fixed32
            } else {
                TailPreserveMode::None
            },
        )
    }

    pub fn insert_compact_block(
        &mut self,
        key: [u8; KEY_SIZE],
        value: &[u8],
        preserve_tail_32: bool,
    ) -> bool {
        self.insert_with_mode(
            key,
            value,
            if preserve_tail_32 {
                TailPreserveMode::CompactBlock32
            } else {
                TailPreserveMode::None
            },
        )
    }

    fn insert_with_mode(
        &mut self,
        key: [u8; KEY_SIZE],
        value: &[u8],
        preserve_mode: TailPreserveMode,
    ) -> bool {
        let t0 = crate::clint::mtime();
        let is_new = self.insert_impl(key, value, preserve_mode);
        crate::storage::bpm::STORAGE_PROF[8].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        crate::storage::bpm::STORAGE_PROF[9]
            .fetch_add(crate::clint::mtime() - t0, core::sync::atomic::Ordering::Relaxed);
        is_new
    }

    /// Fast in-place insert/update when the target node has sufficient room without splitting.
    fn insert_fast(
        &mut self,
        key: &[u8; KEY_SIZE],
        value: &[u8],
        preserve_mode: TailPreserveMode,
    ) -> Option<bool> {
        let target_head = key_head(key);
        let k_slice = key_compact_slice(key);
        let klen = k_slice.len();
        let vlen = value.len().min(VALUE_SIZE);
        let new_clen = 3 + klen + vlen;
        let mut cur = self.meta.root_page_id;
        loop {
            let mut guard = match self.bpm.fetch_page(cur as usize) {
                Ok(g) => g,
                Err(_) => return None,
            };
            let _ = page_ensure_soa(guard.data_cache_mut());
            let (is_leaf, lb, equal, child) = {
                let d = guard.data();
                let is_leaf = page_is_leaf(d);
                let (lb, equal) = page_lower_bound(d, key, target_head);
                if equal {
                    let old_off = page_get_slot(d, is_leaf, lb);
                    let old_clen = page_cell_len(d, old_off);
                    let max_new_clen = new_clen
                        + if preserve_mode == TailPreserveMode::CompactBlock32 {
                            32
                        } else {
                            0
                        };
                    if max_new_clen > old_clen && page_usable_free(d) + old_clen < max_new_clen {
                        return None;
                    }
                    (is_leaf, lb, true, 0)
                } else if is_leaf {
                    let nk = page_num_keys(d);
                    let dir_grow = page_free_start(true, nk + 1) - page_free_start(true, nk);
                    if nk >= MAX_KEYS || page_usable_free(d) < dir_grow + new_clen {
                        return None;
                    }
                    (true, lb, false, 0)
                } else {
                    (false, lb, false, page_get_child(d, lb))
                }
            };

            if equal {
                page_overwrite_same_key(
                    guard.data_mut(),
                    is_leaf,
                    lb,
                    k_slice,
                    value,
                    preserve_mode,
                );
                return Some(false);
            }
            if is_leaf {
                page_insert_at(guard.data_mut(), lb, k_slice, value, 0);
                return Some(true);
            }
            drop(guard);
            self.bpm.prefetch_page_write(child as usize);
            cur = child;
        }
    }

    fn split_child_in_place(
        &mut self,
        parent_id: u32,
        idx: usize,
        child_id: u32,
    ) -> ([u8; KEY_SIZE], u32) {
        let new_child_id = self.allocate_page();
        debug_assert!(
            parent_id != child_id && parent_id != new_child_id && child_id != new_child_id
        );
        let mut parent_guard = self.bpm.fetch_page(parent_id as usize).unwrap();
        let mut child_guard = self.bpm.fetch_page(child_id as usize).unwrap();
        let mut new_child_guard = self.bpm.new_page(new_child_id as usize).unwrap();
        let mid_key = split_pages(
            parent_guard.data_mut(),
            idx,
            child_guard.data_mut(),
            new_child_id,
            new_child_guard.data_mut(),
        );
        (mid_key, new_child_id)
    }

    fn split_child_by_id(
        &mut self,
        parent_id: u32,
        parent_data: &mut [u8; BLOCK_SIZE],
        idx: usize,
        child_id: u32,
        child_data: &mut [u8; BLOCK_SIZE],
    ) -> ([u8; KEY_SIZE], u32, [u8; BLOCK_SIZE]) {
        let new_child_id = self.allocate_page();
        let mut new_child_data = [0u8; BLOCK_SIZE];
        let mid_key = split_pages(parent_data, idx, child_data, new_child_id, &mut new_child_data);
        self.write_page(child_id, child_data);
        self.write_new_page(new_child_id, &new_child_data);
        self.write_page(parent_id, parent_data);
        (mid_key, new_child_id, new_child_data)
    }

    fn insert_impl(
        &mut self,
        key: [u8; KEY_SIZE],
        value: &[u8],
        preserve_mode: TailPreserveMode,
    ) -> bool {
        if let Some(is_new) = self.insert_fast(&key, value, preserve_mode) {
            return is_new;
        }

        let root_id = self.meta.root_page_id;
        let mut cur_id = root_id;
        let root_full = {
            let guard = self.bpm.fetch_page(root_id as usize).unwrap();
            let _ = page_ensure_soa(guard.data_cache_mut());
            page_is_full(guard.data())
        };

        if root_full {
            let new_root_id = self.allocate_page();
            let new_child_id = self.allocate_page();
            {
                let mut root_guard = self.bpm.fetch_page(root_id as usize).unwrap();
                let mut new_root_guard = self.bpm.new_page(new_root_id as usize).unwrap();
                let mut new_child_guard = self.bpm.new_page(new_child_id as usize).unwrap();
                let new_root_data = new_root_guard.data_mut();
                page_init(new_root_data, false);
                page_set_child(new_root_data, 0, root_id);
                let _ = split_pages(
                    new_root_data,
                    0,
                    root_guard.data_mut(),
                    new_child_id,
                    new_child_guard.data_mut(),
                );
            }
            self.meta.root_page_id = new_root_id;
            self.save_meta();
            cur_id = new_root_id;
        }

        let target_head = key_head(&key);
        let k_slice = key_compact_slice(&key);

        loop {
            let mut cur_guard = self.bpm.fetch_page(cur_id as usize).unwrap();
            let _ = page_ensure_soa(cur_guard.data_cache_mut());
            let (is_leaf, lb, equal, child_id) = {
                let d = cur_guard.data();
                let is_leaf = page_is_leaf(d);
                let (lb, equal) = page_lower_bound(d, &key, target_head);
                let child_id = if !equal && !is_leaf {
                    page_get_child(d, lb)
                } else {
                    0
                };
                (is_leaf, lb, equal, child_id)
            };
            if equal {
                page_overwrite_same_key(
                    cur_guard.data_mut(),
                    is_leaf,
                    lb,
                    k_slice,
                    value,
                    preserve_mode,
                );
                return false;
            }
            if is_leaf {
                page_insert_at(cur_guard.data_mut(), lb, k_slice, value, 0);
                return true;
            }
            drop(cur_guard);

            let child_full = {
                let child_guard = self.bpm.fetch_page(child_id as usize).unwrap();
                let _ = page_ensure_soa(child_guard.data_cache_mut());
                page_is_full(child_guard.data())
            };
            if child_full {
                let (mid_key, new_child_id) = self.split_child_in_place(cur_id, lb, child_id);
                match key.cmp(&mid_key) {
                    Ordering::Equal => {
                        let mut cur_guard = self.bpm.fetch_page(cur_id as usize).unwrap();
                        page_overwrite_same_key(
                            cur_guard.data_mut(),
                            false,
                            lb,
                            k_slice,
                            value,
                            preserve_mode,
                        );
                        return false;
                    }
                    Ordering::Greater => {
                        cur_id = new_child_id;
                        continue;
                    }
                    Ordering::Less => {}
                }
            }
            cur_id = child_id;
        }
    }

    /// In-place delete for the common case where `key` is in a leaf node with `> 1` keys (or root).
    fn delete_fast(&mut self, key: &[u8; KEY_SIZE]) -> bool {
        let target_head = key_head(key);
        let root_id = self.meta.root_page_id;
        let mut cur = root_id;
        loop {
            let mut guard = match self.bpm.fetch_page(cur as usize) {
                Ok(g) => g,
                Err(_) => return false,
            };
            let _ = page_ensure_soa(guard.data_cache_mut());
            let (is_leaf, nk, lb, equal, child) = {
                let d = guard.data();
                let is_leaf = page_is_leaf(d);
                let nk = page_num_keys(d);
                let (lb, equal) = page_lower_bound(d, key, target_head);
                if equal && !is_leaf {
                    return false;
                }
                let child = if !is_leaf { page_get_child(d, lb) } else { 0 };
                (is_leaf, nk, lb, equal, child)
            };

            if is_leaf {
                if !equal {
                    return true;
                }
                if cur != root_id && nk <= 1 {
                    return false;
                }
                page_remove_at(guard.data_mut(), lb);
                return true;
            }
            cur = child;
        }
    }

    pub fn delete(&mut self, key: [u8; KEY_SIZE]) {
        if self.delete_fast(&key) {
            return;
        }
        self.delete_slow(&key);
    }

    fn delete_slow(&mut self, key: &[u8; KEY_SIZE]) {
        let target_head = key_head(key);
        let mut stack = [(0u32, 0usize); 24];
        let mut stack_len = 0usize;

        let mut cur_id = self.meta.root_page_id;
        let mut cur_data = self.read_page(cur_id);

        if page_is_full(&cur_data) {
            let new_root_id = self.allocate_page();
            let mut new_root_data = [0u8; BLOCK_SIZE];
            page_init(&mut new_root_data, false);
            page_set_child(&mut new_root_data, 0, cur_id);

            let new_child_id = self.allocate_page();
            let mut new_child_data = [0u8; BLOCK_SIZE];
            let _ = split_pages(
                &mut new_root_data,
                0,
                &mut cur_data,
                new_child_id,
                &mut new_child_data,
            );
            self.write_page(cur_id, &cur_data);
            self.write_new_page(new_child_id, &new_child_data);
            self.write_new_page(new_root_id, &new_root_data);

            self.meta.root_page_id = new_root_id;
            self.save_meta();

            cur_id = new_root_id;
            cur_data = new_root_data;
        }

        // 1. Descend until we find `key` (proactively splitting full nodes so every ancestor has >= 330B free).
        let (found_node_id, mut found_data, found_idx, found_is_leaf) = loop {
            let is_leaf = page_is_leaf(&cur_data);
            let (mut lb, equal) = page_lower_bound(&cur_data, key, target_head);
            if equal {
                break (cur_id, cur_data, lb, is_leaf);
            }
            if is_leaf {
                return;
            }
            let mut child_id = page_get_child(&cur_data, lb);
            let mut child_data = self.read_page(child_id);
            if page_is_full(&child_data) {
                let (mid_key, new_child_id, new_child_data) =
                    self.split_child_by_id(cur_id, &mut cur_data, lb, child_id, &mut child_data);
                match key.cmp(&mid_key) {
                    Ordering::Equal => {
                        break (cur_id, cur_data, lb, false);
                    }
                    Ordering::Greater => {
                        lb += 1;
                        child_id = new_child_id;
                        child_data = new_child_data;
                    }
                    Ordering::Less => {}
                }
            }
            if stack_len < stack.len() {
                stack[stack_len] = (cur_id, lb);
                stack_len += 1;
            }
            cur_id = child_id;
            cur_data = child_data;
        };

        // 2. Remove from leaf directly, or replace internal key with its predecessor leaf key.
        let (mut leaf_id, mut leaf_data) = if found_is_leaf {
            page_remove_at(&mut found_data, found_idx);
            self.write_page(found_node_id, &found_data);
            (found_node_id, found_data)
        } else {
            let mut internal_id = found_node_id;
            let mut internal_data = found_data;
            let mut internal_idx = found_idx;

            // Ensure left child of `internal_idx` is non-full before descending to predecessor.
            let mut pred_id = page_get_child(&internal_data, internal_idx);
            let mut pred_data = self.read_page(pred_id);
            if page_is_full(&pred_data) {
                let (_, new_child_id, new_child_data) = self.split_child_by_id(
                    internal_id,
                    &mut internal_data,
                    internal_idx,
                    pred_id,
                    &mut pred_data,
                );
                internal_idx += 1;
                pred_id = new_child_id;
                pred_data = new_child_data;
            }
            if stack_len < stack.len() {
                stack[stack_len] = (internal_id, internal_idx);
                stack_len += 1;
            }

            while !page_is_leaf(&pred_data) {
                let nk = page_num_keys(&pred_data);
                let mut rc_id = page_get_child(&pred_data, nk);
                let mut rc_data = self.read_page(rc_id);
                let mut child_pos = nk;
                if page_is_full(&rc_data) {
                    let (_, new_rc_id, new_rc_data) = self.split_child_by_id(
                        pred_id,
                        &mut pred_data,
                        nk,
                        rc_id,
                        &mut rc_data,
                    );
                    if pred_id == internal_id && nk <= internal_idx {
                        internal_idx += 1;
                    }
                    child_pos = nk + 1;
                    rc_id = new_rc_id;
                    rc_data = new_rc_data;
                }
                if stack_len < stack.len() {
                    stack[stack_len] = (pred_id, child_pos);
                    stack_len += 1;
                }
                pred_id = rc_id;
                pred_data = rc_data;
            }

            // Copy predecessor `(pred_k, pred_v)` from the last slot of `pred_data`.
            let pred_nk = page_num_keys(&pred_data);
            let pred_off = page_get_slot(&pred_data, true, pred_nk - 1);
            let (pck, pcv) = page_cell(&pred_data, pred_off);
            let pk_len = pck.len();
            let pv_len = pcv.len();
            let mut pk_buf = [0u8; KEY_SIZE];
            let mut pv_buf = [0u8; VALUE_SIZE];
            pk_buf[..pk_len].copy_from_slice(pck);
            pv_buf[..pv_len].copy_from_slice(pcv);

            // Re-read `internal_id` in case it was updated on disk, overwrite `internal_idx`, and persist.
            let mut latest_internal = self.read_page(internal_id);
            page_overwrite_slot(
                &mut latest_internal,
                false,
                internal_idx,
                &pk_buf[..pk_len],
                &pv_buf[..pv_len],
                TailPreserveMode::None,
            );
            self.write_page(internal_id, &latest_internal);
            let _ = &mut internal_id;

            // Remove the predecessor from `pred_data`.
            page_remove_at(&mut pred_data, pred_nk - 1);
            self.write_page(pred_id, &pred_data);
            (pred_id, pred_data)
        };

        // 3. Rebalance bottom-up if removing from the leaf left `num_keys == 0`.
        while page_num_keys(&leaf_data) == 0 && stack_len > 0 {
            stack_len -= 1;
            let (parent_id, child_idx) = stack[stack_len];
            let mut parent_data = self.read_page(parent_id);
            let child_is_leaf = page_is_leaf(&leaf_data);
            let c0 = if child_is_leaf {
                0
            } else {
                page_get_child(&leaf_data, 0)
            };

            if child_idx > 0 {
                let sep = child_idx - 1;
                let left_id = page_get_child(&parent_data, sep);
                let mut left_data = self.read_page(left_id);

                let sep_off = page_get_slot(&parent_data, false, sep);
                let (sck, scv) = page_cell(&parent_data, sep_off);
                let sk_len = sck.len();
                let sv_len = scv.len();
                let mut sk_buf = [0u8; KEY_SIZE];
                let mut sv_buf = [0u8; VALUE_SIZE];
                sk_buf[..sk_len].copy_from_slice(sck);
                sv_buf[..sv_len].copy_from_slice(scv);

                if !page_is_full(&left_data) {
                    let left_nk = page_num_keys(&left_data);
                    page_insert_at(
                        &mut left_data,
                        left_nk,
                        &sk_buf[..sk_len],
                        &sv_buf[..sv_len],
                        c0,
                    );
                    page_remove_at(&mut parent_data, sep);
                    self.write_page(left_id, &left_data);
                    self.write_page(parent_id, &parent_data);
                    self.free_page(leaf_id);
                } else {
                    let left_nk = page_num_keys(&left_data);
                    let borrow_idx = left_nk - 1;
                    let b_off = page_get_slot(&left_data, child_is_leaf, borrow_idx);
                    let (bck, bcv) = page_cell(&left_data, b_off);
                    let bk_len = bck.len();
                    let bv_len = bcv.len();
                    let mut bk_buf = [0u8; KEY_SIZE];
                    let mut bv_buf = [0u8; VALUE_SIZE];
                    bk_buf[..bk_len].copy_from_slice(bck);
                    bv_buf[..bv_len].copy_from_slice(bcv);
                    let b_child = if child_is_leaf {
                        0
                    } else {
                        page_get_child(&left_data, borrow_idx + 1)
                    };

                    page_init(&mut leaf_data, child_is_leaf);
                    if !child_is_leaf {
                        page_set_child(&mut leaf_data, 0, b_child);
                    }
                    page_insert_at(&mut leaf_data, 0, &sk_buf[..sk_len], &sv_buf[..sv_len], c0);
                    page_overwrite_slot(
                        &mut parent_data,
                        false,
                        sep,
                        &bk_buf[..bk_len],
                        &bv_buf[..bv_len],
                        TailPreserveMode::None,
                    );
                    page_remove_at(&mut left_data, borrow_idx);

                    self.write_page(left_id, &left_data);
                    self.write_page(leaf_id, &leaf_data);
                    self.write_page(parent_id, &parent_data);
                    break;
                }
            } else {
                let sep = 0usize;
                let right_id = page_get_child(&parent_data, 1);
                let mut right_data = self.read_page(right_id);

                let sep_off = page_get_slot(&parent_data, false, sep);
                let (sck, scv) = page_cell(&parent_data, sep_off);
                let sk_len = sck.len();
                let sv_len = scv.len();
                let mut sk_buf = [0u8; KEY_SIZE];
                let mut sv_buf = [0u8; VALUE_SIZE];
                sk_buf[..sk_len].copy_from_slice(sck);
                sv_buf[..sv_len].copy_from_slice(scv);

                if !page_is_full(&right_data) {
                    let r_c0 = if child_is_leaf {
                        0
                    } else {
                        page_get_child(&right_data, 0)
                    };
                    if !child_is_leaf {
                        page_set_child(&mut right_data, 0, c0);
                    }
                    page_insert_at(
                        &mut right_data,
                        0,
                        &sk_buf[..sk_len],
                        &sv_buf[..sv_len],
                        r_c0,
                    );
                    page_set_child(&mut parent_data, 0, right_id);
                    page_remove_at(&mut parent_data, 0);
                    self.write_page(right_id, &right_data);
                    self.write_page(parent_id, &parent_data);
                    self.free_page(leaf_id);
                } else {
                    let b_off = page_get_slot(&right_data, child_is_leaf, 0);
                    let (bck, bcv) = page_cell(&right_data, b_off);
                    let bk_len = bck.len();
                    let bv_len = bcv.len();
                    let mut bk_buf = [0u8; KEY_SIZE];
                    let mut bv_buf = [0u8; VALUE_SIZE];
                    bk_buf[..bk_len].copy_from_slice(bck);
                    bv_buf[..bv_len].copy_from_slice(bcv);
                    let r_c0 = if child_is_leaf {
                        0
                    } else {
                        page_get_child(&right_data, 0)
                    };
                    let r_c1 = if child_is_leaf {
                        0
                    } else {
                        page_get_child(&right_data, 1)
                    };

                    page_init(&mut leaf_data, child_is_leaf);
                    if !child_is_leaf {
                        page_set_child(&mut leaf_data, 0, c0);
                    }
                    page_insert_at(
                        &mut leaf_data,
                        0,
                        &sk_buf[..sk_len],
                        &sv_buf[..sv_len],
                        r_c0,
                    );
                    page_overwrite_slot(
                        &mut parent_data,
                        false,
                        0,
                        &bk_buf[..bk_len],
                        &bv_buf[..bv_len],
                        TailPreserveMode::None,
                    );
                    if !child_is_leaf {
                        page_set_child(&mut right_data, 0, r_c1);
                    }
                    page_remove_at(&mut right_data, 0);

                    self.write_page(right_id, &right_data);
                    self.write_page(leaf_id, &leaf_data);
                    self.write_page(parent_id, &parent_data);
                    break;
                }
            }

            leaf_id = parent_id;
            leaf_data = parent_data;
        }

        // Collapse root if it is an internal node with 0 keys.
        let root_id = self.meta.root_page_id;
        let root_data = self.read_page(root_id);
        if !page_is_leaf(&root_data) && page_num_keys(&root_data) == 0 {
            let new_root_id = page_get_child(&root_data, 0);
            if new_root_id != 0 {
                self.meta.root_page_id = new_root_id;
                self.save_meta();
                self.free_page(root_id);
            }
        }
    }

    pub fn iter(&mut self, start_key: [u8; KEY_SIZE]) -> RangeIterator<'_> {
        let start_head = key_head(&start_key);
        let mut stack = [(0u32, 0u16); 24];
        let mut stack_len = 0usize;
        let mut current_id = self.meta.root_page_id;

        loop {
            let (is_leaf, lo, equal, child_id) = {
                let guard = self
                    .bpm
                    .fetch_page(current_id as usize)
                    .expect("Failed to fetch node");
                let _ = page_ensure_soa(guard.data_cache_mut());
                let data = guard.data();
                let is_leaf = page_is_leaf(data);
                let (lo, equal) = page_lower_bound(data, &start_key, start_head);
                let child_id = if !is_leaf { page_get_child(data, lo) } else { 0 };
                (is_leaf, lo, equal, child_id)
            };

            if stack_len < stack.len() {
                stack[stack_len] = (current_id, lo as u16);
                stack_len += 1;
            }

            if is_leaf || equal {
                break;
            } else {
                current_id = child_id;
            }
        }

        RangeIterator {
            tree: self,
            stack,
            stack_len,
        }
    }

    pub fn page_count(&self) -> usize {
        self.meta.next_free_page as usize
    }

    pub fn free_page_count(&mut self) -> usize {
        let mut count = 0;
        let mut curr = self.meta.first_free_page;
        while curr != 0 {
            count += 1;
            if let Ok(guard) = self.bpm.fetch_page_read_only(curr as usize) {
                curr = u32::from_le_bytes(guard.data()[0..4].try_into().unwrap());
            } else {
                break;
            }
        }
        count
    }

    pub fn capacity_remaining(&self, shard_max_pages: usize) -> usize {
        shard_max_pages.saturating_sub(self.meta.next_free_page as usize)
    }

    pub fn count_prefixed(&mut self, prefix: u8) -> usize {
        let mut count = 0;
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = prefix;
        let mut it = self.iter(start_key);
        while let Some(k) = it.next_key() {
            if k[0] != prefix {
                break;
            }
            count += 1;
            if count % 1000 == 0 {
                crate::watchdog::kick();
            }
        }
        count
    }
}

pub struct RangeIterator<'tree> {
    tree: &'tree mut BTree,
    stack: [(u32, u16); 24],
    stack_len: usize,
}

impl<'tree> RangeIterator<'tree> {
    #[inline]
    fn pop_stack(&mut self) -> Option<(u32, usize)> {
        if self.stack_len == 0 {
            None
        } else {
            self.stack_len -= 1;
            let (node_id, idx) = self.stack[self.stack_len];
            Some((node_id, idx as usize))
        }
    }

    #[inline]
    fn push_stack(&mut self, node_id: u32, idx: usize) {
        if self.stack_len < self.stack.len() {
            self.stack[self.stack_len] = (node_id, idx as u16);
            self.stack_len += 1;
        }
    }

    /// Zero-allocation iterator step: invokes `f(&key, value_slice)` directly on the pinned
    /// 4KB page slice without allocating a `Vec<u8>` on the heap.
    pub fn next_kv_with<R>(&mut self, mut f: impl FnMut(&[u8; KEY_SIZE], &[u8]) -> R) -> Option<R> {
        while let Some((node_id, index)) = self.pop_stack() {
            let step = {
                let guard = self
                    .tree
                    .bpm
                    .fetch_page(node_id as usize)
                    .expect("Failed to fetch node");
                let data = guard.data();
                let is_leaf = page_is_leaf(data);
                let num_keys = page_num_keys(data);
                if index < num_keys {
                    let cell_off = page_get_slot(data, is_leaf, index);
                    let (ck, cv) = page_cell(data, cell_off);
                    let key = expand_cell_key(ck);
                    let child = if !is_leaf {
                        Some(page_get_child(data, index + 1))
                    } else {
                        None
                    };
                    let out = f(&key, cv);
                    Some((out, child))
                } else {
                    None
                }
            };

            if let Some((out, next_child)) = step {
                self.push_stack(node_id, index + 1);

                if let Some(mut curr_id) = next_child {
                    loop {
                        let (is_leaf, first_child) = {
                            let guard = self
                                .tree
                                .bpm
                                .fetch_page(curr_id as usize)
                                .expect("Failed to fetch node");
                            let data = guard.data();
                            let is_leaf = page_is_leaf(data);
                            let c0 = if !is_leaf {
                                page_get_child(data, 0)
                            } else {
                                0
                            };
                            (is_leaf, c0)
                        };
                        self.push_stack(curr_id, 0);
                        if is_leaf {
                            break;
                        }
                        curr_id = first_child;
                    }
                }

                return Some(out);
            }
        }
        None
    }

    pub fn next_kv(&mut self) -> Option<([u8; KEY_SIZE], Vec<u8>)> {
        self.next_kv_with(|k, v| (*k, v.to_vec()))
    }

    pub fn next_key(&mut self) -> Option<[u8; KEY_SIZE]> {
        while let Some((node_id, index)) = self.pop_stack() {
            let (key_opt, next_child) = {
                let guard = self
                    .tree
                    .bpm
                    .fetch_page(node_id as usize)
                    .expect("Failed to fetch node");
                let data = guard.data();
                let is_leaf = page_is_leaf(data);
                let num_keys = page_num_keys(data);
                if index < num_keys {
                    let cell_off = page_get_slot(data, is_leaf, index);
                    let (ck, _) = page_cell(data, cell_off);
                    let key = expand_cell_key(ck);
                    let child = if !is_leaf {
                        Some(page_get_child(data, index + 1))
                    } else {
                        None
                    };
                    (Some(key), child)
                } else {
                    (None, None)
                }
            };

            if let Some(key) = key_opt {
                self.push_stack(node_id, index + 1);

                if let Some(mut curr_id) = next_child {
                    loop {
                        let (is_leaf, first_child) = {
                            let guard = self
                                .tree
                                .bpm
                                .fetch_page(curr_id as usize)
                                .expect("Failed to fetch node");
                            let data = guard.data();
                            let is_leaf = page_is_leaf(data);
                            let c0 = if !is_leaf {
                                page_get_child(data, 0)
                            } else {
                                0
                            };
                            (is_leaf, c0)
                        };
                        self.push_stack(curr_id, 0);
                        if is_leaf {
                            break;
                        }
                        curr_id = first_child;
                    }
                }

                return Some(key);
            }
        }
        None
    }
}
