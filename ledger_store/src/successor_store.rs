use rsnano_core::BlockHash;
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeSuccessorStore {}

impl BtreeSuccessorStore {
    pub const TABLE_ID: u8 = 4;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, hash: &BlockHash, successor: &BlockHash) {
        let key = self.make_key(hash);
        let mut value = [0u8; 512];
        value[0..32].copy_from_slice(successor.as_bytes());
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &value[0..32]);
        }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<BlockHash> {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                if value.len() == 32 {
                    return Some(BlockHash::from_bytes(value[0..32].try_into().unwrap()));
                } else {
                    tracing::debug!("SuccessorStore: garbage value found for hash {}, length={}", hash, value.len());
                }
            }
        }
        None
    }

    pub fn del(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }
}
