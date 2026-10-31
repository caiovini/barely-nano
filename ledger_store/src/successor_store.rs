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
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, successor.as_bytes());
        }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<BlockHash> {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(res) = db.get_with(&key, |value| {
                if value.len() == 32 {
                    Some(BlockHash::from_bytes(value[0..32].try_into().unwrap()))
                } else {
                    tracing::debug!("SuccessorStore: garbage value found for hash {}, length={}", hash, value.len());
                    None
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
            db.delete(key);
        }
    }
}
