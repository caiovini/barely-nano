use rsnano_core::{PublicKey, Amount, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeRepWeightStore {}

impl BtreeRepWeightStore {
    pub const TABLE_ID: u8 = 12;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, pub_key: &PublicKey) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(pub_key.as_bytes());
        key
    }

    pub fn put(&self, pub_key: PublicKey, amount: Amount) {
        let key = self.make_key(&pub_key);
        let mut buffer = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        amount.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buffer[0..len]);
        }
    }

    pub fn get(&self, _tx: &dyn core::any::Any, pub_key: &PublicKey) -> Option<Amount> {
        let key = self.make_key(pub_key);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut reader = BufferReader::new(&value);
                return Amount::deserialize(&mut reader).ok();
            }
        }
        None
    }
}
