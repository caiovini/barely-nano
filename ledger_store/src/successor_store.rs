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
