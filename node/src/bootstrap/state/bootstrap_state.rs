use alloc::vec::Vec;
use super::running_query::QuerySource;
use super::{CandidateAccounts, FrontierScan, PeerScoring, PriorityResult, RunningQueryContainer};
use crate::bootstrap::{AscPullQuerySpec, BootstrapConfig};
use rsnano_core::Account;
use rsnano_core::{utils::ContainerInfo, BlockHash};
use rsnano_messages::AscPullReqType;
use rsnano_network::Channel;
use rsnano_nullable_clock::Timestamp;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

pub struct BootstrapState {
    pub candidate_accounts: CandidateAccounts,
    pub(crate) scoring: PeerScoring,
    pub(crate) running_queries: RunningQueryContainer,
    pub frontier_scan: FrontierScan,
    pub counters: BootstrapCounters,
    pub(crate) frontier_ack_processor_busy: bool,
    pub last_outdated_accounts: VecDeque<Account>,
    pub(crate) stopped: bool,
}

impl BootstrapState {
    /// Diagnostics: number of in-flight queries for `account`.
    pub fn diag_queries_for(&self, account: &rsnano_core::Account) -> usize {
        self.running_queries.iter_account(account).count()
    }

    /// Diagnostics: (in-flight queries, tracked channels, in-flight per channel total, channels at limit).
    pub fn diag_summary(&self) -> (usize, usize, usize, usize) {
        let (n, total, at_limit) = self.scoring.summary();
        (self.running_queries.len(), n, total, at_limit)
    }

    pub fn new(config: BootstrapConfig) -> Self {
        let mut scoring = PeerScoring::new();
        scoring.set_channel_limit(config.channel_limit);

        Self {
            candidate_accounts: CandidateAccounts::new(config.candidate_accounts.clone()),
            scoring,
            frontier_scan: FrontierScan::new(config.frontier_scan.clone()),
            running_queries: RunningQueryContainer::default(),
            counters: BootstrapCounters::default(),
            frontier_ack_processor_busy: false,
            last_outdated_accounts: VecDeque::new(),
            stopped: false,
        }
    }

    pub fn next_blocking_query(&mut self, channel: &Arc<Channel>, now: Timestamp) -> Option<AscPullQuerySpec> {
        let next = self.next_blocking(now);
        if !next.is_zero() {
            Some(Self::create_blocking_query(next, channel.clone()))
        } else {
            None
        }
    }

    fn create_blocking_query(next: BlockHash, channel: Arc<Channel>) -> AscPullQuerySpec {
        AscPullQuerySpec {
            channel,
            req_type: AscPullReqType::account_info_by_hash(next),
            account: Account::zero(),
            hash: next,
            cooldown_account: false,
        }
    }

    fn count_queries_by_hash(&self, hash: &BlockHash, source: QuerySource) -> usize {
        self.running_queries
            .iter_hash(hash)
            .filter(|i| i.source == source)
            .count()
    }

    pub fn next_priority(&mut self, now: Timestamp) -> PriorityResult {
        let mut next = self.candidate_accounts.next_priority(now, |account| {
            self.running_queries
                .count_by_account(account, QuerySource::Priority)
                == 0
        });

        if next.account.is_zero() {
            return Default::default();
        }

        #[cfg(not(test))]
        {
            if let Some(gap_hash) = self.candidate_accounts.gap_target(&next.account) {
                next.target_hash = gap_hash;
            } else if let Some(dep_hash) = self
                .candidate_accounts
                .next_dependency_for_account(&next.account)
            {
                next.target_hash = dep_hash;
            }
        }

        next
    }

    /* Waits for next available blocking block */
    pub fn next_blocking(&self, now: Timestamp) -> BlockHash {
        self.candidate_accounts
            .next_blocking(now, |hash| self.count_queries_by_hash(hash, QuerySource::Dependencies) == 0)
    }

    pub fn dependency_failed(&mut self, hash: &BlockHash, now: Timestamp) {
        self.candidate_accounts.dependency_failed(hash, now);
    }

    pub fn frontiers_processed(&mut self, outdated: &OutdatedAccounts) {
        self.counters.received_frontiers += outdated.fontiers_received;
        self.counters.outdated_accounts_found += outdated.accounts.len();

        #[cfg(not(test))]
        let prio = super::Priority::new(0.5);
        #[cfg(test)]
        let prio = CandidateAccounts::PRIORITY_CUTOFF;

        for account in &outdated.accounts {
            // Use the lowest possible priority here
            self.candidate_accounts
                .priority_set(account, prio);

            self.last_outdated_accounts.push_back(*account);
            if self.last_outdated_accounts.len() > 20 {
                self.last_outdated_accounts.pop_front();
            }
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf(
                "tags",
                self.running_queries.len(),
                RunningQueryContainer::ELEMENT_SIZE,
            )
            .node("accounts", self.candidate_accounts.container_info())
            .node("frontiers", self.frontier_scan.container_info())
            .node("peers", self.scoring.container_info())
            .finish()
    }
}

impl Default for BootstrapState {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[derive(Default, Clone)]
pub struct BootstrapCounters {
    pub received_frontiers: usize,
    pub outdated_accounts_found: usize,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct OutdatedAccounts {
    pub accounts: Vec<Account>,
    /// Accounts that exist but are outdated
    pub outdated: usize,
    /// Accounts that don't exist but have pending blocks in the ledger
    pub pending: usize,
    /// Total count of received frontiers
    pub fontiers_received: usize,
}
