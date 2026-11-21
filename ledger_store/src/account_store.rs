use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, AccountInfo,
};

use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use crate::LedgerCache;

/// Maximum unconfirmed accounts tracked per shard in RAM (`16 * 4,096 = 65,536` accounts max, `< 2.5 MB`).
const MAX_UNCONFIRMED_PER_SHARD: usize = 4096;
/// Maximum recently confirmed accounts buffered for `on_up_to_date` observers (`BoundedBacklog`).
const MAX_RECENTLY_CONFIRMED: usize = 4096;

/// Sharded in-RAM index of accounts with `block_count > confirmation_height` (`Account -> block_count`).
/// Eliminates full-table `BacklogScan` sweeps across 35M cold accounts on NVMe.
static UNCONFIRMED_ACCOUNTS: [spin::Mutex<BTreeMap<Account, u64>>; SHARD_COUNT] =
    [const { spin::Mutex::new(BTreeMap::new()) }; SHARD_COUNT];
static UNCONFIRMED_COUNT: AtomicUsize = AtomicUsize::new(0);

static RECENTLY_CONFIRMED: spin::Mutex<Vec<Account>> = spin::Mutex::new(Vec::new());
static RECENTLY_CONFIRMED_LEN: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn account_shard(account: &Account) -> usize {
    (account.as_bytes()[0] >> 4) as usize
}

#[inline]
fn push_recently_confirmed(account: Account) {
    let mut guard = RECENTLY_CONFIRMED.lock();
    if guard.len() < MAX_RECENTLY_CONFIRMED {
        guard.push(account);
        RECENTLY_CONFIRMED_LEN.store(guard.len(), Ordering::Relaxed);
    }
}

/// Records that `account` was written with `block_count` in `TABLE_ACCOUNTS`.
#[inline]
pub fn unconfirmed_account_on_account_put(account: &Account, block_count: u64) {
    if block_count == 0 {
        return;
    }
    if !cfg!(test) {
        if let Some(conf) = crate::confirmation_height_store::hot_conf_height_get(account) {
            if conf.height >= block_count {
                let shard = account_shard(account);
                let mut map = UNCONFIRMED_ACCOUNTS[shard].lock();
                if map.remove(account).is_some() {
                    UNCONFIRMED_COUNT.fetch_sub(1, Ordering::Relaxed);
                }
                return;
            }
        }
    }
    let shard = account_shard(account);
    let mut map = UNCONFIRMED_ACCOUNTS[shard].lock();
    if let Some(existing) = map.get_mut(account) {
        *existing = block_count;
    } else if map.len() < MAX_UNCONFIRMED_PER_SHARD {
        map.insert(*account, block_count);
        UNCONFIRMED_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Records that `account` was deleted from `TABLE_ACCOUNTS`.
#[inline]
pub fn unconfirmed_account_on_account_del(account: &Account) {
    let shard = account_shard(account);
    let mut map = UNCONFIRMED_ACCOUNTS[shard].lock();
    if map.remove(account).is_some() {
        UNCONFIRMED_COUNT.fetch_sub(1, Ordering::Relaxed);
        drop(map);
        push_recently_confirmed(*account);
    }
}

/// Records that `account`'s confirmation height advanced to `conf_height`.
/// If `conf_height >= tracked_block_count`, removes `account` in $O(\log U)$ from the shard map.
#[inline]
pub fn unconfirmed_account_on_conf_put(account: &Account, conf_height: u64) {
    let shard = account_shard(account);
    let mut map = UNCONFIRMED_ACCOUNTS[shard].lock();
    if let Some(&block_count) = map.get(account) {
        if conf_height >= block_count {
            map.remove(account);
            UNCONFIRMED_COUNT.fetch_sub(1, Ordering::Relaxed);
            drop(map);
            push_recently_confirmed(*account);
        }
    }
}

/// Records that `account`'s confirmation height entry was deleted.
#[inline]
pub fn unconfirmed_account_on_conf_del(account: &Account) {
    if let Some(info) = hot_account_get(account) {
        if info.block_count > 0 {
            unconfirmed_account_on_account_put(account, info.block_count);
        }
    }
}

/// Seeds an unconfirmed account discovered during the boot-time `LedgerRecount` merge-join.
#[inline]
pub fn unconfirmed_account_seed(account: &Account, block_count: u64) {
    unconfirmed_account_on_account_put(account, block_count);
}

/// Removes `account` from `UNCONFIRMED_ACCOUNTS` if `BacklogScan` verified on disk that
/// `current_conf_height >= current_block_count` (or the account no longer exists).
#[inline]
pub fn unconfirmed_account_resolve_clean(account: &Account, current_conf_height: u64) {
    let shard = account_shard(account);
    let mut map = UNCONFIRMED_ACCOUNTS[shard].lock();
    if let Some(&tracked_bc) = map.get(account) {
        if current_conf_height == u64::MAX || current_conf_height >= tracked_bc {
            map.remove(account);
            UNCONFIRMED_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Clears all tracked unconfirmed and recently confirmed accounts (used on DB wipe / test reset).
pub fn unconfirmed_accounts_clear() {
    for s in 0..SHARD_COUNT {
        UNCONFIRMED_ACCOUNTS[s].lock().clear();
    }
    UNCONFIRMED_COUNT.store(0, Ordering::Relaxed);
    RECENTLY_CONFIRMED.lock().clear();
    RECENTLY_CONFIRMED_LEN.store(0, Ordering::Relaxed);
}

/// Lock-free $O(1)$ count of tracked unconfirmed accounts.
#[inline(always)]
pub fn unconfirmed_accounts_count() -> usize {
    UNCONFIRMED_COUNT.load(Ordering::Relaxed)
}

/// Lock-free $O(1)$ check whether `BacklogScan` has any unconfirmed or newly cemented accounts to process.
#[inline(always)]
pub fn unconfirmed_accounts_has_work() -> bool {
    UNCONFIRMED_COUNT.load(Ordering::Relaxed) > 0
        || RECENTLY_CONFIRMED_LEN.load(Ordering::Relaxed) > 0
}

/// Drains up to `max_count` recently cemented accounts for `on_up_to_date` notifications.
pub fn drain_recently_confirmed(max_count: usize) -> Vec<Account> {
    if RECENTLY_CONFIRMED_LEN.load(Ordering::Relaxed) == 0 {
        return Vec::new();
    }
    let mut guard = RECENTLY_CONFIRMED.lock();
    let take = guard.len().min(max_count);
    let out: Vec<Account> = guard.drain(..take).collect();
    RECENTLY_CONFIRMED_LEN.store(guard.len(), Ordering::Relaxed);
    out
}

/// Fetches up to `max_count` tracked unconfirmed accounts starting at `>= start` across the 16 in-RAM shard maps.
/// Returns `(accounts, last_account, reached_end)`.
pub fn unconfirmed_accounts_next_batch(
    start: Account,
    max_count: usize,
) -> (Vec<Account>, Option<Account>, bool) {
    let mut out = Vec::new();
    if UNCONFIRMED_COUNT.load(Ordering::Relaxed) == 0 || max_count == 0 {
        return (out, None, true);
    }
    let start_shard = account_shard(&start);
    let mut reached_end = true;
    for s in start_shard..SHARD_COUNT {
        let map = UNCONFIRMED_ACCOUNTS[s].lock();
        if map.is_empty() {
            continue;
        }
        let iter = if s == start_shard {
            map.range(start..)
        } else {
            map.range(Account::zero()..)
        };
        for (&acc, _) in iter {
            out.push(acc);
            if out.len() >= max_count {
                reached_end = false;
                break;
            }
        }
        if !reached_end {
            break;
        }
    }
    let last = out.last().copied();
    (out, last, reached_end)
}

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

const HOT_ACCOUNT_SLOTS: usize = 32; // 16 shards * 32 slots = 512 hot accounts

#[derive(Clone)]
struct HotAccountEntry {
    gen: u32,
    account: Account,
    info: AccountInfo,
}

static HOT_ACCOUNTS: [spin::Mutex<[Option<HotAccountEntry>; HOT_ACCOUNT_SLOTS]>; SHARD_COUNT] =
    [const { spin::Mutex::new([const { None }; HOT_ACCOUNT_SLOTS]) }; SHARD_COUNT];

#[inline]
fn hot_account_slot(account: &Account) -> (usize, usize) {
    let b = account.as_bytes();
    let shard = (b[0] >> 4) as usize;
    let slot = (u16::from_ne_bytes([b[1], b[2]]) as usize) & (HOT_ACCOUNT_SLOTS - 1);
    (shard, slot)
}

#[inline]
fn hot_account_get(account: &Account) -> Option<AccountInfo> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_account_slot(account);
    let guard = HOT_ACCOUNTS[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.gen == gen && entry.account == *account {
            return Some(entry.info.clone());
        }
    }
    None
}

#[inline]
fn hot_account_put(account: &Account, info: &AccountInfo) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_account_slot(account);
    HOT_ACCOUNTS[shard].lock()[slot] = Some(HotAccountEntry {
        gen,
        account: *account,
        info: info.clone(),
    });
}

#[inline]
fn hot_account_del(account: &Account) {
    let (shard, slot) = hot_account_slot(account);
    let mut guard = HOT_ACCOUNTS[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.account == *account {
            guard[slot] = None;
        }
    }
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
            drop(lock);
            if !cfg!(test) {
                hot_account_put(account, info);
            }
            unconfirmed_account_on_account_put(account, info.block_count);
        }
    }

    pub fn get(&self, account: &Account) -> Option<AccountInfo> {
        if !cfg!(test)
            && ACCOUNT_BLOOM_READY.load(Ordering::Acquire)
            && !account_bloom_may_contain(account.as_bytes())
        {
            return None;
        }
        if !cfg!(test) {
            if let Some(cached) = hot_account_get(account) {
                return Some(cached);
            }
        }
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(Some(info)) = db.get_with(&key, |value| {
                let mut stream = BufferReader::new(value);
                AccountInfo::deserialize(&mut stream).ok()
            }) {
                drop(lock);
                if !cfg!(test) {
                    hot_account_put(account, &info);
                }
                tracing::debug!("AccountStore: found account {}, head={}", account.encode_account(), info.head);
                return Some(info);
            }
        }
        // tracing::debug!("AccountStore: NOT found account {}", account.encode_account());
        None
    }

    pub fn clear_bloom(&self) {
        kernel_common::storage::STORAGE_GEN.fetch_add(1, Ordering::Release);
        unconfirmed_accounts_clear();
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
        hot_account_del(account);
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
            drop(lock);
            unconfirmed_account_on_account_del(account);
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
