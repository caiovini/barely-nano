use alloc::sync::Arc;
use spin::Mutex;
use hashbrown::HashSet;

use tracing::warn;

use rsnano_core::{PublicKey, Root, WorkNonce};
use rsnano_ledger::Ledger;
use rsnano_work::WorkThresholds;

pub struct Wallet {
    pub representatives: Mutex<HashSet<PublicKey>>,
    ledger: Arc<Ledger>,
    work_thresholds: WorkThresholds,
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
        })
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

    pub fn exists(&self, _pub_key: &PublicKey) -> bool {
        false
    }
}
