
use alloc::vec::Vec;
use kernel_common::storage::btree::KEY_SIZE;
use kernel_common::storage::{get_shard_index, SHARDS};

const COUNTS_MAGIC: u64 = 0x4E41_4E4F_434E_5431; // "NANOCNT1"
const REPTBL_MAGIC: u64 = 0x4E41_4E4F_5245_5031; // "NANOREP1"

pub struct BtreeVersionStore {}
impl BtreeVersionStore {
    pub const TABLE_ID: u8 = 11;
    pub fn new() -> Self {
        Self {}
    }

    fn counts_key() -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..9].copy_from_slice(b"CACHCNTS");
        key
    }

    fn reptbl_hdr_key() -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..9].copy_from_slice(b"REPTBLHD");
        key
    }

    fn reptbl_chunk_key(chunk: u8) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..8].copy_from_slice(b"REPTBLC");
        key[8] = chunk;
        key
    }

    pub fn get_rep_table(&self) -> Option<Vec<[u8; 32]>> {
        let hdr_key = Self::reptbl_hdr_key();
        let shard_idx = get_shard_index(&hdr_key);
        let mut lock = SHARDS[shard_idx].lock();
        let db = lock.as_mut()?;
        let count = db
            .get_with(&hdr_key, |val| {
                if val.len() < 12 {
                    return None;
                }
                let magic = u64::from_le_bytes(val[0..8].try_into().ok()?);
                if magic != REPTBL_MAGIC {
                    return None;
                }
                let cnt = u32::from_le_bytes(val[8..12].try_into().ok()?) as usize;
                if cnt == 0 || cnt > 256 {
                    return None;
                }
                Some(cnt)
            })
            .flatten()?;

        let mut reps = Vec::with_capacity(count);
        let num_chunks = (count + 7) / 8;
        for c in 0..num_chunks {
            let ckey = Self::reptbl_chunk_key(c as u8);
            let want = (count - c * 8).min(8);
            let ok = db
                .get_with(&ckey, |val| {
                    if val.len() < want * 32 {
                        return false;
                    }
                    for i in 0..want {
                        let mut k = [0u8; 32];
                        k.copy_from_slice(&val[i * 32..(i + 1) * 32]);
                        reps.push(k);
                    }
                    true
                })
                .unwrap_or(false);
            if !ok {
                return None;
            }
        }
        Some(reps)
    }

    pub fn put_rep_table(&self, reps: &[[u8; 32]]) {
        let count = reps.len().min(256);
        if count == 0 {
            return;
        }
        let hdr_key = Self::reptbl_hdr_key();
        let shard_idx = get_shard_index(&hdr_key);
        let mut lock = SHARDS[shard_idx].lock();
        let Some(db) = lock.as_mut() else {
            return;
        };
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&REPTBL_MAGIC.to_le_bytes());
        hdr[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        db.insert(hdr_key, &hdr);

        let num_chunks = (count + 7) / 8;
        for c in 0..num_chunks {
            let ckey = Self::reptbl_chunk_key(c as u8);
            let n = (count - c * 8).min(8);
            let mut buf = [0u8; 256];
            for i in 0..n {
                buf[i * 32..(i + 1) * 32].copy_from_slice(&reps[c * 8 + i]);
            }
            db.insert(ckey, &buf[..n * 32]);
        }
    }

    /// Ensures the active `rsnano_core` compact representative table matches the persisted
    /// NVMe `REPTBL` (if present), or persists the current table if initializing a fresh ledger.
    pub fn sync_compact_rep_table(&self) {
        if let Some(persisted) = self.get_rep_table() {
            let current = rsnano_core::compact_representatives();
            if current != persisted {
                #[cfg(not(test))]
                if !current.is_empty() {
                    tracing::warn!(
                        "[ledger] NVMe persisted compact_representatives ({} entries) differs from config.toml; using NVMe table to preserve block integrity",
                        persisted.len()
                    );
                }
                rsnano_core::set_compact_representatives(&persisted);
            }
        } else {
            let current = rsnano_core::compact_representatives();
            if !current.is_empty() {
                self.put_rep_table(&current);
            }
        }
    }

    pub fn get_cached_counts(&self) -> Option<(u64, u64, u64, u64)> {
        let key = Self::counts_key();
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        let db = lock.as_mut()?;
        db.get_with(&key, |val| {
            if val.len() < 32 {
                return None;
            }
            let magic = u64::from_le_bytes(val[0..8].try_into().ok()?);
            if magic != COUNTS_MAGIC {
                return None;
            }
            let account_count = u64::from_le_bytes(val[8..16].try_into().ok()?);
            let block_count = u64::from_le_bytes(val[16..24].try_into().ok()?);
            let confirmed_count = u64::from_le_bytes(val[24..32].try_into().ok()?);
            let pruned_count = if val.len() >= 40 {
                u64::from_le_bytes(val[32..40].try_into().ok()?)
            } else {
                0
            };
            if account_count == 0 || block_count == 0 {
                return None;
            }
            Some((account_count, block_count, confirmed_count, pruned_count))
        })
        .flatten()
    }

    pub fn put_cached_counts(
        &self,
        account_count: u64,
        block_count: u64,
        confirmed_count: u64,
        pruned_count: u64,
    ) {
        let key = Self::counts_key();
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&COUNTS_MAGIC.to_le_bytes());
        buf[8..16].copy_from_slice(&account_count.to_le_bytes());
        buf[16..24].copy_from_slice(&block_count.to_le_bytes());
        buf[24..32].copy_from_slice(&confirmed_count.to_le_bytes());
        buf[32..40].copy_from_slice(&pruned_count.to_le_bytes());
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buf);
        }
    }
}
