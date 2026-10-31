use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use hashbrown::{HashMap, HashSet};
use spin::Mutex;

use tracing::warn;

use rsnano_core::{
    deterministic_key, Amount, Block, BlockHash, Link, PrivateKey, PublicKey, RawKey, Root,
    StateBlockArgs, WorkNonce,
};
use rsnano_ledger::Ledger;
use rsnano_work::WorkThresholds;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KeyType {
    NotAnAccount,
    Unknown,
    Adhoc,
    Deterministic,
}

pub struct Wallet {
    pub representatives: Mutex<HashSet<PublicKey>>,
    pub representative: Mutex<PublicKey>,
    ledger: Arc<Ledger>,
    #[allow(dead_code)]
    work_thresholds: WorkThresholds,
    pub keys: Mutex<HashMap<PublicKey, PrivateKey>>,
    pub deterministic_keys: Mutex<HashSet<PublicKey>>,
    pub watch_keys: Mutex<HashSet<PublicKey>>,
    pub work_cache: Mutex<HashMap<PublicKey, WorkNonce>>,
    pub seed: Mutex<RawKey>,
    pub deterministic_index: AtomicU32,
    pub password: Mutex<String>,
    pub locked: AtomicBool,
}

impl Wallet {
    pub fn new(
        ledger: Arc<Ledger>,
        work_thresholds: WorkThresholds,
        representative: PublicKey,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            representatives: Mutex::new(HashSet::new()),
            representative: Mutex::new(representative),
            ledger,
            work_thresholds,
            keys: Mutex::new(HashMap::new()),
            deterministic_keys: Mutex::new(HashSet::new()),
            watch_keys: Mutex::new(HashSet::new()),
            work_cache: Mutex::new(HashMap::new()),
            seed: Mutex::new(PrivateKey::new().raw_key()),
            deterministic_index: AtomicU32::new(0),
            password: Mutex::new(String::new()),
            locked: AtomicBool::new(false),
        })
    }

    pub fn insert_key(&self, priv_key: PrivateKey) -> PublicKey {
        let pub_key = priv_key.public_key();
        self.keys.lock().insert(pub_key, priv_key);
        pub_key
    }

    pub fn insert_deterministic_at(&self, index: u32) -> PublicKey {
        let seed = *self.seed.lock();
        let raw = deterministic_key(&seed, index);
        let priv_key = PrivateKey::from(raw);
        let pub_key = priv_key.public_key();
        self.keys.lock().insert(pub_key, priv_key);
        self.deterministic_keys.lock().insert(pub_key);
        let current = self.deterministic_index.load(Ordering::SeqCst);
        if index >= current {
            self.deterministic_index.store(index + 1, Ordering::SeqCst);
        }
        pub_key
    }

    pub fn insert_deterministic(&self) -> PublicKey {
        let index = self.deterministic_index.fetch_add(1, Ordering::SeqCst);
        let seed = *self.seed.lock();
        let raw = deterministic_key(&seed, index);
        let priv_key = PrivateKey::from(raw);
        let pub_key = priv_key.public_key();
        self.keys.lock().insert(pub_key, priv_key);
        self.deterministic_keys.lock().insert(pub_key);
        pub_key
    }

    pub fn create_block(
        &self,
        pub_key: &PublicKey,
        previous: BlockHash,
        representative: PublicKey,
        balance: Amount,
        link: Link,
        work: WorkNonce,
    ) -> anyhow::Result<Block> {
        let keys = self.keys.lock();
        let prv = keys
            .get(pub_key)
            .ok_or_else(|| anyhow::anyhow!("Key not found in wallet"))?;
        let args = StateBlockArgs {
            key: prv,
            previous,
            representative,
            balance,
            link,
            work,
        };
        Ok(Block::from(args))
    }

    pub fn get_key(&self, pub_key: &PublicKey) -> Option<PrivateKey> {
        self.keys.lock().get(pub_key).cloned()
    }

    pub fn work_update(&self, pub_key: &PublicKey, root: &Root, work: WorkNonce) {
        let latest = self.ledger.any().latest_root(&(*pub_key).into());
        if latest == *root {
            self.work_cache.lock().insert(*pub_key, work);
        } else {
            warn!("Cached work no longer valid, discarding");
        }
    }

    pub fn live(&self) -> bool {
        true
    }

    pub fn exists(&self, pub_key: &PublicKey) -> bool {
        self.keys.lock().contains_key(pub_key) || self.watch_keys.lock().contains(pub_key)
    }

    pub fn valid_password(&self) -> bool {
        !self.locked.load(Ordering::SeqCst)
    }

    pub fn key_type(&self, pub_key: &PublicKey) -> KeyType {
        if self.deterministic_keys.lock().contains(pub_key) {
            KeyType::Deterministic
        } else if self.keys.lock().contains_key(pub_key) || self.watch_keys.lock().contains(pub_key) {
            KeyType::Adhoc
        } else {
            KeyType::NotAnAccount
        }
    }
}
