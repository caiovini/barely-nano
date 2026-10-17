use alloc::sync::Arc;
use spin::Mutex;
use hashbrown::{HashSet, HashMap};

use tracing::warn;

use rsnano_core::{Amount, Block, BlockHash, Link, PrivateKey, PublicKey, Root, StateBlockArgs, WorkNonce};
use rsnano_ledger::Ledger;
use rsnano_work::WorkThresholds;

pub struct Wallet {
    pub representatives: Mutex<HashSet<PublicKey>>,
    ledger: Arc<Ledger>,
    #[allow(dead_code)]
    work_thresholds: WorkThresholds,
    pub keys: Mutex<HashMap<PublicKey, PrivateKey>>,
}

impl Wallet {
    pub fn new(
        ledger: Arc<Ledger>,
        work_thresholds: WorkThresholds,
        _representative: PublicKey,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            representatives: Mutex::new(HashSet::new()),
            ledger,
            work_thresholds,
            keys: Mutex::new(HashMap::new()),
        })
    }

    pub fn insert_key(&self, priv_key: PrivateKey) -> PublicKey {
        let pub_key = priv_key.public_key();
        self.keys.lock().insert(pub_key, priv_key);
        pub_key
    }

    pub fn create_block(
        &self, 
        pub_key: &PublicKey, 
        previous: BlockHash, 
        representative: PublicKey, 
        balance: Amount, 
        link: Link, 
        work: WorkNonce
    ) -> anyhow::Result<Block> {
        let keys = self.keys.lock();
        let prv = keys.get(pub_key).ok_or_else(|| anyhow!("Key not found in wallet"))?;
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

    pub fn work_update(
        &self,
        _pub_key: &PublicKey,
        root: &Root,
        _work: WorkNonce,
    ) {
        let latest = self.ledger.any().latest_root(&_pub_key.into());
        if latest == *root {
            // self.store.work_put(txn, pub_key, work);
        } else {
            warn!("Cached work no longer valid, discarding");
        }
    }

    pub fn live(&self) -> bool {
        true
    }

    pub fn exists(&self, pub_key: &PublicKey) -> bool {
        self.keys.lock().contains_key(pub_key)
    }
}
