use rsnano_core::{BlockHash, SavedBlock};
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;
use crate::BtreeBlockStore;

pub struct BtreeSuccessorStore {}

impl BtreeSuccessorStore {
    pub const TABLE_ID: u8 = BtreeBlockStore::TABLE_ID;

    pub fn new() -> Self {
        Self {}
    }

    #[inline]
    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, hash: &BlockHash, successor: &BlockHash) {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            let _ = db.update_compact_successor_32(&key, successor.as_bytes());
            drop(lock);
            if !cfg!(test) {
                crate::hot_block_set_successor(hash, *successor);
            }
        }
    }

    /// Updates 32-byte block successors for a key-sorted slice of `(BlockHash, BlockHash)`,
    /// acquiring each shard B-Tree lock and `HOT_BLOCKS` shard lock at most once per shard.
    pub fn put_batch(&self, items: &[(BlockHash, BlockHash)]) {
        if items.is_empty() {
            return;
        }
        let mut i = 0;
        let n = items.len();
        while i < n {
            let shard_idx = (items[i].0.as_bytes()[0] >> 2) as usize;
            let mut j = i + 1;
            while j < n && ((items[j].0.as_bytes()[0] >> 2) as usize) == shard_idx {
                j += 1;
            }
            let shard_items = &items[i..j];
            let mut has_db = false;
            {
                let mut lock = SHARDS[shard_idx].lock();
                if let Some(db) = lock.as_mut() {
                    has_db = true;
                    for (hash, successor) in shard_items {
                        let key = self.make_key(hash);
                        let _ = db.update_compact_successor_32(&key, successor.as_bytes());
                    }
                }
            }
            if has_db && !cfg!(test) {
                crate::hot_block_set_successor_batch(shard_idx, shard_items);
            }
            i = j;
        }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<BlockHash> {
        if !BtreeBlockStore::may_exist_global(hash) {
            return None;
        }
        if !cfg!(test) {
            if let Some(cached) = crate::hot_block_get_successor(hash) {
                return cached;
            }
        }
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(res) = db.get_with(&key, |value| {
                let succ = SavedBlock::extract_compact_successor(value)?;
                if succ.is_zero() {
                    None
                } else {
                    Some(succ)
                }
            }) {
                return res;
            }
        }
        None
    }

    pub fn del(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            let _ = db.update_compact_successor_32(&key, &[0u8; 32]);
            drop(lock);
            if !cfg!(test) {
                crate::hot_block_set_successor(hash, BlockHash::zero());
            }
        }
    }
}
