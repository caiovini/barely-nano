use rsnano_core::{utils::ContainerInfo, Account, Amount, PublicKey};
use rsnano_ledger_store::LedgerCache;
use alloc::sync::Arc;
use core::{
    mem::size_of,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};
use hashbrown::HashMap;
use spin::{RwLock, RwLockReadGuard};

#[derive(Default, Clone)]
pub struct RepWeights(HashMap<PublicKey, Amount>);

impl RepWeights {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn weight(&self, rep: &PublicKey) -> Amount {
        self.get(rep).cloned().unwrap_or_default()
    }
}

impl Deref for RepWeights {
    type Target = HashMap<PublicKey, Amount>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RepWeights {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Default, Clone)]
pub struct BootstrapWeights {
    pub weights: RepWeights,
    pub max_blocks: u64,
}

/// Returns the cached vote weight for the given representative.
/// If the weight is below the cache limit it returns 0.
/// During bootstrap it returns the preconfigured bootstrap weights.
pub struct RepWeightCache {
    weights: Arc<RwLock<RepWeights>>,
    bootstrap_weights: RwLock<RepWeights>,
    max_blocks: u64,
    pub ledger_cache: Arc<LedgerCache>,
    check_bootstrap_weights: AtomicBool,
}

impl RepWeightCache {
    pub fn new() -> Self {
        Self {
            weights: Arc::new(RwLock::new(RepWeights::new())),
            bootstrap_weights: RwLock::new(RepWeights::new()),
            max_blocks: 0,
            ledger_cache: Arc::new(LedgerCache::new()),
            check_bootstrap_weights: AtomicBool::new(false),
        }
    }

    pub fn with_bootstrap_weights(
        bootstrap_weights: BootstrapWeights,
        ledger_cache: Arc<LedgerCache>,
    ) -> Self {
        Self {
            weights: Arc::new(RwLock::new(RepWeights::new())),
            bootstrap_weights: RwLock::new(bootstrap_weights.weights),
            max_blocks: bootstrap_weights.max_blocks,
            ledger_cache,
            check_bootstrap_weights: AtomicBool::new(true),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, RepWeights> {
        if self.use_bootstrap_weights() {
            self.bootstrap_weights.read()
        } else {
            self.weights.read()
        }
    }

    pub fn use_bootstrap_weights(&self) -> bool {
        if self.check_bootstrap_weights.load(Ordering::SeqCst) {
            if self.ledger_cache.block_count.load(Ordering::SeqCst) < self.max_blocks {
                return true;
            } else {
                self.check_bootstrap_weights.store(false, Ordering::SeqCst);
            }
        }
        false
    }

    pub fn weight(&self, rep: &PublicKey) -> Amount {
        let guard = if self.use_bootstrap_weights() {
            self.bootstrap_weights.read()
        } else {
            self.weights.read()
        };

        guard.get(rep).cloned().unwrap_or_default()
    }

    pub fn bootstrap_weight_max_blocks(&self) -> u64 {
        self.max_blocks
    }

    pub fn bootstrap_weights(&self) -> HashMap<PublicKey, Amount> {
        self.bootstrap_weights.read().0.clone()
    }

    pub fn block_count(&self) -> u64 {
        self.ledger_cache.block_count.load(Ordering::SeqCst)
    }

    pub fn len(&self) -> usize {
        self.weights.read().len()
    }

    pub fn set(&self, account: PublicKey, weight: Amount) {
        self.weights.write().insert(account, weight);
    }

    pub(super) fn inner(&self) -> Arc<RwLock<RepWeights>> {
        self.weights.clone()
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("rep_weights", self.len(), size_of::<(Account, Amount)>())].into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache() {
        let cache = RepWeightCache::new();
        let rep = PublicKey::from(1);
        assert_eq!(cache.weight(&rep), Amount::zero());
        assert_eq!(cache.len(), 0);
        assert!(!cache.use_bootstrap_weights());
    }

    #[test]
    fn set_and_get_weight() {
        let cache = RepWeightCache::new();
        let rep = PublicKey::from(42);
        let weight = Amount::nano(1_000);
        cache.set(rep, weight);
        assert_eq!(cache.weight(&rep), weight);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn bootstrap_weights_fallback_until_max_blocks() {
        let rep = PublicKey::from(7);
        let live_weight = Amount::nano(100);
        let boot_weight = Amount::nano(500);

        let mut boot_weights = RepWeights::new();
        boot_weights.insert(rep, boot_weight);

        let ledger_cache = Arc::new(LedgerCache::new());
        let cache = RepWeightCache::with_bootstrap_weights(
            BootstrapWeights {
                weights: boot_weights,
                max_blocks: 1000,
            },
            ledger_cache.clone(),
        );
        cache.set(rep, live_weight);

        // Below max_blocks -> returns bootstrap weight
        ledger_cache.block_count.store(500, Ordering::SeqCst);
        assert!(cache.use_bootstrap_weights());
        assert_eq!(cache.weight(&rep), boot_weight);

        // Reaches max_blocks -> permanently switches to live weights
        ledger_cache.block_count.store(1000, Ordering::SeqCst);
        assert!(!cache.use_bootstrap_weights());
        assert_eq!(cache.weight(&rep), live_weight);

        // Even if block_count drops below max_blocks later, bootstrap weights stay disabled
        ledger_cache.block_count.store(10, Ordering::SeqCst);
        assert!(!cache.use_bootstrap_weights());
        assert_eq!(cache.weight(&rep), live_weight);
    }
}
