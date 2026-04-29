use rsnano_core::{QualifiedRoot, BlockHash};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeFinalVoteStore {}

impl BtreeFinalVoteStore {
    pub const TABLE_ID: u8 = 8;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, root: &QualifiedRoot) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..65].copy_from_slice(&root.to_bytes());
        key
    }

    pub fn put(&self, root: &QualifiedRoot, hash: &BlockHash) {
        let key = self.make_key(root);
        let mut value = [0u8; 256];
        value[..32].copy_from_slice(hash.as_bytes());
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, _tx: &dyn core::any::Any, root: &QualifiedRoot) -> Option<BlockHash> {
        let key = self.make_key(root);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                return Some(BlockHash::from_bytes(value[..32].try_into().unwrap()));
            }
        }
        None
    }

    pub fn del(&self, _root: &QualifiedRoot) {
        // B-tree currently doesn't support delete.
    }
}
