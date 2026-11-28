use rsnano_core::{Account, BlockHash, PendingKey, PendingInfo, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

const HOT_PENDING_WAYS: usize = 4;
const HOT_PENDING_SETS: usize = 128; // 64 shards * 128 sets * 4 ways = 32,768 hot pending entries (4-way in .bss)

const _: () = assert!(!core::mem::needs_drop::<PendingInfo>());

#[repr(C)]
struct HotPendingSet {
    gens: [u32; HOT_PENDING_WAYS],
    tags: [u32; HOT_PENDING_WAYS],
    keys: [PendingKey; HOT_PENDING_WAYS],
    infos: [core::mem::MaybeUninit<PendingInfo>; HOT_PENDING_WAYS],
}

impl HotPendingSet {
    const EMPTY: Self = Self {
        gens: [0; HOT_PENDING_WAYS],
        tags: [0; HOT_PENDING_WAYS],
        keys: [PendingKey {
            receiving_account: Account::zero(),
            send_block_hash: BlockHash::zero(),
        }; HOT_PENDING_WAYS],
        infos: [const { core::mem::MaybeUninit::zeroed() }; HOT_PENDING_WAYS],
    };
}

static HOT_PENDING: [spin::Mutex<[HotPendingSet; HOT_PENDING_SETS]>; SHARD_COUNT] =
    [const { spin::Mutex::new([const { HotPendingSet::EMPTY }; HOT_PENDING_SETS]) }; SHARD_COUNT];

#[inline]
fn hot_pending_set(key: &PendingKey) -> (usize, usize, usize, u32) {
    let acc_b = key.receiving_account.as_bytes();
    let hash_b = key.send_block_hash.as_bytes();
    let shard = (acc_b[0] >> 2) as usize;
    let w = u32::from_ne_bytes([acc_b[1], acc_b[2], acc_b[3], acc_b[4]])
        ^ u32::from_ne_bytes([hash_b[0], hash_b[1], hash_b[2], hash_b[3]]).rotate_left(13);
    let h = w.wrapping_mul(0x9E37_79B9);
    let set_idx = (h >> 25) as usize & (HOT_PENDING_SETS - 1);
    let victim_way = ((h >> 23) as usize) & (HOT_PENDING_WAYS - 1);
    (shard, set_idx, victim_way, h)
}

#[inline]
fn hot_pending_prefetch(key: &PendingKey) {
    let (shard, set_idx, _, _) = hot_pending_set(key);
    // SAFETY: `set_idx < HOT_PENDING_SETS`; `prefetch_read` issues a non-faulting Zicbop
    // `prefetch.r` cache hint for the 32-byte `(gens, tags)` header of `HotPendingSet[set_idx]`.
    let ptr = unsafe {
        (&HOT_PENDING[shard] as *const _ as *const HotPendingSet).add(set_idx) as *const u8
    };
    kernel_common::storage::prefetch_read(ptr);
}

#[inline]
fn hot_pending_get(key: &PendingKey) -> Option<PendingInfo> {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, set_idx, _, tag) = hot_pending_set(key);
    let guard = HOT_PENDING[shard].lock();
    let set = &guard[set_idx];
    for way in 0..HOT_PENDING_WAYS {
        if set.gens[way] == gen && set.tags[way] == tag && set.keys[way] == *key {
            // SAFETY: `set.infos[way]` was initialized with a valid `PendingInfo` before `set.gens[way]` was set to `gen > 0`.
            return Some(unsafe { set.infos[way].assume_init_ref().clone() });
        }
    }
    None
}

#[inline]
fn hot_pending_exists(key: &PendingKey) -> bool {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, set_idx, _, tag) = hot_pending_set(key);
    let guard = HOT_PENDING[shard].lock();
    let set = &guard[set_idx];
    for way in 0..HOT_PENDING_WAYS {
        if set.gens[way] == gen && set.tags[way] == tag && set.keys[way] == *key {
            return true;
        }
    }
    false
}

#[inline]
fn hot_pending_put_in_shard(
    guard: &mut [HotPendingSet; HOT_PENDING_SETS],
    gen: u32,
    key: &PendingKey,
    info: &PendingInfo,
) {
    let (_, set_idx, victim_way, tag) = hot_pending_set(key);
    let set = &mut guard[set_idx];
    let mut target_way = victim_way;
    for way in 0..HOT_PENDING_WAYS {
        if set.gens[way] == gen && set.tags[way] == tag && set.keys[way] == *key {
            target_way = way;
            break;
        }
        if set.gens[way] != gen {
            target_way = way;
        }
    }
    set.gens[target_way] = gen;
    set.tags[target_way] = tag;
    set.keys[target_way] = key.clone();
    set.infos[target_way] = core::mem::MaybeUninit::new(info.clone());
}

#[inline]
fn hot_pending_del_in_shard(
    guard: &mut [HotPendingSet; HOT_PENDING_SETS],
    key: &PendingKey,
) {
    let (_, set_idx, _, tag) = hot_pending_set(key);
    let set = &mut guard[set_idx];
    for way in 0..HOT_PENDING_WAYS {
        if set.tags[way] == tag && set.keys[way] == *key {
            set.gens[way] = 0;
        }
    }
}

#[inline]
fn hot_pending_put(key: &PendingKey, info: &PendingInfo) {
    let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
    let (shard, _, _, _) = hot_pending_set(key);
    let mut guard = HOT_PENDING[shard].lock();
    hot_pending_put_in_shard(&mut guard, gen, key, info);
}

#[inline]
fn hot_pending_del(key: &PendingKey) {
    let (shard, _, _, _) = hot_pending_set(key);
    let mut guard = HOT_PENDING[shard].lock();
    hot_pending_del_in_shard(&mut guard, key);
}

pub struct BtreePendingStore {}

impl BtreePendingStore {
    pub const TABLE_ID: u8 = 7;

    pub fn new() -> Self {
        Self {}
    }

    #[inline]
    pub fn prefetch_hot(&self, key: &PendingKey) {
        if !cfg!(test) {
            hot_pending_prefetch(key);
        }
    }

    #[inline]
    pub fn get_hot(&self, key: &PendingKey) -> Option<PendingInfo> {
        if !cfg!(test) {
            hot_pending_get(key)
        } else {
            None
        }
    }

    #[inline]
    pub fn is_hot(&self, key: &PendingKey) -> bool {
        !cfg!(test) && hot_pending_exists(key)
    }

    pub fn put(&self, key: &PendingKey, info: &PendingInfo) {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let mut val_bytes = [0u8; 64];
        let mut adapter = MutStreamAdapter::new(&mut val_bytes);
        info.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            btree.insert(key_bytes, &val_bytes[0..len]);
            drop(lock);
            if !cfg!(test) {
                hot_pending_put(key, info);
            }
        }
    }

    pub fn get(&self, key: &PendingKey) -> Option<PendingInfo> {
        if !cfg!(test) {
            if let Some(cached) = hot_pending_get(key) {
                return Some(cached);
            }
        }
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        let btree = lock.as_mut()?;
        let found = btree.get_with(&key_bytes, |val_bytes| {
            let mut reader = BufferReader::new(val_bytes);
            PendingInfo::deserialize(&mut reader).ok()
        }).flatten();
        drop(lock);
        if !cfg!(test) {
            if let Some(ref info) = found {
                hot_pending_put(key, info);
            }
        }
        found
    }

    pub fn del(&self, key: &PendingKey) {
        if !cfg!(test) {
            hot_pending_del(key);
        }
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            btree.delete(key_bytes);
        }
    }

    /// Applies a key-sorted slice of `(PendingKey, Option<PendingInfo>)` (`Some` = insert, `None` = delete),
    /// acquiring each shard B-Tree lock and `HOT_PENDING` shard lock at most once per shard.
    pub fn flush_batch(&self, items: &[(PendingKey, Option<PendingInfo>)]) {
        if items.is_empty() {
            return;
        }
        let gen = kernel_common::storage::STORAGE_GEN.load(Ordering::Acquire);
        let mut val_bytes = [0u8; 64];
        let mut i = 0;
        let n = items.len();
        while i < n {
            let shard_idx = (items[i].0.receiving_account.as_bytes()[0] >> 2) as usize;
            let mut j = i + 1;
            while j < n
                && ((items[j].0.receiving_account.as_bytes()[0] >> 2) as usize) == shard_idx
            {
                j += 1;
            }
            let shard_items = &items[i..j];
            let mut has_db = false;
            {
                let mut lock = SHARDS[shard_idx].lock();
                if let Some(btree) = lock.as_mut() {
                    has_db = true;
                    for (key, op) in shard_items {
                        let mut key_bytes = [0u8; KEY_SIZE];
                        key_bytes[0] = Self::TABLE_ID;
                        key_bytes[1..65].copy_from_slice(&key.to_bytes());
                        match op {
                            Some(info) => {
                                let mut adapter = MutStreamAdapter::new(&mut val_bytes);
                                info.serialize(&mut adapter);
                                let len = adapter.bytes_written();
                                btree.insert(key_bytes, &val_bytes[0..len]);
                            }
                            None => {
                                btree.delete(key_bytes);
                            }
                        }
                    }
                }
            }
            if has_db && !cfg!(test) {
                let mut hot_guard = HOT_PENDING[shard_idx].lock();
                for (key, op) in shard_items {
                    match op {
                        Some(info) => hot_pending_put_in_shard(&mut hot_guard, gen, key, info),
                        None => hot_pending_del_in_shard(&mut hot_guard, key),
                    }
                }
            }
            i = j;
        }
    }

    /// Returns true if at least one pending entry exists for `account`.
    /// Stops at the first match instead of collecting every entry (O(log n), no allocation
    /// per entry), unlike `iter_for_account`.
    pub fn exists_for_account(&self, account: &rsnano_core::Account) -> bool {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        start_key[1..33].copy_from_slice(account.as_bytes());

        let shard_idx = get_shard_index(&start_key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            if let Some(k) = btree.first_key_from(&start_key) {
                return k[0] == Self::TABLE_ID && &k[1..33] == account.as_bytes();
            }
        }
        false
    }

    pub fn iter_for_account(&self, account: &rsnano_core::Account) -> Vec<(PendingKey, PendingInfo)> {
        let mut results = Vec::new();
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        start_key[1..33].copy_from_slice(account.as_bytes());

        let shard_idx = get_shard_index(&start_key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            let mut it = btree.iter(start_key);
            while let Some(step) = it.next_kv_with(|k, v| {
                if k[0] != Self::TABLE_ID || &k[1..33] != account.as_bytes() {
                    return Err(());
                }
                let mut reader = BufferReader::new(v);
                if let Ok(info) = PendingInfo::deserialize(&mut reader) {
                    let mut key_reader = BufferReader::new(&k[1..]);
                    if let Ok(key) = PendingKey::deserialize(&mut key_reader) {
                        return Ok(Some((key, info)));
                    }
                }
                Ok(None)
            }) {
                match step {
                    Ok(Some(item)) => results.push(item),
                    Ok(None) => {}
                    Err(()) => break,
                }
            }
        }
        results
    }

    pub fn for_each_from_while<F>(&self, start: PendingKey, mut f: F)
    where
        F: FnMut(PendingKey, PendingInfo) -> bool,
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        start_key[1..65].copy_from_slice(&start.to_bytes());

        let start_shard = get_shard_index(&start_key);

        for i in start_shard..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(btree) = lock.as_mut() {
                let seek_key = if i == start_shard {
                    start_key
                } else {
                    let mut k = [0u8; KEY_SIZE];
                    k[0] = Self::TABLE_ID;
                    k[1] = (i as u8) << 2;
                    k
                };
                let mut it = btree.iter(seek_key);
                while let Some(step) = it.next_kv_with(|k, v| {
                    if k[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut reader = BufferReader::new(v);
                    if let Ok(info) = PendingInfo::deserialize(&mut reader) {
                        let mut key_reader = BufferReader::new(&k[1..]);
                        if let Ok(key) = PendingKey::deserialize(&mut key_reader) {
                            return Ok(Some((key, info)));
                        }
                    }
                    Ok(None)
                }) {
                    match step {
                        Ok(Some((key, info))) => {
                            if !f(key, info) {
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

    pub fn for_each_while<F>(&self, f: F)
    where
        F: FnMut(PendingKey, PendingInfo) -> bool,
    {
        self.for_each_from_while(PendingKey::new(Account::zero(), BlockHash::zero()), f)
    }

    pub fn for_each_from<F>(&self, start: PendingKey, mut f: F)
    where
        F: FnMut(PendingKey, PendingInfo),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        start_key[1..65].copy_from_slice(&start.to_bytes());

        let start_shard = get_shard_index(&start_key);

        for i in start_shard..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(btree) = lock.as_mut() {
                let seek_key = if i == start_shard {
                    start_key
                } else {
                    let mut k = [0u8; KEY_SIZE];
                    k[0] = Self::TABLE_ID;
                    k[1] = (i as u8) << 2;
                    k
                };
                let mut it = btree.iter(seek_key);
                while let Some(step) = it.next_kv_with(|k, v| {
                    if k[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut reader = BufferReader::new(v);
                    if let Ok(info) = PendingInfo::deserialize(&mut reader) {
                        let mut key_reader = BufferReader::new(&k[1..]);
                        if let Ok(key) = PendingKey::deserialize(&mut key_reader) {
                            return Ok(Some((key, info)));
                        }
                    }
                    Ok(None)
                }) {
                    match step {
                        Ok(Some((key, info))) => f(key, info),
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }

    pub fn iter_range(&self, start: PendingKey) -> Vec<(PendingKey, PendingInfo)> {
        let mut results: Vec<(PendingKey, PendingInfo)> = Vec::with_capacity(32);
        let mut visited = 0usize;
        self.for_each_from_while(start, |key, info| {
            visited += 1;
            if results.last().map(|(k, _)| k.receiving_account) != Some(key.receiving_account) {
                results.push((key, info));
            }
            results.len() < 32 && visited < 256
        });
        results
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(PendingKey, PendingInfo),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(btree) = lock.as_mut() {
                let mut it = btree.iter(start_key);
                while let Some(step) = it.next_kv_with(|k, v| {
                    if k[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let mut reader = BufferReader::new(v);
                    if let Ok(info) = PendingInfo::deserialize(&mut reader) {
                        let mut key_reader = BufferReader::new(&k[1..]);
                        if let Ok(key) = PendingKey::deserialize(&mut key_reader) {
                            return Ok(Some((key, info)));
                        }
                    }
                    Ok(None)
                }) {
                    match step {
                        Ok(Some((key, info))) => f(key, info),
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }

    pub fn iter(&self) -> Vec<(PendingKey, PendingInfo)> {
        let mut results = Vec::new();
        self.for_each(|key, info| results.push((key, info)));
        results
    }
}
