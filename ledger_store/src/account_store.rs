use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, AccountInfo,
};

use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::sync::Arc;
use crate::LedgerCache;

pub struct BtreeAccountStore {
    #[allow(dead_code)]
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
        let mut buffer = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        info.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            let is_new = db.search(&key).is_none();
            db.insert(key, &buffer[0..len]);
            if is_new {
                self.cache.account_count.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    pub fn get(&self, account: &Account) -> Option<AccountInfo> {
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut stream = BufferReader::new(&value);
                if let Ok(info) = AccountInfo::deserialize(&mut stream) {
                    tracing::debug!("AccountStore: found account {}, head={}", account.encode_account(), info.head);
                    return Some(info);
                }
            }
        }
        // tracing::debug!("AccountStore: NOT found account {}", account.encode_account());
        None
    }

    pub fn del(&self, account: &Account) {
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if db.search(&key).is_some() {
                db.delete(key);
                self.cache.account_count.fetch_sub(1, core::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    pub fn scan_count(&self) -> usize {
        let mut count = 0;
        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                count += db.count_prefixed(Self::TABLE_ID);
            }
        }
        count
    }

    pub fn count(&self) -> usize {
        self.cache.account_count.load(core::sync::atomic::Ordering::SeqCst) as usize
    }

    pub fn for_each<F>(&self, start: Account, mut f: F)
    where
        F: FnMut(Account, AccountInfo),
    {
        let start_key = self.make_key(&start);

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some((key, value)) = it.next_kv() {
                    if key[0] != Self::TABLE_ID {
                        break;
                    }
                    let account = Account::from_bytes(key[1..33].try_into().unwrap());
                    let mut stream = BufferReader::new(&value);
                    if let Ok(info) = AccountInfo::deserialize(&mut stream) {
                        f(account, info);
                    }
                }
            }
        }
    }

    pub fn iter(&self, start: Account) -> alloc::vec::Vec<(Account, AccountInfo)> {
        let mut results = alloc::vec::Vec::new();
        self.for_each(start, |acc, info| results.push((acc, info)));
        results
    }
}
