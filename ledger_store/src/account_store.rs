use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, AccountInfo,
};

use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;
use alloc::sync::Arc;
use crate::LedgerCache;

pub struct BtreeAccountStore {
    cache: Arc<LedgerCache>,
}

impl BtreeAccountStore {
    pub const TABLE_ID: u8 = 1;

    pub fn new(cache: Arc<LedgerCache>) -> Self {
        Self { cache }
    }

    fn make_key(&self, account: &Account) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(account.as_bytes());
        key
    }

    pub fn put(&self, account: &Account, info: &AccountInfo) {
        let key = self.make_key(account);
        let mut value = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut value);
        info.serialize(&mut adapter);
        
        tracing::debug!("AccountStore: putting account {}, head={}", account.encode_account(), info.head);
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
                if let Ok(info) = AccountInfo::deserialize(&mut stream) {
                    tracing::debug!("AccountStore: found account {}, head={}", account.encode_account(), info.head);
                    return Some(info);
                }
            }
        }
        tracing::debug!("AccountStore: NOT found account {}", account.encode_account());
        None
    }

    pub fn del(&self, account: &Account) {
        let key = self.make_key(account);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn iter(&self, start: Account) -> alloc::vec::Vec<(Account, AccountInfo)> {
        let start_key = self.make_key(&start);
        let mut results = alloc::vec::Vec::new();
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            let mut it = db.iter(start_key);
            while let Some((key, value)) = it.next_kv() {
                if key[0] != Self::TABLE_ID {
                    break;
                }
                let account = Account::from_bytes(key[1..33].try_into().unwrap());
                let mut stream = BufferReader::new(&value);
                if let Ok(info) = AccountInfo::deserialize(&mut stream) {
                    results.push((account, info));
                }
            }
        }
        results
    }
}
