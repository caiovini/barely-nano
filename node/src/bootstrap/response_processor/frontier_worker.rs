use alloc::vec::Vec;
use spin::Mutex;

use rsnano_core::Frontier;
use rsnano_ledger::OwningAnySet;
use rsnano_stats::{DetailType, StatType, Stats};

use super::frontier_checker::FrontierChecker;
use crate::bootstrap::state::{BootstrapState, OutdatedAccounts};

/// Handles received frontiers
pub(crate) struct FrontierWorker<'a> {
    stats: &'a Stats,
    state: &'a Mutex<BootstrapState>,
    checker: FrontierChecker<'a>,
}

impl<'a> FrontierWorker<'a> {
    pub(crate) fn new(
        any: &'a OwningAnySet<'a>,
        stats: &'a Stats,
        state: &'a Mutex<BootstrapState>,
    ) -> Self {
        Self {
            stats,
            state,
            checker: FrontierChecker::new(any),
        }
    }

    pub fn process(&mut self, frontiers: Vec<Frontier>) {
        let t0 = kernel_common::clint::mtime();
        let outdated = self.checker.get_outdated_accounts(&frontiers);
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::ProcessMs,
            (kernel_common::clint::mtime() - t0) * 1000 / kernel_common::clint::FREQ,
        );
        if !outdated.accounts.is_empty() {
            tracing::debug!(
                "FrontierAckProcessor: Found {} outdated accounts (outdated: {}, pending: {})",
                outdated.accounts.len(),
                outdated.outdated,
                outdated.pending
            );
        }
        self.update_stats(&frontiers, &outdated);
        self.state.lock().frontiers_processed(&outdated);
    }

    fn update_stats(&self, frontiers: &[Frontier], outdated: &OutdatedAccounts) {
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Processed,
            frontiers.len() as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Prioritized,
            outdated.accounts.len() as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Outdated,
            outdated.outdated as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Pending,
            outdated.pending as u64,
        );
    }
}

