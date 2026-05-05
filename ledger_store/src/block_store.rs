use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    BlockHash, SavedBlock,
};
use kernel_common::storage::DB;
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
        let mut value = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut value);
        block.serialize(&mut adapter);
        
        tracing::debug!("BlockStore: putting block {}", block.hash());
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn get(&self, hash: &BlockHash) -> Option<SavedBlock> {
        let key = self.make_key(hash);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            if let Some(value) = db.search(&key) {
                let mut stream = BufferReader::new(&value);
                if let Ok(block) = SavedBlock::deserialize(&mut stream) {
                    tracing::debug!("BlockStore: found block {}", hash);
                    return Some(block);
                }
            }
        }
        tracing::debug!("BlockStore: NOT found block {}", hash);
        None
    }

    pub fn exists(&self, hash: &BlockHash) -> bool {
        let key = self.make_key(hash);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn del(&self, hash: &BlockHash) {
        let key = self.make_key(hash);
        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn iter(&self) -> alloc::vec::Vec<SavedBlock> {
        let mut results = alloc::vec::Vec::new();
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        let mut lock = DB.lock();
        if let Some(db) = lock.as_mut() {
            let mut it = db.iter(start_key);
            while let Some((key, value)) = it.next_kv() {
                if key[0] != Self::TABLE_ID {
                    break;
                }
                let mut stream = BufferReader::new(&value);
                if let Ok(block) = SavedBlock::deserialize(&mut stream) {
                    results.push(block);
                }
            }
        }
        results
    }
}
