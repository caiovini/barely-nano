use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
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
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &value[0..16]);
        }
    }

    pub fn del(&self, timestamp: u64) {
        let key = self.make_key(timestamp);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(u64, Amount),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some((k, v)) = it.next_kv() {
                    if k[0] != Self::TABLE_ID {
                        break;
                    }
                    let timestamp = u64::from_be_bytes(k[1..9].try_into().unwrap());
                    let amount = Amount::from_be_bytes(v[0..16].try_into().unwrap());
                    f(timestamp, amount);
                }
            }
        }
    }

    pub fn iter(&self) -> Vec<(u64, Amount)> {
        let mut result = Vec::new();
        self.for_each(|ts, amt| result.push((ts, amt)));
        result
    }

    pub fn iter_rev(&self) -> Vec<(u64, Amount)> {
        let mut result = self.iter();
        result.reverse();
        result
    }
}
