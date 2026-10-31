use rsnano_core::{PublicKey, Amount, utils::{Serialize, Deserialize, BufferReader, MutStreamAdapter}};
use kernel_common::storage::{SHARDS, get_shard_index};
use kernel_common::storage::btree::KEY_SIZE;

pub struct BtreeRepWeightStore {}

impl BtreeRepWeightStore {
    pub const TABLE_ID: u8 = 12;

    pub fn new() -> Self {
        Self {}
    }

    fn make_key(&self, pub_key: &PublicKey) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(pub_key.as_bytes());
        key
    }

    pub fn put(&self, pub_key: PublicKey, amount: Amount) {
        let key = self.make_key(&pub_key);
        let mut buffer = [0u8; 512];
        let mut adapter = MutStreamAdapter::new(&mut buffer);
        amount.serialize(&mut adapter);
        let len = adapter.bytes_written();
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buffer[0..len]);
        }
    }

    pub fn get(&self, _tx: &dyn core::any::Any, pub_key: &PublicKey) -> Option<Amount> {
        let key = self.make_key(pub_key);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            if let Some(res) = db.get_with(&key, |value| {
                let mut reader = BufferReader::new(value);
                Amount::deserialize(&mut reader).ok()
            }) {
                return res;
            }
        }
        None
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(PublicKey, Amount),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;
        let mut count = 0usize;
        for i in 0..kernel_common::storage::SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some(step) = it.next_kv_with(|key, value| {
                    if key[0] != Self::TABLE_ID {
                        return Err(());
                    }
                    let rep = PublicKey::from_bytes(key[1..33].try_into().unwrap());
                    let mut reader = BufferReader::new(value);
                    Ok(Amount::deserialize(&mut reader)
                        .ok()
                        .map(|amount| (rep, amount)))
                }) {
                    match step {
                        Ok(Some((rep, amount))) => {
                            f(rep, amount);
                            count += 1;
                            if count % 1000 == 0 {
                                kernel_common::watchdog::kick();
                            }
                        }
                        Ok(None) => {}
                        Err(()) => break,
                    }
                }
            }
        }
    }
}
