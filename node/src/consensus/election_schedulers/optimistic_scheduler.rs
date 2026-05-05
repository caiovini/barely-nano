use alloc::collections::VecDeque;
use alloc::sync::Arc;
use hashbrown::{ HashMap };
use spin::{ Mutex, RwLock };
use core::sync::atomic::{ AtomicBool, Ordering };
use rsnano_nullable_clock::Timestamp as Instant;
use crate::utils::{Condvar, JoinHandle};
use core::mem::size_of;
use core::cmp::min;

use rsnano_core::{utils::ContainerInfo, Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_ledger::{AnySet, ConfirmedSet, Ledger};
use rsnano_stats::{DetailType, StatType, Stats};

use crate::{
    cementation::ConfirmingSet,
    config::NetworkConstants,
    consensus::{election::ElectionBehavior, ActiveElectionsContainer, AecInsertRequest},
};
use rsnano_nullable_clock::SteadyClock;

#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerConfig {
    pub gap_threshold: u64,
    pub max_size: usize,
    pub optimistic_limit_percentage: usize,
}

impl OptimisticSchedulerConfig {
    pub fn new() -> Self {
        Self {
            gap_threshold: 32,
            max_size: 1024 * 64,
            optimistic_limit_percentage: 10,
        }
    }
}

impl Default for OptimisticSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OptimisticScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    config: OptimisticSchedulerConfig,
    stopped: AtomicBool,
    condition: Condvar,
    candidates: Mutex<OrderedCandidates>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    network_constants: NetworkConstants,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    pub max_elections: usize,
}

impl OptimisticScheduler {
    pub fn new(
        config: OptimisticSchedulerConfig,
        stats: Arc<Stats>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        network_constants: NetworkConstants,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let max_elections =
            active_elections.read().max_len() * config.optimistic_limit_percentage / 100;

        Self {
            thread: Mutex::new(None),
            config,
            stopped: AtomicBool::new(true),
            condition: Condvar::new(),
            candidates: Mutex::new(OrderedCandidates::default()),
            stats,
            active_elections,
            network_constants,
            ledger,
            confirming_set,
            clock,
            max_elections,
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify();
        let _handle = self.thread.lock().take();
        // if let Some(handle) = handle {
        //     handle.join().unwrap();
        // }
    }

    /// Notify about changes in AEC vacancy
    pub fn notify(&self) {
        self.condition.notify_all();
    }

    fn activate_predicate(
        &self,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        let big_enough_gap =
            account_info.block_count - conf_info.height > self.config.gap_threshold;

        let nothing_confirmed_yet = conf_info.height == 0;

        big_enough_gap | nothing_confirmed_yet
    }

    /// Called from backlog population to process accounts with unconfirmed blocks
    pub fn activate(
        &self,
        account: &Account,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }

        if self.activate_predicate(account_info, conf_info) {
            {
                let mut candidates = self.candidates.lock();
                // Prevent duplicate candidate accounts
                if candidates.contains(&account) {
                    return false; // Not activated
                }
                // Limit candidates container size
                if candidates.len() >= self.config.max_size {
                    return false; // Not activated
                }

                self.stats
                    .inc(StatType::OptimisticScheduler, DetailType::Activated);
                candidates.insert(*account, self.clock.now());
            }
            true // Activated
        } else {
            false // Not activated
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.candidates.lock();
        [(
            "candidates",
            guard.len(),
            size_of::<Account>() * 2 + size_of::<Instant>(),
        )]
        .into()
    }

    fn predicate(&self, candidates: &OrderedCandidates) -> bool {
        let active = self.active_elections.read();
        let vacancy = self.max_elections as i64
            - active.count_by_behavior(ElectionBehavior::Optimistic) as i64;
        let vacancy = min(vacancy, active.vacancy());

        if vacancy <= 0 {
            return false;
        }
        if let Some((_account, time)) = candidates.front() {
            time.elapsed(self.clock.now()) >= self.network_constants.optimistic_activation_delay
        } else {
            false
        }
    }

    #[allow(dead_code)]
    fn run(&self) {
        let mut guard = self.candidates.lock();
        while !self.stopped.load(Ordering::SeqCst) {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Loop);

            if self.predicate(&guard) {
                let any = self.ledger.any();

                while self.predicate(&guard) {
                    let (account, time) = guard.pop_front().unwrap();
                    drop(guard);
                    self.run_one(&any, account, time);
                    guard = self.candidates.lock();
                }
            }

            guard = self
                .condition
                .wait_timeout_while(
                    guard,
                    self.network_constants.optimistic_activation_delay / 2,
                    |g| !self.stopped.load(Ordering::SeqCst) && !self.predicate(g),
                )
                .unwrap()
                .0;
        }
    }

    pub fn run_once(&self) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }

        let mut guard = self.candidates.lock();
        if self.predicate(&guard) {
            let any = self.ledger.any();

            while self.predicate(&guard) {
                let (account, time) = guard.pop_front().unwrap();
                drop(guard);
                self.run_one(&any, account, time);
                guard = self.candidates.lock();
            }
        }
    }

    fn run_one(&self, any: &impl AnySet, account: Account, _time: Instant) {
        let Some(head) = any.account_head(&account) else {
            return;
        };
        if let Some(block) = any.get_block(&head) {
            // Ensure block is not already confirmed
            if !self.confirming_set.contains(&block.hash())
                || any.confirmed().block_exists_or_pruned(&block.hash())
            {
                // Try to insert it into AEC
                // We check for AEC vacancy inside our predicate
                let now = self.clock.now();
                let priority = any.block_priority(&block);
                let inserted = self
                    .active_elections
                    .write()
                    .insert(AecInsertRequest::new_optimistic(block, priority), now)
                    .is_ok();

                if inserted {
                    self.stats
                        .inc(StatType::OptimisticScheduler, DetailType::Insert);
                } else {
                    self.stats
                        .inc(StatType::OptimisticScheduler, DetailType::InsertFailed);
                }
            }
        }
    }
}

impl Drop for OptimisticScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        // debug_assert!(self.thread.lock().is_none())
    }
}

pub trait OptimisticSchedulerExt {
    fn start(&self);
}

impl OptimisticSchedulerExt for Arc<OptimisticScheduler> {
    fn start(&self) {
        self.stopped.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct OrderedCandidates {
    by_account: HashMap<Account, Instant>,
    sequenced: VecDeque<Account>,
}

impl OrderedCandidates {
    fn insert(&mut self, account: Account, time: Instant) {
        if self.by_account.insert(account, time).is_some() {
            self.sequenced.retain(|i| *i != account);
        }
        self.sequenced.push_back(account);
    }

    fn len(&self) -> usize {
        self.sequenced.len()
    }

    fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    fn front(&self) -> Option<(Account, Instant)> {
        self.sequenced
            .front()
            .and_then(|account| self.by_account.get(account).map(|time| (*account, *time)))
    }

    fn pop_front(&mut self) -> Option<(Account, Instant)> {
        self.sequenced.pop_front().map(|account| {
            let time = self.by_account.remove(&account).unwrap();
            (account, time)
        })
    }
}
