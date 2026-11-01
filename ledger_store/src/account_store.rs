use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, AccountInfo,
};

use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::LedgerCache;

const ACCOUNT_BLOOM_BUCKETS: usize = 1 << 13; // 8,192 4-word buckets = 32,768 u64s = 256 KB
const ACCOUNT_BLOOM_WORDS: usize = ACCOUNT_BLOOM_BUCKETS * 4;
const ACCOUNT_BLOOM_MAX_ACCOUNTS: u64 = 250_000;

static ACCOUNT_BLOOM: [AtomicU64; ACCOUNT_BLOOM_WORDS] =
    [const { AtomicU64::new(0) }; ACCOUNT_BLOOM_WORDS];
static ACCOUNT_BLOOM_READY: AtomicBool = AtomicBool::new(false);

#[inline]
fn account_bloom_probes(bytes: &[u8; 32]) -> (usize, u64, u64, u64, u64) {
    let w0 = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let w1 = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let base = ((w0 as usize) & (ACCOUNT_BLOOM_BUCKETS - 1)) * 4;
    let m0 = (1u64 << (w1 & 63)) | (1u64 << ((w1 >> 6) & 63));
    let m1 = (1u64 << ((w1 >> 12) & 63)) | (1u64 << ((w1 >> 18) & 63));
    let m2 = 1u64 << ((w1 >> 24) & 63);
    let m3 = 1u64 << ((w1 >> 30) & 63);
    (base, m0, m1, m2, m3)
}

#[inline]
fn account_bloom_insert(bytes: &[u8; 32]) {
    let (base, m0, m1, m2, m3) = account_bloom_probes(bytes);
    ACCOUNT_BLOOM[base].fetch_or(m0, Ordering::Relaxed);
    ACCOUNT_BLOOM[base + 1].fetch_or(m1, Ordering::Relaxed);
    ACCOUNT_BLOOM[base + 2].fetch_or(m2, Ordering::Relaxed);
    ACCOUNT_BLOOM[base + 3].fetch_or(m3, Ordering::Relaxed);
}

#[inline]
fn account_bloom_may_contain(bytes: &[u8; 32]) -> bool {
    let (base, m0, m1, m2, m3) = account_bloom_probes(bytes);
    (ACCOUNT_BLOOM[base].load(Ordering::Relaxed) & m0 == m0)
        && (ACCOUNT_BLOOM[base + 1].load(Ordering::Relaxed) & m1 == m1)
        && (ACCOUNT_BLOOM[base + 2].load(Ordering::Relaxed) & m2 == m2)
        && (ACCOUNT_BLOOM[base + 3].load(Ordering::Relaxed) & m3 == m3)
}

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
        account_bloom_insert(account.as_bytes());
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
        if !cfg!(test)
            && ACCOUNT_BLOOM_READY.load(Ordering::Acquire)
            && !account_bloom_may_contain(account.as_bytes())
        {
            return None;
        }
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

    pub fn clear_bloom(&self) {
        ACCOUNT_BLOOM_READY.store(false, Ordering::Release);
        for w in ACCOUNT_BLOOM.iter() {
            w.store(0, Ordering::Relaxed);
        }
    }

    pub fn disable_bloom(&self) {
        ACCOUNT_BLOOM_READY.store(false, Ordering::Release);
    }

    pub fn populate_bloom(&self) {
        self.clear_bloom();
        let mut account_count = 0u64;
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(step) = it.next_kv_with(|key, _value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let acc_bytes: [u8; 32] = key[1..33].try_into().unwrap();
                    account_bloom_insert(&acc_bytes);
                    Ok(())
                }) {
                    match step {
                        Ok(()) => {
                            account_count += 1;
                            if account_count > ACCOUNT_BLOOM_MAX_ACCOUNTS {
                                return;
                            }
                            if account_count % 2000 == 0 {
                                kernel_common::watchdog::kick();
                            }
                        }
                        Err(()) => break,
                    }
                }
            }
        }
        ACCOUNT_BLOOM_READY.store(true, Ordering::Release);
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
        self.clear_bloom();
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
                    let acc_bytes: [u8; 32] = key[1..33].try_into().unwrap();
                    account_bloom_insert(&acc_bytes);
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
        if account_count <= ACCOUNT_BLOOM_MAX_ACCOUNTS {
            ACCOUNT_BLOOM_READY.store(true, Ordering::Release);
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
