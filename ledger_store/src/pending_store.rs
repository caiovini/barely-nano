use rsnano_core::{PendingKey, PendingInfo, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;
use alloc::vec::Vec;

pub struct BtreePendingStore {}

impl BtreePendingStore {
    pub const TABLE_ID: u8 = 7;

    pub fn new() -> Self {
        Self {}
    }

    pub fn put(&self, key: &PendingKey, info: &PendingInfo) {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let mut val_bytes = [0u8; 256];
        let mut stream = MutStreamAdapter::new(&mut val_bytes);
        info.serialize(&mut stream);

        let mut guard = DB.lock();
        if let Some(btree) = guard.as_mut() {
            btree.insert(key_bytes, val_bytes);
        }
    }

    pub fn get(&self, key: &PendingKey) -> Option<PendingInfo> {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let mut guard = DB.lock();
        let btree = guard.as_mut()?;
        let val_bytes = btree.search(&key_bytes)?;
        
        let mut reader = BufferReader::new(&val_bytes);
        PendingInfo::deserialize(&mut reader).ok()
    }

    pub fn del(&self, key: &PendingKey) {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let mut guard = DB.lock();
        if let Some(btree) = guard.as_mut() {
            btree.delete(key_bytes);
        }
    }

    pub fn iter(&self) -> Vec<(PendingKey, PendingInfo)> {
        let mut results = Vec::new();
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        let mut guard = DB.lock();
        if let Some(btree) = guard.as_mut() {
            let mut it = btree.iter(start_key);
            while let Some((k, v)) = it.next_kv() {
                if k[0] != Self::TABLE_ID {
                    break;
                }
                let mut reader = BufferReader::new(&v);
                if let Ok(info) = PendingInfo::deserialize(&mut reader) {
                    let mut key_reader = BufferReader::new(&k[1..]);
                    if let Ok(key) = PendingKey::deserialize(&mut key_reader) {
                        results.push((key, info));
                    }
                }
            }
        }
        results
    }
}
