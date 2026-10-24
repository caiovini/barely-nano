use rsnano_core::BlockHash;
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreePrunedStore {}

impl BtreePrunedStore {
    pub const TABLE_ID: u8 = 5;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let value = &[]; // Just mark existence
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn exists(&self, hash: &BlockHash) -> bool {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn del(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn count(&self, _tx: &dyn core::any::Any) -> u64 {
        let mut count = 0;
        for shard in SHARDS.iter() {
            let mut lock = shard.lock();
            if let Some(db) = lock.as_mut() {
                count += db.count_prefixed(Self::TABLE_ID) as u64;
            }
        }
        count
    }
}
