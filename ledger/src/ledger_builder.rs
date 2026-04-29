use alloc::sync::Arc;
use core::cmp::{max, min};

use rsnano_core::{utils::get_cpu_count, Amount};
use rsnano_stats::Stats;
use rsnano_ledger_store::{BtreeStore, LedgerCache};

use crate::{BootstrapWeights, Ledger, LedgerConstants, RepWeightCache};

pub struct LedgerBuilder {
    bootstrap_weights: Option<BootstrapWeights>,
    stats: Option<Arc<Stats>>,
    min_rep_weight: Amount,
    ledger_constants: Option<LedgerConstants>,
    thread_count: usize,
}

impl LedgerBuilder {
    pub fn new() -> Self {
        Self {
            bootstrap_weights: None,
            stats: None,
            min_rep_weight: Amount::zero(),
            ledger_constants: None,
            thread_count: 0,
        }
    }

    pub fn bootstrap_weights(mut self, weights: BootstrapWeights) -> Self {
        self.bootstrap_weights = Some(weights);
        self
    }

    pub fn constants(mut self, constants: LedgerConstants) -> Self {
        self.ledger_constants = Some(constants);
        self
    }

    pub fn stats(mut self, stats: Arc<Stats>) -> Self {
        self.stats = Some(stats);
        self
    }

    pub fn min_rep_weight(mut self, weight: Amount) -> Self {
        self.min_rep_weight = weight;
        self
    }

    pub fn init_thread_count(mut self, count: usize) -> Self {
        self.thread_count = count;
        self
    }

    pub fn finish(mut self) -> anyhow::Result<Ledger> {
        let ledger_cache = Arc::new(LedgerCache::new());
        let bootstrap_weights = self.bootstrap_weights.unwrap_or_default();

        let rep_weights = Arc::new(RepWeightCache::with_bootstrap_weights(
            bootstrap_weights,
            ledger_cache.clone(),
        ));

        let stats = self.stats.unwrap_or_else(|| Arc::new(Stats::default()));
        let ledger_constants = self
            .ledger_constants
            .unwrap_or_else(|| LedgerConstants::unit_test());

        if self.thread_count == 0 {
            self.thread_count = max(1, min(40, 11 * get_cpu_count()));
        }

        let store = BtreeStore::new();

        Ledger::new(
            store,
            ledger_constants,
            self.min_rep_weight,
            rep_weights.clone(),
            stats.clone(),
            self.thread_count,
        )
    }
}
