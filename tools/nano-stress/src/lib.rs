#![cfg_attr(target_arch = "wasm32", no_std)]

use core::cell::UnsafeCell;
use rsnano_sha::{blake2b, ed25519};

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

pub const STATE_BLOCK_WIRE_SIZE: usize = 216;
pub const MAX_BATCH_BLOCKS: usize = 256;
pub const MAX_ACCOUNTS: usize = 64;

/// Default Nano Dev Genesis Private Key:
/// 34F0A37AAD20F4A260F0A5B3CB3D7FB50673212263E58A380BC10474BB039CE4
const DEV_GENESIS_PRIV: [u8; 32] = [
    0x34, 0xF0, 0xA3, 0x7A, 0xAD, 0x20, 0xF4, 0xA2, 0x60, 0xF0, 0xA5, 0xB3, 0xCB, 0x3D, 0x7F, 0xB5,
    0x06, 0x73, 0x21, 0x22, 0x63, 0xE5, 0x8A, 0x38, 0x0B, 0xC1, 0x04, 0x74, 0xBB, 0x03, 0x9C, 0xE4,
];

#[derive(Clone, Copy)]
struct WasmAccountTrack {
    priv_key: [u8; 32],
    pub_key: [u8; 32],
    frontier: [u8; 32],
    balance: u128,
    representative: [u8; 32],
    is_open: bool,
}

impl WasmAccountTrack {
    const ZERO: Self = Self {
        priv_key: [0u8; 32],
        pub_key: [0u8; 32],
        frontier: [0u8; 32],
        balance: 0,
        representative: [0u8; 32],
        is_open: false,
    };
}

struct WasmStressState {
    genesis_priv: [u8; 32],
    genesis_pub: [u8; 32],
    current_frontier: [u8; 32],
    current_balance_be: [u8; 16],
    pow_threshold: u64,
    strategy: u32,
    num_accounts: usize,
    round_robin_idx: usize,
    rng_state: u64,
    seq_counter: u64,
    accounts: [WasmAccountTrack; MAX_ACCOUNTS],
    out_blocks: [u8; MAX_BATCH_BLOCKS * STATE_BLOCK_WIRE_SIZE],
    out_hashes: [u8; MAX_BATCH_BLOCKS * 32],
    pow_root: [u8; 32],
    pow_out_be: [u8; 8],
    pow_nonce_cursor: u64,
}

struct SyncState(UnsafeCell<WasmStressState>);
// SAFETY: WebAssembly instances are single-threaded.
unsafe impl Sync for SyncState {}

static STATE: SyncState = SyncState(UnsafeCell::new(WasmStressState {
    genesis_priv: DEV_GENESIS_PRIV,
    genesis_pub: [0u8; 32],
    current_frontier: [0u8; 32],
    current_balance_be: [0xFF; 16],
    pow_threshold: 0xffc0_0000_0000_0000,
    strategy: 0,
    num_accounts: 1,
    round_robin_idx: 0,
    rng_state: 0x853c_49e6_748f_ea9b,
    seq_counter: 1000,
    accounts: [WasmAccountTrack::ZERO; MAX_ACCOUNTS],
    out_blocks: [0u8; MAX_BATCH_BLOCKS * STATE_BLOCK_WIRE_SIZE],
    out_hashes: [0u8; MAX_BATCH_BLOCKS * 32],
    pow_root: [0u8; 32],
    pow_out_be: [0u8; 8],
    pow_nonce_cursor: 1,
}));

#[inline]
#[allow(clippy::mut_from_ref)]
unsafe fn state() -> &'static mut WasmStressState {
    &mut *STATE.0.get()
}

#[inline]
fn next_u64(rng_state: &mut u64) -> u64 {
    let mut x = *rng_state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *rng_state = x;
    x
}

#[inline]
fn key_from_u64(val: u64) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[24..32].copy_from_slice(&val.to_be_bytes());
    k
}

#[inline]
fn compute_work_batch4(rng_state: &mut u64, threshold: u64, root: &[u8; 32]) -> u64 {
    if threshold == 0 {
        return 0;
    }
    let mut nonce = next_u64(rng_state);
    let mut inputs = [[0u8; 40]; 4];
    for item in &mut inputs {
        item[8..40].copy_from_slice(root);
    }
    loop {
        let n0 = nonce;
        let n1 = nonce.wrapping_add(1);
        let n2 = nonce.wrapping_add(2);
        let n3 = nonce.wrapping_add(3);
        inputs[0][0..8].copy_from_slice(&n0.to_le_bytes());
        inputs[1][0..8].copy_from_slice(&n1.to_le_bytes());
        inputs[2][0..8].copy_from_slice(&n2.to_le_bytes());
        inputs[3][0..8].copy_from_slice(&n3.to_le_bytes());

        let diffs = blake2b::hash8_batch_4([
            &inputs[0][..],
            &inputs[1][..],
            &inputs[2][..],
            &inputs[3][..],
        ]);
        if diffs[0] >= threshold {
            return n0;
        }
        if diffs[1] >= threshold {
            return n1;
        }
        if diffs[2] >= threshold {
            return n2;
        }
        if diffs[3] >= threshold {
            return n3;
        }
        nonce = nonce.wrapping_add(4);
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn build_and_serialize_state_block(
    priv_key: &[u8; 32],
    account_pub: &[u8; 32],
    previous: &[u8; 32],
    representative: &[u8; 32],
    balance: u128,
    link: &[u8; 32],
    work: u64,
    out_wire: &mut [u8],
) -> [u8; 32] {
    let mut hash_input = [0u8; 176];
    hash_input[31] = 6; // BlockType::State = 6
    hash_input[32..64].copy_from_slice(account_pub);
    hash_input[64..96].copy_from_slice(previous);
    hash_input[96..128].copy_from_slice(representative);
    let bal_be = balance.to_be_bytes();
    hash_input[128..144].copy_from_slice(&bal_be);
    hash_input[144..176].copy_from_slice(link);

    let hash = blake2b::hash(&hash_input);
    let sig = ed25519::sign(priv_key, &hash);

    out_wire[0..32].copy_from_slice(account_pub);
    out_wire[32..64].copy_from_slice(previous);
    out_wire[64..96].copy_from_slice(representative);
    out_wire[96..112].copy_from_slice(&bal_be);
    out_wire[112..144].copy_from_slice(link);
    out_wire[144..208].copy_from_slice(&sig);
    out_wire[208..216].copy_from_slice(&work.to_be_bytes());

    hash
}

#[no_mangle]
pub extern "C" fn wasm_init() {
    ed25519::init_radix16_precomp();
    // SAFETY: Single-threaded WebAssembly instance initialization.
    let st = unsafe { state() };
    st.genesis_priv = DEV_GENESIS_PRIV;
    st.genesis_pub = ed25519::derive_public_key(&st.genesis_priv);

    // Compute DEV_GENESIS_BLOCK open block hash = blake2b(source || rep || account)
    let mut open_hashables = [0u8; 96];
    open_hashables[0..32].copy_from_slice(&st.genesis_pub);
    open_hashables[32..64].copy_from_slice(&st.genesis_pub);
    open_hashables[64..96].copy_from_slice(&st.genesis_pub);
    st.current_frontier = blake2b::hash(&open_hashables);
    st.current_balance_be = [0xFF; 16];
    st.seq_counter = 1000;
}

#[no_mangle]
pub extern "C" fn wasm_get_frontier_ptr() -> *mut u8 {
    // SAFETY: Returns pointer to static WASM linear memory buffer.
    unsafe { state().current_frontier.as_mut_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_get_balance_ptr() -> *mut u8 {
    // SAFETY: Returns pointer to static WASM linear memory buffer.
    unsafe { state().current_balance_be.as_mut_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_get_privkey_ptr() -> *mut u8 {
    // SAFETY: Returns pointer to static WASM linear memory buffer.
    unsafe { state().genesis_priv.as_mut_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_get_pubkey_ptr() -> *const u8 {
    // SAFETY: Derives public key from current genesis_priv in static buffer.
    let st = unsafe { state() };
    st.genesis_pub = ed25519::derive_public_key(&st.genesis_priv);
    st.genesis_pub.as_ptr()
}

#[no_mangle]
pub extern "C" fn wasm_get_batch_ptr() -> *const u8 {
    // SAFETY: Returns pointer to static WASM output buffer.
    unsafe { state().out_blocks.as_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_get_hashes_ptr() -> *const u8 {
    // SAFETY: Returns pointer to static WASM hashes buffer.
    unsafe { state().out_hashes.as_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_set_config(
    threshold_hi: u32,
    threshold_lo: u32,
    strategy: u32,
    num_accounts: u32,
    seed: u32,
) {
    // SAFETY: Single-threaded WebAssembly configuration update.
    let st = unsafe { state() };
    st.pow_threshold = ((threshold_hi as u64) << 32) | (threshold_lo as u64);
    st.strategy = strategy;
    st.genesis_pub = ed25519::derive_public_key(&st.genesis_priv);
    st.rng_state = (seed as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x853c_49e6_748f_ea9b;
    if st.rng_state == 0 {
        st.rng_state = 1;
    }
    st.round_robin_idx = 0;

    let n = (num_accounts as usize).min(MAX_ACCOUNTS);
    st.num_accounts = if n <= 1 { 1 } else { n };

    if st.num_accounts > 1 {
        let base_seed = next_u64(&mut st.rng_state);
        for i in 0..st.num_accounts {
            let val = (base_seed ^ (i as u64)).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let priv_key = key_from_u64(val);
            let pub_key = ed25519::derive_public_key(&priv_key);
            st.accounts[i] = WasmAccountTrack {
                priv_key,
                pub_key,
                frontier: [0u8; 32],
                balance: 0,
                representative: pub_key,
                is_open: false,
            };
        }
    }
}

#[no_mangle]
pub extern "C" fn wasm_generate_batch(count: u32) -> u32 {
    let count = (count as usize).clamp(1, MAX_BATCH_BLOCKS);
    // SAFETY: Single-threaded WebAssembly batch generator operating on static buffers.
    let st = unsafe { state() };
    let threshold = st.pow_threshold;
    let strategy = st.strategy;
    let mut gen_balance = u128::from_be_bytes(st.current_balance_be);
    let open_amount: u128 = 1_000_000_000_000_000;
    let mut written = 0usize;

    if st.num_accounts <= 1 {
        for _ in 0..count {
            let start = written * STATE_BLOCK_WIRE_SIZE;
            let end = start + STATE_BLOCK_WIRE_SIZE;
            let frontier = st.current_frontier;
            let genesis_priv = st.genesis_priv;
            let genesis_pub = st.genesis_pub;

            let work = compute_work_batch4(&mut st.rng_state, threshold, &frontier);
            let hash = if strategy == 0 {
                // Strategy::Chained
                gen_balance = gen_balance.saturating_sub(1000);
                let dest_priv = key_from_u64(st.seq_counter);
                st.seq_counter = st.seq_counter.wrapping_add(1);
                let dest_pub = ed25519::derive_public_key(&dest_priv);

                build_and_serialize_state_block(
                    &genesis_priv,
                    &genesis_pub,
                    &frontier,
                    &genesis_pub,
                    gen_balance,
                    &dest_pub,
                    work,
                    &mut st.out_blocks[start..end],
                )
            } else {
                // Strategy::Change
                let rep_bytes = key_from_u64(st.seq_counter);
                st.seq_counter = st.seq_counter.wrapping_add(1);
                let zero_link = [0u8; 32];

                build_and_serialize_state_block(
                    &genesis_priv,
                    &genesis_pub,
                    &frontier,
                    &rep_bytes,
                    gen_balance,
                    &zero_link,
                    work,
                    &mut st.out_blocks[start..end],
                )
            };

            st.current_frontier = hash;
            st.out_hashes[written * 32..(written + 1) * 32].copy_from_slice(&hash);
            written += 1;
        }
    } else {
        let mut remaining = count;
        let num_accounts = st.num_accounts;
        let genesis_priv = st.genesis_priv;
        let genesis_pub = st.genesis_pub;

        for i in 0..num_accounts {
            if !st.accounts[i].is_open && remaining >= 2 {
                let acc_priv = st.accounts[i].priv_key;
                let acc_pub = st.accounts[i].pub_key;
                let acc_rep = st.accounts[i].representative;
                let frontier = st.current_frontier;

                // 1. Send from genesis to child account
                gen_balance = gen_balance.saturating_sub(open_amount);
                let work_send = compute_work_batch4(&mut st.rng_state, threshold, &frontier);
                let s_start = written * STATE_BLOCK_WIRE_SIZE;
                let s_end = s_start + STATE_BLOCK_WIRE_SIZE;
                let send_hash = build_and_serialize_state_block(
                    &genesis_priv,
                    &genesis_pub,
                    &frontier,
                    &genesis_pub,
                    gen_balance,
                    &acc_pub,
                    work_send,
                    &mut st.out_blocks[s_start..s_end],
                );
                st.current_frontier = send_hash;
                st.out_hashes[written * 32..(written + 1) * 32].copy_from_slice(&send_hash);
                written += 1;

                // 2. Open block on child account (root = child pub_key)
                let work_open = compute_work_batch4(&mut st.rng_state, threshold, &acc_pub);
                let o_start = written * STATE_BLOCK_WIRE_SIZE;
                let o_end = o_start + STATE_BLOCK_WIRE_SIZE;
                let zero_prev = [0u8; 32];
                let open_hash = build_and_serialize_state_block(
                    &acc_priv,
                    &acc_pub,
                    &zero_prev,
                    &acc_rep,
                    open_amount,
                    &send_hash,
                    work_open,
                    &mut st.out_blocks[o_start..o_end],
                );
                st.accounts[i].frontier = open_hash;
                st.accounts[i].balance = open_amount;
                st.accounts[i].is_open = true;
                st.out_hashes[written * 32..(written + 1) * 32].copy_from_slice(&open_hash);
                written += 1;

                remaining -= 2;
            }
        }

        for _ in 0..remaining {
            let acc_idx = st.round_robin_idx % num_accounts;
            st.round_robin_idx = st.round_robin_idx.wrapping_add(1);

            let start = written * STATE_BLOCK_WIRE_SIZE;
            let end = start + STATE_BLOCK_WIRE_SIZE;

            if !st.accounts[acc_idx].is_open {
                let frontier = st.current_frontier;
                gen_balance = gen_balance.saturating_sub(1000);
                let dest_priv = key_from_u64(next_u64(&mut st.rng_state));
                let dest_pub = ed25519::derive_public_key(&dest_priv);
                let work = compute_work_batch4(&mut st.rng_state, threshold, &frontier);
                let hash = build_and_serialize_state_block(
                    &genesis_priv,
                    &genesis_pub,
                    &frontier,
                    &genesis_pub,
                    gen_balance,
                    &dest_pub,
                    work,
                    &mut st.out_blocks[start..end],
                );
                st.current_frontier = hash;
                st.out_hashes[written * 32..(written + 1) * 32].copy_from_slice(&hash);
                written += 1;
                continue;
            }

            let acc_priv = st.accounts[acc_idx].priv_key;
            let acc_pub = st.accounts[acc_idx].pub_key;
            let acc_frontier = st.accounts[acc_idx].frontier;
            let work = compute_work_batch4(&mut st.rng_state, threshold, &acc_frontier);

            let hash = if strategy == 0 {
                st.accounts[acc_idx].balance = st.accounts[acc_idx].balance.saturating_sub(1000);
                let acc_bal = st.accounts[acc_idx].balance;
                let acc_rep = st.accounts[acc_idx].representative;
                let dest_priv = key_from_u64(next_u64(&mut st.rng_state));
                let dest_pub = ed25519::derive_public_key(&dest_priv);
                build_and_serialize_state_block(
                    &acc_priv,
                    &acc_pub,
                    &acc_frontier,
                    &acc_rep,
                    acc_bal,
                    &dest_pub,
                    work,
                    &mut st.out_blocks[start..end],
                )
            } else {
                let new_rep = key_from_u64(next_u64(&mut st.rng_state));
                st.accounts[acc_idx].representative = new_rep;
                let acc_bal = st.accounts[acc_idx].balance;
                let zero_link = [0u8; 32];
                build_and_serialize_state_block(
                    &acc_priv,
                    &acc_pub,
                    &acc_frontier,
                    &new_rep,
                    acc_bal,
                    &zero_link,
                    work,
                    &mut st.out_blocks[start..end],
                )
            };
            st.accounts[acc_idx].frontier = hash;
            st.out_hashes[written * 32..(written + 1) * 32].copy_from_slice(&hash);
            written += 1;
        }
    }

    st.current_balance_be = gen_balance.to_be_bytes();
    (written * STATE_BLOCK_WIRE_SIZE) as u32
}

#[no_mangle]
pub extern "C" fn wasm_get_pow_root_ptr() -> *mut u8 {
    // SAFETY: Returns pointer to static WASM buffer.
    unsafe { state().pow_root.as_mut_ptr() }
}

#[no_mangle]
pub extern "C" fn wasm_get_pow_out_ptr() -> *const u8 {
    // SAFETY: Returns pointer to static WASM buffer.
    unsafe { state().pow_out_be.as_ptr() }
}

/// Chunked PoW solver for Browser Wallet / UI so the main thread or worker can yield.
/// Returns 1 if a valid nonce was found and written to `pow_out_be`, or 0 if `max_iters`
/// elapsed without hitting the threshold.
#[no_mangle]
pub extern "C" fn wasm_compute_pow_step(
    threshold_hi: u32,
    threshold_lo: u32,
    max_iters: u32,
) -> u32 {
    let threshold = ((threshold_hi as u64) << 32) | (threshold_lo as u64);
    // SAFETY: Single-threaded WebAssembly PoW helper.
    let st = unsafe { state() };
    let mut nonce = st.pow_nonce_cursor.wrapping_add(next_u64(&mut st.rng_state));
    let mut inputs = [[0u8; 40]; 4];
    for item in &mut inputs {
        item[8..40].copy_from_slice(&st.pow_root);
    }
    let batches = (max_iters / 4).max(1);
    for _ in 0..batches {
        let n0 = nonce;
        let n1 = nonce.wrapping_add(1);
        let n2 = nonce.wrapping_add(2);
        let n3 = nonce.wrapping_add(3);
        inputs[0][0..8].copy_from_slice(&n0.to_le_bytes());
        inputs[1][0..8].copy_from_slice(&n1.to_le_bytes());
        inputs[2][0..8].copy_from_slice(&n2.to_le_bytes());
        inputs[3][0..8].copy_from_slice(&n3.to_le_bytes());

        let diffs = blake2b::hash8_batch_4([
            &inputs[0][..],
            &inputs[1][..],
            &inputs[2][..],
            &inputs[3][..],
        ]);
        for (k, &d) in diffs.iter().enumerate() {
            if d >= threshold {
                let winner = nonce.wrapping_add(k as u64);
                st.pow_out_be = winner.to_be_bytes();
                st.pow_nonce_cursor = winner.wrapping_add(1);
                return 1;
            }
        }
        nonce = nonce.wrapping_add(4);
    }
    st.pow_nonce_cursor = nonce;
    0
}
