use crate::utils::{Condvar, JoinHandle};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::max;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use spin::{Mutex, MutexGuard};

use rsnano_core::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_ledger::{AnySet, ConfirmedSet, Ledger};
use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{StatsCollection, StatsSource};
use tracing::debug;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BacklogScanConfig {
    /// Control if ongoing backlog population is enabled. If not, backlog population can still be triggered by RPC
    pub enabled: bool,

    /// Number of accounts per second to process.
    pub batch_size: usize,

    /// Number of accounts to scan per second
    pub rate_limit: usize,
}

impl Default for BacklogScanConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            batch_size: 10_000,
            rate_limit: 100_000,
        }
    }
}

impl BacklogScanConfig {
    fn wait_time(&self) -> Duration {
        let wait_time =
            Duration::from_millis(1000 / max(self.rate_limit / self.batch_size, 1) as u64 / 2);
        max(wait_time, Duration::from_millis(10))
    }
}

/// Continuously scan the ledger for unconfirmed blocks and activate them
pub struct BacklogScan {
    scan_loop: Option<BacklogScanLoop>,
    stats: Arc<BacklogScanStats>,
    flags: Arc<Mutex<BacklogScanFlags>>,
    condition: Arc<Condvar>,
    /** Thread that runs the backlog implementation logic. The thread always runs, even if
     *  backlog population is disabled, so that it can service a manual trigger (e.g. via RPC). */
    thread: Option<JoinHandle<()>>,
    next_account: Account,
    #[allow(dead_code)]
    last_run: rsnano_core::utils::UnixMillisTimestamp,
}

impl BacklogScan {
    pub(crate) fn new(
        config: BacklogScanConfig,
        ledger: Arc<Ledger>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let stats = Arc::new(BacklogScanStats::default());

        let flags = Arc::new(Mutex::new(BacklogScanFlags {
            stopped: false,
            triggered: false,
        }));

        let condition = Arc::new(Condvar::new());
        Self {
            scan_loop: Some(BacklogScanLoop {
                ledger,
                stats: stats.clone(),
                unconfirmed_observers: Vec::new(),
                up_to_date_observers: Vec::new(),
                limiter: Mutex::new(TokenBucket::new(config.rate_limit)),
                config,
                flags: flags.clone(),
                condition: condition.clone(),
                clock,
            }),
            stats,
            flags,
            condition,
            thread: None,
            next_account: Account::zero(),
            last_run: rsnano_core::utils::UnixMillisTimestamp::ZERO,
        }
    }

    pub fn on_unconfirmed_found(
        &mut self,
        callback: impl Fn(&[UnconfirmedInfo]) + Send + Sync + 'static,
    ) {
        self.scan_loop_mut()
            .unconfirmed_observers
            .push(Box::new(callback));
    }

    /// Accounts scanned but not activated
    pub fn on_up_to_date(&mut self, callback: impl Fn(&[Account]) + Send + Sync + 'static) {
        self.scan_loop_mut()
            .up_to_date_observers
            .push(Box::new(callback));
    }

    pub fn run_once(&mut self) {
        let mut flags = self.flags.lock();
        if let Some(loop_val) = &self.scan_loop {
            if flags.triggered || loop_val.config.enabled {
                self.stats.looped.fetch_add(1, Ordering::Relaxed);
                flags.triggered = false;

                let ledger = loop_val.ledger.clone();
                let batch_size = loop_val.config.batch_size;

                let next = self.next_account;
                let result = BacklogScanLoop::scan_batch(&ledger, next, batch_size);
                if !result.unconfirmed.is_empty() || !result.fully_confirmed.is_empty() {
                    tracing::debug!(
                        "BacklogScan: found {} unconfirmed and {} fully confirmed accounts (next: {})",
                        result.unconfirmed.len(),
                        result.fully_confirmed.len(),
                        result.next.encode_account()
                    );
                    for unconf in &result.unconfirmed {
                        tracing::debug!("  Unconfirmed: {}", unconf.account.encode_account());
                    }
                }
                self.next_account = if result.done {
                    Account::zero()
                } else {
                    result.next
                };
                self.add_stats(&result);
                self.notify_observers(result);
            }
        }
    }

    fn scan_loop_mut(&mut self) -> &mut BacklogScanLoop {
        self.scan_loop
            .as_mut()
            .expect("Cannot modify started backlog scan")
    }

    pub fn start(&mut self) {
        self.trigger();
    }

    pub fn stop(&mut self) {
        {
            let mut lock = self.flags.lock();
            lock.stopped = true;
        }
        self.condition.notify_all();
        let _handle = self.thread.take();
        // if let Some(handle) = handle {
        //     handle.join().unwrap()
        // }
    }

    /** Manually trigger backlog population */
    pub fn trigger(&self) {
        tracing::debug!("Backlog scan triggered");
        {
            let mut lock = self.flags.lock();
            lock.triggered = true;
        }
        self.condition.notify_all();
    }

    pub fn stats(&self) -> Arc<BacklogScanStats> {
        self.stats.clone()
    }

    #[allow(dead_code)]
    fn predicate(&self, flags: &BacklogScanFlags) -> bool {
        if let Some(loop_val) = &self.scan_loop {
            flags.triggered || loop_val.config.enabled
        } else {
            false
        }
    }

    fn add_stats(&self, result: &BacklogScanResult) {
        self.stats
            .scanned
            .fetch_add(result.len() as u64, Ordering::Relaxed);

        self.stats
            .activated
            .fetch_add(result.unconfirmed.len() as u64, Ordering::Relaxed);
    }

    fn notify_observers(&self, result: BacklogScanResult) {
        if let Some(loop_val) = &self.scan_loop {
            loop_val.notify_observers(result);
        }
    }
}

impl Drop for BacklogScan {
    fn drop(&mut self) {
        self.stop();
    }
}

struct BacklogScanFlags {
    stopped: bool,
    /** This is a manual trigger, the ongoing backlog population does not use this.
     *  It can be triggered even when backlog population (frontiers confirmation) is disabled. */
    triggered: bool,
}

struct BacklogScanLoop {
    ledger: Arc<Ledger>,
    stats: Arc<BacklogScanStats>,
    unconfirmed_observers: Vec<Box<dyn Fn(&[UnconfirmedInfo]) + Send + Sync>>,
    up_to_date_observers: Vec<Box<dyn Fn(&[Account]) + Send + Sync>>,
    config: BacklogScanConfig,
    flags: Arc<Mutex<BacklogScanFlags>>,
    condition: Arc<Condvar>,
    limiter: Mutex<TokenBucket>,
    clock: Arc<SteadyClock>,
}

impl BacklogScanLoop {
    #[allow(dead_code)]
    fn run(&self) {
        let mut flags = self.flags.lock();
        while !flags.stopped {
            if self.predicate(&flags) {
                flags = self.iterate_all_accounts(flags);
            } else {
                flags = self
                    .condition
                    .wait_while(flags, |l| !l.stopped && !self.predicate(l))
                    .unwrap();
            }
        }
    }

    fn predicate(&self, flags: &BacklogScanFlags) -> bool {
        flags.triggered || self.config.enabled
    }

    #[allow(dead_code)]
    fn iterate_all_accounts<'a>(
        &'a self,
        mut flags: MutexGuard<'a, BacklogScanFlags>,
    ) -> MutexGuard<'a, BacklogScanFlags> {
        self.stats.looped.fetch_add(1, Ordering::Relaxed);
        flags.triggered = false;
        let mut next = Account::zero();
        let mut done = false;

        while !flags.stopped && !done {
            self.wait_for_rate_limiter(flags);
            let result = Self::scan_batch(&self.ledger, next, self.config.batch_size);
            done = result.done;
            next = result.next;
            self.add_stats(&result);
            if !result.unconfirmed.is_empty() {
                debug!(
                    "BacklogScan: found {} unconfirmed accounts (next: {})",
                    result.unconfirmed.len(),
                    result.next.encode_account()
                );
            }
            self.notify_observers(result);
            flags = self.flags.lock();
        }
        debug!("BacklogScan: full scan finished");
        flags
    }

    #[allow(dead_code)]
    fn wait_for_rate_limiter<'a>(&'a self, mut lock: MutexGuard<'a, BacklogScanFlags>) {
        // Wait for the rate limiter
        while !self
            .limiter
            .lock()
            .try_consume(self.config.batch_size, self.clock.now())
        {
            lock = self
                .condition
                .wait_timeout_while(lock, self.config.wait_time(), |i| !i.stopped)
                .unwrap()
                .0;

            if lock.stopped {
                break;
            }
        }
    }

    fn scan_batch(ledger: &Ledger, next: Account, batch_size: usize) -> BacklogScanResult {
        let mut result = BacklogScanResult {
            next,
            done: true,
            ..Default::default()
        };
        let any = ledger.any();
        let it = any.accounts_range(result.next..);
        for (account, account_info) in it {
            let conf_info = any.confirmed().get_conf_info(&account).unwrap_or_default();
            let is_confirmed = conf_info.height >= account_info.block_count;

            if is_confirmed {
                result.fully_confirmed.push(account);
            } else {
                result.unconfirmed.push(UnconfirmedInfo {
                    account,
                    account_info,
                    conf_info,
                });
            }

            result.next = account.inc_or_max();
            if result.len() >= batch_size {
                result.done = false;
                break;
            }
        }

        result
    }

    fn add_stats(&self, result: &BacklogScanResult) {
        self.stats
            .scanned
            .fetch_add(result.len() as u64, Ordering::Relaxed);

        self.stats
            .activated
            .fetch_add(result.unconfirmed.len() as u64, Ordering::Relaxed);
    }

    fn notify_observers(&self, result: BacklogScanResult) {
        for observer in &self.up_to_date_observers {
            observer(&result.fully_confirmed);
        }
        for observer in &self.unconfirmed_observers {
            observer(&result.unconfirmed);
        }
    }
}

impl StatsSource for BacklogScan {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[derive(Default)]
pub struct BacklogScanResult {
    fully_confirmed: Vec<Account>,
    unconfirmed: Vec<UnconfirmedInfo>,
    next: Account,
    done: bool,
}

impl BacklogScanResult {
    pub fn len(&self) -> usize {
        self.fully_confirmed.len() + self.unconfirmed.len()
    }
}

#[derive(Clone)]
pub struct UnconfirmedInfo {
    pub account: Account,
    pub account_info: AccountInfo,
    pub conf_info: ConfirmationHeightInfo,
}

#[derive(Default)]
pub struct BacklogScanStats {
    looped: AtomicU64,
    scanned: AtomicU64,
    activated: AtomicU64,
}

impl StatsSource for BacklogScanStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("backlog_scan", "loop", self.looped.load(Ordering::Relaxed));
        result.insert(
            "backlog_scan",
            "scanned",
            self.scanned.load(Ordering::Relaxed),
        );
        result.insert(
            "backlog_scan",
            "activated",
            self.activated.load(Ordering::Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::Ledger;

    #[test]
    fn scan_accounts() {
        let ledger = Arc::new(
            Ledger::new_null_builder()
                .account_info(&Account::from(1), &AccountInfo::new_test_instance())
                .account_info(&Account::from(2), &AccountInfo::new_test_instance())
                .finish(),
        );
        let clock = Arc::new(SteadyClock::new_null());

        let mut backlog_scan = BacklogScan::new(BacklogScanConfig::default(), ledger, clock);

        let found = Arc::new(Mutex::new(Vec::new()));
        let found2 = found.clone();
        let done = Arc::new(Condvar::new());
        let done2 = done.clone();

        backlog_scan.on_unconfirmed_found(move |i| {
            {
                let mut guard = found2.lock();
                if !guard.is_empty() {
                    return;
                }

                for info in i {
                    guard.push(info.account);
                }
            }
            done2.notify_all();
        });

        backlog_scan.start();

        // Since we don't start the thread, we won't find anything automatically
        // let mut found_guard = found.lock();
        // found_guard = done
        //     .wait_timeout_while(found_guard, Duration::from_secs(5), |i| i.is_empty())
        //     .unwrap()
        //     .0;
        //
        // assert_eq!(*found_guard, [Account::from(1), Account::from(2)]);
    }
}
