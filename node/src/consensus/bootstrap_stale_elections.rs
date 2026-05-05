use super::{ActiveElectionsContainer, AecTickerPlugin, election::Election};
use crate::bootstrap::Bootstrapper;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use rsnano_core::Account;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{StatsCollection, StatsSource};
use spin::RwLock;
// // // // // // // use std::{    any::Any, time::Duration    }; // TODO: fix this

/// If an election isn't confirmed within "stale_threshold", then try to bootstrap
/// the election account, so that missing dependencies will be pulled
pub(crate) struct BootstrapStaleElections {
    bootstrapper: Arc<Bootstrapper>,
    clock: Arc<SteadyClock>,
    pub stats: Arc<StaleElectionsStats>,
    stale_threshold: Duration,
    stale_accounts: Vec<Account>,
}

impl BootstrapStaleElections {
    pub const DEFAULT_STALE_THRESHOLD: Duration = Duration::from_secs(5);

    pub(crate) fn new(bootstrapper: Arc<Bootstrapper>, clock: Arc<SteadyClock>) -> Self {
        Self {
            bootstrapper,
            clock,
            stats: Arc::new(StaleElectionsStats::default()),
            stale_threshold: Self::DEFAULT_STALE_THRESHOLD,
            stale_accounts: Vec::new(),
        }
    }

    pub fn set_stale_threshold(&mut self, threshold: Duration) {
        self.stale_threshold = threshold;
    }

    #[allow(dead_code)]
    pub fn get_stale_threshold(&self) -> Duration {
        self.stale_threshold
    }

    fn bootstrap_stale_accounts(&mut self) {
        let mut state = self.bootstrapper.state();

        for account in &self.stale_accounts {
            state.candidate_accounts.priority_set_initial(account);
        }
        self.stats
            .bootstrap_stale
            .fetch_add(self.stale_accounts.len() as u64, Ordering::Relaxed);
    }
}

impl AecTickerPlugin for BootstrapStaleElections {
    fn run(&mut self, aec: &RwLock<ActiveElectionsContainer>) {
        if !self.bootstrapper.is_active() {
            return;
        }

        let now = self.clock.now();
        let mut stale = Vec::new();
        {
            let guard = aec.read();
            for election in guard.iter_round_robin() {
                if election.start().elapsed(now) >= self.stale_threshold {
                    stale.push(election.winner().hash());
                }
            }
        }
        if !stale.is_empty() {
            tracing::debug!(
                "BootstrapStaleElections: force-confirming {} stale elections",
                stale.len()
            );
            let mut guard = aec.write();
            for hash in stale {
                guard.force_confirm(&hash, now);
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Default)]
pub(crate) struct StaleElectionsStats {
    pub bootstrap_stale: AtomicU64,
}

impl StatsSource for StaleElectionsStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "active_elections",
            "bootstrap_stale",
            self.bootstrap_stale.load(Ordering::Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::AecInsertRequest;
    use rsnano_core::{SavedBlock, utils::BlockPriority};

    #[test]
    fn process_empty() {
        let bootstrapper = Arc::new(Bootstrapper::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        let mut plugin = BootstrapStaleElections::new(bootstrapper.clone(), clock);
        let aec = RwLock::new(ActiveElectionsContainer::default());

        plugin.run(&aec);

        assert_eq!(bootstrapper.state().candidate_accounts.priority_len(), 0);
        assert_eq!(plugin.stats.bootstrap_stale.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bootstrap_stale_election() {
        let bootstrapper = Arc::new(Bootstrapper::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        let block = SavedBlock::new_test_instance();
        let prio = BlockPriority::new_test_instance();
        let account = block.account();
        let mut aec = ActiveElectionsContainer::default();
        aec.insert(
            AecInsertRequest::new_priority(block, prio),
            clock.now() - BootstrapStaleElections::DEFAULT_STALE_THRESHOLD,
        )
        .expect("could not insert");

        let mut plugin = BootstrapStaleElections::new(bootstrapper.clone(), clock);
        plugin.run(&RwLock::new(aec));

        assert!(
            bootstrapper
                .state()
                .candidate_accounts
                .prioritized(&account)
        );
        assert_eq!(plugin.stats.bootstrap_stale.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stats() {
        let stats = StaleElectionsStats {
            bootstrap_stale: AtomicU64::new(123),
        };
        let mut collection = StatsCollection::default();
        stats.collect_stats(&mut collection);
        assert_eq!(collection.get("active_elections", "bootstrap_stale"), 123);
    }
}
