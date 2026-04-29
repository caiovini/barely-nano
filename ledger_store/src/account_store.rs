use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, AccountInfo,
};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeAccountStore {}

impl BtreeAccountStore {
    pub const TABLE_ID: u8 = 1;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, account: &Account) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(account.as_bytes());
        key
    }

    pub fn put(&self, account: &Account, info: &AccountInfo) {
        let key = self.make_key(account);
        let mut value = [0u8; 256];
        let mut adapter = MutStreamAdapter::new(&mut value);
        info.serialize(&mut adapter);
        
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, account: &Account) -> Option<AccountInfo> {
        let key = self.make_key(account);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut stream = BufferReader::new(&value);
                return AccountInfo::deserialize(&mut stream).ok();
            }
        }
        None
    }

    pub fn del(&self, account: &Account) {
        let key = self.make_key(account);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }
}
