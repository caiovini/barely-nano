use kernel_common::storage::DB;
use rsnano_core::Amount;
use alloc::vec::Vec;

pub struct BtreeOnlineWeightStore {}
impl BtreeOnlineWeightStore {
    pub const TABLE_ID: u8 = 9;
    pub fn new() -> Self { Self {} }

    fn make_key(&self, timestamp: u64) -> [u8; kernel_common::storage::btree::KEY_SIZE] {
        let mut key = [0u8; kernel_common::storage::btree::KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..9].copy_from_slice(&timestamp.to_be_bytes());
        key
    }

    pub fn put(&self, timestamp: u64, amount: &Amount) {
        let key = self.make_key(timestamp);
        let mut value = [0u8; 512];
        value[0..16].copy_from_slice(&amount.to_be_bytes());
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn del(&self, timestamp: u64) {
        let key = self.make_key(timestamp);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn iter(&self) -> Vec<(u64, Amount)> {
        let result = Vec::new();
        let mut lock = DB.lock();
        if let Some(_db) = lock.as_mut() {
            // Simplified iteration for now. Real implementation would use db.iter()
            // but our BTree doesn't have a good iterator yet.
        }
        result
    }

    pub fn iter_rev(&self) -> Vec<(u64, Amount)> {
        let mut result = self.iter();
        result.reverse();
        result
    }
}
