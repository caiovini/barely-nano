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
            db.insert(key, &buffer[0..len]);
        }
    }

    pub fn get(&self, account: &Account) -> Option<AccountInfo> {
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(Some(info)) = db.get_with(&key, |value| {
                let mut stream = BufferReader::new(value);
                AccountInfo::deserialize(&mut stream).ok()
            }) {
                tracing::debug!("AccountStore: found account {}, head={}", account.encode_account(), info.head);
                return Some(info);
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
            db.delete(key);
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

    /// Single-pass scan over the accounts table (matching RSNano `Ledger::initialize`),
    /// returning `(account_count, total_block_count)` without scanning the blocks table.
    pub fn scan_counts(&self) -> (u64, u64) {
        let mut account_count = 0u64;
        let mut block_count = 0u64;
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut stream = BufferReader::new(value);
                    Ok(AccountInfo::deserialize(&mut stream)
                        .ok()
                        .map(|info| info.block_count))
                }) {
                    match step {
                        Ok(Some(bc)) => {
                            account_count += 1;
                            block_count += bc;
                            if account_count % 1000 == 0 {
                                kernel_common::watchdog::kick();
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
        (account_count, block_count)
    }

    pub fn count(&self) -> usize {
        self.cache.account_count.load(core::sync::atomic::Ordering::Relaxed) as usize
    }

    pub fn for_each_while<F>(&self, start: Account, mut f: F)
    where
        F: FnMut(Account, AccountInfo) -> bool,
    {
        let start_key = self.make_key(&start);
        let start_shard = get_shard_index(&start_key);

        for i in start_shard..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let seek_key = if i == start_shard {
                    start_key
                } else {
                    let mut k = [0u8; KEY_SIZE];
                    k[0] = Self::TABLE_ID;
                    k[1] = (i as u8) << 4;
                    k
                };
                let mut it = db.iter(seek_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let account = Account::from_bytes(key[1..33].try_into().unwrap());
                    let mut stream = BufferReader::new(value);
                    Ok(AccountInfo::deserialize(&mut stream).ok().map(|info| (account, info)))
                }) {
                    match step {
                        Ok(Some((account, info))) => {
                            if !f(account, info) {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }

    pub fn for_each<F>(&self, start: Account, mut f: F)
    where
        F: FnMut(Account, AccountInfo),
    {
        let start_key = self.make_key(&start);
        let start_shard = get_shard_index(&start_key);

        for i in start_shard..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let seek_key = if i == start_shard {
                    start_key
                } else {
                    let mut k = [0u8; KEY_SIZE];
                    k[0] = Self::TABLE_ID;
                    k[1] = (i as u8) << 4;
                    k
                };
                let mut it = db.iter(seek_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let account = Account::from_bytes(key[1..33].try_into().unwrap());
                    let mut stream = BufferReader::new(value);
                    Ok(AccountInfo::deserialize(&mut stream).ok().map(|info| (account, info)))
                }) {
                    match step {
                        Ok(Some((account, info))) => f(account, info),
                        Ok(None) => {}
                        Err(()) => break,
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
