use rsnano_core::{Account, BlockHash, PendingKey, PendingInfo, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::vec::Vec;

pub struct BtreePendingStore {}

impl BtreePendingStore {
    pub const TABLE_ID: u8 = 7;

    pub fn new() -> Self {
        Self {}
    }

    pub fn put(&self, key: &PendingKey, info: &PendingInfo) {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let mut val_bytes = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut val_bytes);
        info.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            btree.insert(key_bytes, &val_bytes[0..len]);
        }
    }

    pub fn get(&self, key: &PendingKey) -> Option<PendingInfo> {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        let btree = lock.as_mut()?;
        btree.get_with(&key_bytes, |val_bytes| {
            let mut reader = BufferReader::new(val_bytes);
            PendingInfo::deserialize(&mut reader).ok()
        })?
    }

    pub fn del(&self, key: &PendingKey) {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes[0] = Self::TABLE_ID;
        key_bytes[1..65].copy_from_slice(&key.to_bytes());

        let shard_idx = get_shard_index(&key_bytes);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(btree) = lock.as_mut() {
            btree.delete(key_bytes);
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
                    k[1] = (i as u8) << 4;
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
                    k[1] = (i as u8) << 4;
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
