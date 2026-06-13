use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    BlockHash, SavedBlock,
};
use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use alloc::sync::Arc;
use crate::LedgerCache;

pub struct BtreeBlockStore {
    cache: Arc<LedgerCache>,
}

impl BtreeBlockStore {
    pub const TABLE_ID: u8 = 2;

    pub fn new(cache: Arc<LedgerCache>) -> Self {
        Self { cache }
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, block: &SavedBlock) {
        let key = self.make_key(&block.hash());
        let mut buffer = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        block.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key);
        tracing::debug!("BlockStore: putting block {} into shard {}", block.hash(), shard_idx);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buffer[0..len]);
        }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<SavedBlock> {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut stream = BufferReader::new(&value);
                if let Ok(block) = SavedBlock::deserialize(&mut stream) {
                    tracing::debug!("BlockStore: found block {}", hash);
                    return Some(block);
                }
            }
        }
        tracing::debug!("BlockStore: NOT found block {} in shard {}", hash, shard_idx);
        None
    }

    pub fn exists(&self, hash: &BlockHash) -> bool {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn del(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn count(&self) -> usize {
        let mut count = 0;
        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                count += db.count_prefixed(Self::TABLE_ID);
            }
        }
        count
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(SavedBlock),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some((key, value)) = it.next_kv() {
                    if key[0] != Self::TABLE_ID {
                        break;
                    }
                    let mut stream = BufferReader::new(&value);
                    if let Ok(block) = SavedBlock::deserialize(&mut stream) {
                        f(block);
                    }
                }
            }
        }
    }

    pub fn iter(&self) -> alloc::vec::Vec<SavedBlock> {
        let mut results = alloc::vec::Vec::new();
        self.for_each(|block| results.push(block));
        results
    }
}
