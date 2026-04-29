use rsnano_core::BlockHash;
use kernel_common::storage::DB;
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
        let value = [1u8; 256]; // Just store '1' to indicate pruned
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn exists(&self, hash: &BlockHash) -> bool {
        let key = self.make_key(hash);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn del(&self, _hash: &BlockHash) {
        // B-tree currently doesn't support delete.
    }

    pub fn count(&self, _tx: &dyn core::any::Any) -> u64 {
        // Mock count for now
        0
    }
}
