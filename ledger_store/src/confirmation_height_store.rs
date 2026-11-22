use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    Account, ConfirmationHeightInfo,
};
use alloc::boxed::Box;
use core::sync::atomic::Ordering;
use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;

const HOT_CONF_SLOTS: usize = 32; // 64 shards * 32 slots = 2,048 hot confirmation heights (~128 KB)

#[derive(Clone)]
struct HotConfEntry {
    gen: u32,
    account: Account,
    info: ConfirmationHeightInfo,
}

static HOT_CONF_HEIGHT: [spin::Mutex<[Option<HotConfEntry>; HOT_CONF_SLOTS]>; SHARD_COUNT] =
    [const { spin::Mutex::new([const { None }; HOT_CONF_SLOTS]) }; SHARD_COUNT];

#[inline]
fn hot_conf_slot(account: &Account) -> (usize, usize) {
    let b = account.as_bytes();
    let shard = (b[0] >> 2) as usize;
    let slot = (u16::from_ne_bytes([b[1], b[2]]) as usize) & (HOT_CONF_SLOTS - 1);
    (shard, slot)
}

#[inline]
pub(crate) fn hot_conf_height_get(account: &Account) -> Option<ConfirmationHeightInfo> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_conf_slot(account);
    let guard = HOT_CONF_HEIGHT[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.gen == gen && entry.account == *account {
            return Some(entry.info.clone());
        }
    }
    None
}

#[inline]
pub(crate) fn hot_conf_height_put(account: &Account, info: &ConfirmationHeightInfo) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, slot) = hot_conf_slot(account);
    HOT_CONF_HEIGHT[shard].lock()[slot] = Some(HotConfEntry {
        gen,
        account: *account,
        info: info.clone(),
    });
}

#[inline]
pub(crate) fn hot_conf_height_del(account: &Account) {
    let (shard, slot) = hot_conf_slot(account);
    let mut guard = HOT_CONF_HEIGHT[shard].lock();
    if let Some(entry) = &guard[slot] {
        if entry.account == *account {
            guard[slot] = None;
        }
    }
}

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
                hot_conf_height_put(account, info);
            }
            crate::unconfirmed_account_on_conf_put(account, info.height);
        }
    }

    pub fn del(&self, account: &Account) {
        hot_conf_height_del(account);
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
            drop(lock);
            crate::unconfirmed_account_on_conf_del(account);
        }
    }

    pub fn get(&self, account: &Account) -> Option<ConfirmationHeightInfo> {
        if !cfg!(test) {
            if let Some(cached) = hot_conf_height_get(account) {
                return Some(cached);
            }
        }
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(Some(info)) = db.get_with(&key, |value| {
                let mut stream = BufferReader::new(value);
                ConfirmationHeightInfo::deserialize(&mut stream).ok()
            }) {
                drop(lock);
                if !cfg!(test) {
                    hot_conf_height_put(account, &info);
                }
                return Some(info);
            }
        }
        None
    }

    pub fn exists(&self, account: &Account) -> bool {
        let key = self.make_key(account);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.contains_key(&key)
        } else {
            false
        }
    }

    pub fn for_each_while<F>(&self, start: Account, mut f: F)
    where
        F: FnMut(Account, ConfirmationHeightInfo) -> bool,
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
                    k[1] = (i as u8) << 2;
                    k
                };
                let mut it = db.iter(seek_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let account = Account::from_bytes(key[1..33].try_into().unwrap());
                    let mut stream = BufferReader::new(value);
                    Ok(ConfirmationHeightInfo::deserialize(&mut stream)
                        .ok()
                        .map(|info| (account, info)))
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
        F: FnMut(Account, ConfirmationHeightInfo),
    {
        self.for_each_while(start, |account, info| {
            f(account, info);
            true
        });
    }

    pub fn iter(&self) -> alloc::vec::Vec<(Account, ConfirmationHeightInfo)> {
        let mut results = alloc::vec::Vec::new();
        self.for_each(Account::zero(), |acc, info| results.push((acc, info)));
        results
    }

    pub fn iter_range(
        &self,
        _tx: &(),
        range: core::ops::RangeFrom<Account>,
    ) -> Box<dyn Iterator<Item = (Account, ConfirmationHeightInfo)>> {
        Box::new(ConfirmationHeightRangeIterator::new(range.start))
    }

    pub fn sum_heights(&self) -> u64 {
        let mut total = 0;
        let mut scanned = 0usize;
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
                    let h = if value.len() >= 8 {
                        Some(u64::from_ne_bytes(value[0..8].try_into().unwrap()))
                    } else {
                        None
                    };
                    Ok(h)
                }) {
                    match step {
                        Ok(Some(h)) => {
                            total += h;
                            scanned += 1;
                            if scanned % 1000 == 0 {
                                kernel_common::watchdog::kick();
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
        total
    }
}

struct ConfirmationHeightRangeIterator {
    batch: alloc::collections::VecDeque<(Account, ConfirmationHeightInfo)>,
    next_start: Option<Account>,
}

impl ConfirmationHeightRangeIterator {
    const BATCH_SIZE: usize = 32;

    fn new(start: Account) -> Self {
        Self {
            batch: alloc::collections::VecDeque::with_capacity(Self::BATCH_SIZE),
            next_start: Some(start),
        }
    }
}

impl Iterator for ConfirmationHeightRangeIterator {
    type Item = (Account, ConfirmationHeightInfo);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.batch.pop_front() {
            return Some(item);
        }
        let start = self.next_start?;
        let store = BtreeConfirmationHeightStore::new();
        let mut last_acc = None;
        store.for_each_while(start, |acc, info| {
            self.batch.push_back((acc, info));
            last_acc = Some(acc);
            self.batch.len() < Self::BATCH_SIZE
        });
        if self.batch.len() < Self::BATCH_SIZE {
            self.next_start = None;
        } else if let Some(acc) = last_acc {
            self.next_start = acc.inc();
        } else {
            self.next_start = None;
        }
        self.batch.pop_front()
    }
}
