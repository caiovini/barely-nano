
use kernel_common::storage::btree::KEY_SIZE;
use kernel_common::storage::{get_shard_index, SHARDS};

const COUNTS_MAGIC: u64 = 0x4E41_4E4F_434E_5431; // "NANOCNT1"

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
