use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use rsnano_core::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_ledger::{AnySet, ConfirmedSet, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{StatsCollection, StatsSource};

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
            batch_size: 256,
            rate_limit: 10_000,
        }
    }
}

/// Continuously scan the ledger for unconfirmed blocks and activate them
pub struct BacklogScan {
    scan_loop: Option<BacklogScanLoop>,
    stats: Arc<BacklogScanStats>,
    flags: Arc<Mutex<BacklogScanFlags>>,
    next_account: Account,
}

impl BacklogScan {
    pub(crate) fn new(
        config: BacklogScanConfig,
        ledger: Arc<Ledger>,
        _clock: Arc<SteadyClock>,
    ) -> Self {
        let stats = Arc::new(BacklogScanStats::default());

        let flags = Arc::new(Mutex::new(BacklogScanFlags {
            stopped: false,
            triggered: false,
        }));

        Self {
            scan_loop: Some(BacklogScanLoop {
                ledger,
                unconfirmed_observers: Vec::new(),
                up_to_date_observers: Vec::new(),
                config,
            }),
            stats,
            flags,
            next_account: Account::zero(),
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
        let mut lock = self.flags.lock();
        lock.stopped = true;
    }

    /** Manually trigger backlog population */
    pub fn trigger(&self) {
        tracing::debug!("Backlog scan triggered");
        let mut lock = self.flags.lock();
        lock.triggered = true;
    }

    pub fn stats(&self) -> Arc<BacklogScanStats> {
        self.stats.clone()
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
    unconfirmed_observers: Vec<Box<dyn Fn(&[UnconfirmedInfo]) + Send + Sync>>,
    up_to_date_observers: Vec<Box<dyn Fn(&[Account]) + Send + Sync>>,
    config: BacklogScanConfig,
}

impl BacklogScanLoop {
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

