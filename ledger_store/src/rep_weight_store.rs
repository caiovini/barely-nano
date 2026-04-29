use rsnano_core::{PublicKey, Amount, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeRepWeightStore {}

impl BtreeRepWeightStore {
    pub const TABLE_ID: u8 = 6;

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
        let mut value = [0u8; 256];
        let mut adapter = MutStreamAdapter::new(&mut value);
        amount.serialize(&mut adapter);
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, _tx: &dyn core::any::Any, pub_key: &PublicKey) -> Option<Amount> {
        let key = self.make_key(pub_key);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut reader = BufferReader::new(&value);
                return Amount::deserialize(&mut reader).ok();
            }
        }
        None
    }
}
