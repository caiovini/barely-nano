use kernel_common::storage::DB;
use rsnano_core::utils::SocketAddrV6;
use rsnano_nullable_clock::SystemTime;
use alloc::vec::Vec;

pub struct BtreePeerStore {}
impl BtreePeerStore {
    pub const TABLE_ID: u8 = 6;
    pub fn new() -> Self { Self {} }

    fn make_key(&self, endpoint: &SocketAddrV6) -> [u8; kernel_common::storage::btree::KEY_SIZE] {
        let mut key = [0u8; kernel_common::storage::btree::KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..17].copy_from_slice(&endpoint.ip().octets());
        key[17..19].copy_from_slice(&endpoint.port().to_be_bytes());
        key
    }

    pub fn put(&self, endpoint: SocketAddrV6, timestamp: SystemTime) {
        let key = self.make_key(&endpoint);
        let mut value = [0u8; 256];
        // value[0..8].copy_from_slice(&timestamp.as_secs().to_be_bytes());
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn del(&self, endpoint: SocketAddrV6) {
        let key = self.make_key(&endpoint);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn exists(&self, endpoint: SocketAddrV6) -> bool {
        let key = self.make_key(&endpoint);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn iter(&self) -> Vec<(SocketAddrV6, SystemTime)> {
        let mut result = Vec::new();
        // Simplified iteration for now
        result
    }
}
