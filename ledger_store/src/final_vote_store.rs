use rsnano_core::{BlockHash, Root};
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeFinalVoteStore {}

impl BtreeFinalVoteStore {
    pub const TABLE_ID: u8 = 8;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, root: &Root) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(root.as_bytes());
        key
    }

    pub fn put(&self, root: &Root, hash: &BlockHash) {
        let key = self.make_key(root);
        let value = hash.as_bytes();
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, root: &Root) -> Option<BlockHash> {
        let key = self.make_key(root);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                return Some(BlockHash::from_bytes(value[0..32].try_into().unwrap()));
            }
        }
        None
    }

    pub fn del(&self, _root: &Root) {
        // B-tree currently doesn't support delete.
    }
}
