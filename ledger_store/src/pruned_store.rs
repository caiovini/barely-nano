use core::sync::atomic::{AtomicBool, Ordering};
use rsnano_core::BlockHash;
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

static HAS_PRUNED: AtomicBool = AtomicBool::new(false);

pub struct BtreePrunedStore {}

impl BtreePrunedStore {
    pub const TABLE_ID: u8 = 5;

    pub fn new() -> Self {
        Self {}
    }

    pub fn set_has_pruned(&self, has_pruned: bool) {
        HAS_PRUNED.store(has_pruned, Ordering::Relaxed);
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, hash: &BlockHash) {
        HAS_PRUNED.store(true, Ordering::Relaxed);
        let key = self.make_key(hash);
        let value = &[]; // Just mark existence
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    #[inline]
    pub fn exists(&self, hash: &BlockHash) -> bool {
        if !HAS_PRUNED.load(Ordering::Relaxed) {
            return false;
        }
        let key = self.make_key(hash);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.contains_key(&key)
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

    pub fn count(&self, _tx: &dyn core::any::Any) -> u64 {
        let mut count = 0;
        for shard in SHARDS.iter() {
            let mut lock = shard.lock();
            if let Some(db) = lock.as_mut() {
                count += db.count_prefixed(Self::TABLE_ID) as u64;
            }
        }
        count
    }
}
