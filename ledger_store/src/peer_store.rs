use kernel_common::storage::{SHARDS, get_shard_index, SHARD_COUNT};
use kernel_common::storage::btree::KEY_SIZE;
use rsnano_core::utils::SocketAddrV6;
use rsnano_nullable_clock::SystemTime;
use alloc::vec::Vec;

pub struct BtreePeerStore {}
impl BtreePeerStore {
    pub const TABLE_ID: u8 = 6;
    pub fn new() -> Self { Self {} }

    fn make_key(&self, endpoint: &SocketAddrV6) -> [u8; kernel_common::storage::btree::KEY_SIZE] {
        let mut key = [0u8; kernel_common::storage::btree::KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..17].copy_from_slice(&endpoint.ip().octets());
        key[17..19].copy_from_slice(&endpoint.port().to_be_bytes());
        key
    }

    pub fn put(&self, endpoint: SocketAddrV6, _timestamp: SystemTime) {
        let key = self.make_key(&endpoint);
        let value = &[]; // Just mark existence
        
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, value);
        }
    }

    pub fn del(&self, endpoint: SocketAddrV6) {
        let key = self.make_key(&endpoint);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
        }
    }

    pub fn exists(&self, endpoint: SocketAddrV6) -> bool {
        let key = self.make_key(&endpoint);
        let shard_idx = get_shard_index(&key);
        let mut lock = SHARDS[shard_idx].lock();
        if let Some(db) = lock.as_mut() {
            db.search(&key).is_some()
        } else {
            false
        }
    }

    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(SocketAddrV6, SystemTime),
    {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        for i in 0..SHARD_COUNT {
            let mut lock = SHARDS[i].lock();
            if let Some(db) = lock.as_mut() {
                let mut it = db.iter(start_key);
                while let Some((k, _)) = it.next_kv() {
                    if k[0] != Self::TABLE_ID {
                        break;
                    }
                    let ip = core::net::Ipv6Addr::from(<[u8; 16]>::try_from(&k[1..17]).unwrap());
                    let port = u16::from_be_bytes(k[17..19].try_into().unwrap());
                    let endpoint = SocketAddrV6::new(ip, port, 0, 0);
                    f(endpoint, SystemTime::UNIX_EPOCH); // Placeholder time
                }
            }
        }
    }

    pub fn iter(&self) -> Vec<(SocketAddrV6, SystemTime)> {
        let mut result = Vec::new();
        self.for_each(|e, t| result.push((e, t)));
        result
    }
}
