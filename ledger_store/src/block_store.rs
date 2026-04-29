use rsnano_core::{
    utils::{BufferReader, Deserialize, MutStreamAdapter, Serialize},
    BlockHash, SavedBlock,
};
use kernel_common::storage::DB;
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeBlockStore {}

impl BtreeBlockStore {
    pub const TABLE_ID: u8 = 2;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, hash: &BlockHash) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(hash.as_bytes());
        key
    }

    pub fn put(&self, block: &SavedBlock) {
        let key = self.make_key(&block.hash());
        let mut value = [0u8; 256];
        let mut adapter = MutStreamAdapter::new(&mut value);
        block.serialize(&mut adapter);
        
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
                return SavedBlock::deserialize(&mut stream).ok();
            }
        }
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
}
