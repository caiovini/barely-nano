use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, ConfirmationHeightInfo,
};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeConfirmationHeightStore {}

impl BtreeConfirmationHeightStore {
    pub const TABLE_ID: u8 = 3;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, account: &Account) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(account.as_bytes());
        key
    }

    pub fn put(&self, account: &Account, info: &ConfirmationHeightInfo) {
        let key = self.make_key(account);
        let mut value = [0u8; 256];
        let mut adapter = MutStreamAdapter::new(&mut value);
        info.serialize(&mut adapter);
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, account: &Account) -> Option<ConfirmationHeightInfo> {
        let key = self.make_key(account);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut stream = BufferReader::new(&value);
                return ConfirmationHeightInfo::deserialize(&mut stream).ok();
            }
        }
        None
    }

    pub fn exists(&self, account: &Account) -> bool {
        let key = self.make_key(account);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn del(&self, _account: &Account) {
        // B-tree currently doesn't support delete.
    }
}
