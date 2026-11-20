//! Read-only, incremental ledger recount used to diagnose drift between the cached
//! `block_count` / `confirmed_count` counters (seeded from the snapshot header) and the
//! actual per-account ledger state.
//!
//! The recount merge-joins `TABLE_ACCOUNTS` and `TABLE_CONFIRMATION_HEIGHT` (both keyed by
//! account, so both are sequential B-Tree range scans served by the multi-page read-ahead)
//! in small chunks, so the caller (Hart 4) never holds a shard lock for long.
//!
//! In read-only mode it never writes to the store and never touches the cached counters.
//! In repair mode (`start_repair`), orphan confirmation-height rows (account record lost in
//! the LMDB copy) are queued in `repair_queue` for `Ledger::recount_step` to rebuild from local
//! blocks, and the cached counters are set from the recounted totals once the pass completes.

use alloc::vec::Vec;
use rsnano_core::{Account, BlockHash};
use rsnano_ledger_store::BtreeStore;

/// Number of accounts with the largest `block_count - confirmation_height` gap to retain.
pub const RECOUNT_TOP_N: usize = 16;
/// Number of sample accounts retained per anomaly class.
pub const RECOUNT_SAMPLES: usize = 12;

#[derive(Clone, Copy, Default)]
pub struct RecountGap {
    pub account: Account,
    pub block_count: u64,
    pub conf_height: u64,
}

impl RecountGap {
    #[inline]
    pub fn gap(&self) -> u64 {
        self.block_count.saturating_sub(self.conf_height)
    }
}

pub struct LedgerRecount {
    pub started: bool,
    pub done: bool,
    pub start_tick: u64,
    pub end_tick: u64,
    /// Cached counters at start / end of the scan (for comparison).
    pub start_block_count: u64,
    pub start_confirmed_count: u64,
    pub start_account_count: u64,
    pub end_block_count: u64,
    pub end_confirmed_count: u64,
    pub end_account_count: u64,

    /// `TABLE_ACCOUNTS` totals.
    pub accounts: u64,
    pub sum_block_count: u64,
    /// `TABLE_CONFIRMATION_HEIGHT` totals.
    pub conf_entries: u64,
    pub sum_conf_height: u64,
    /// Accounts with no confirmation height entry (and the blocks they hold).
    pub missing_conf_accounts: u64,
    pub missing_conf_blocks: u64,
    /// Accounts whose confirmation height exceeds their block count.
    pub conf_over_accounts: u64,
    pub conf_over_blocks: u64,
    /// Confirmation height entries with no matching account.
    pub orphan_conf_entries: u64,
    pub orphan_conf_height: u64,
    /// Accounts with `conf_height < block_count`.
    pub unconfirmed_accounts: u64,
    pub unconfirmed_blocks: u64,

    pub top: [RecountGap; RECOUNT_TOP_N],
    pub top_len: usize,

    /// Samples + key-space histogram (by first account byte) of orphan confirmation heights,
    /// to tell whether they cluster (page-level copy damage) or are spread uniformly.
    pub orphan_samples: [Account; RECOUNT_SAMPLES],
    pub orphan_samples_len: usize,
    pub orphan_hist: [u32; 256],
    /// Samples of accounts that have no confirmation height entry.
    pub missing_conf_samples: [Account; RECOUNT_SAMPLES],
    pub missing_conf_samples_len: usize,

    /// Repair mode: queue orphan accounts for rebuild and apply corrected counters at the end.
    pub repair: bool,
    /// Set by the HTTP trigger; consumed by the driver when (re)starting the scan.
    pub repair_requested: bool,
    pub repair_queue: Vec<(Account, BlockHash)>,
    pub missing_conf_queue: Vec<(Account, u64, BlockHash, u64)>,
    pub repaired_accounts: u64,
    pub repaired_blocks: u64,
    pub repaired_missing_conf_accounts: u64,
    pub repaired_missing_conf_blocks: u64,
    pub repair_failed: u64,
    pub counters_applied: bool,
    pub applied_block_count: u64,
    pub applied_confirmed_count: u64,
    pub applied_account_count: u64,

    next_acc: Option<Account>,
    next_conf: Option<Account>,
    acc_buf: Vec<(Account, u64, BlockHash, u64)>,
    conf_buf: Vec<(Account, u64, BlockHash)>,
}

impl LedgerRecount {
    pub const fn new() -> Self {
        Self {
            started: false,
            done: false,
            start_tick: 0,
            end_tick: 0,
            start_block_count: 0,
            start_confirmed_count: 0,
            start_account_count: 0,
            end_block_count: 0,
            end_confirmed_count: 0,
            end_account_count: 0,
            accounts: 0,
            sum_block_count: 0,
            conf_entries: 0,
            sum_conf_height: 0,
            missing_conf_accounts: 0,
            missing_conf_blocks: 0,
            conf_over_accounts: 0,
            conf_over_blocks: 0,
            orphan_conf_entries: 0,
            orphan_conf_height: 0,
            unconfirmed_accounts: 0,
            unconfirmed_blocks: 0,
            top: [RecountGap {
                account: Account::zero(),
                block_count: 0,
                conf_height: 0,
            }; RECOUNT_TOP_N],
            top_len: 0,
            orphan_samples: [Account::zero(); RECOUNT_SAMPLES],
            orphan_samples_len: 0,
            orphan_hist: [0; 256],
            missing_conf_samples: [Account::zero(); RECOUNT_SAMPLES],
            missing_conf_samples_len: 0,
            repair: false,
            repair_requested: false,
            repair_queue: Vec::new(),
            missing_conf_queue: Vec::new(),
            repaired_accounts: 0,
            repaired_blocks: 0,
            repaired_missing_conf_accounts: 0,
            repaired_missing_conf_blocks: 0,
            repair_failed: 0,
            counters_applied: false,
            applied_block_count: 0,
            applied_confirmed_count: 0,
            applied_account_count: 0,
            next_acc: None,
            next_conf: None,
            acc_buf: Vec::new(),
            conf_buf: Vec::new(),
        }
    }

    fn snapshot_counters(store: &BtreeStore) -> (u64, u64, u64) {
        use core::sync::atomic::Ordering;
        (
            store.cache.block_count.load(Ordering::Relaxed),
            store.cache.confirmed_count.load(Ordering::Relaxed),
            store.cache.account_count.load(Ordering::Relaxed),
        )
    }

    /// Resets all totals and begins a new scan.
    pub fn start(&mut self, store: &BtreeStore, now_tick: u64) {
        let acc_buf = core::mem::take(&mut self.acc_buf);
        let conf_buf = core::mem::take(&mut self.conf_buf);
        *self = Self::new();
        self.acc_buf = acc_buf;
        self.conf_buf = conf_buf;
        self.started = true;
        self.start_tick = now_tick;
        let (b, c, a) = Self::snapshot_counters(store);
        self.start_block_count = b;
        self.start_confirmed_count = c;
        self.start_account_count = a;
        self.next_acc = Some(Account::zero());
        self.next_conf = Some(Account::zero());
    }

    /// Like `start`, but in repair mode.
    pub fn start_repair(&mut self, store: &BtreeStore, now_tick: u64) {
        self.start(store, now_tick);
        self.repair = true;
    }

    #[inline]
    pub fn is_running(&self) -> bool {
        self.started && !self.done
    }

    /// Processes up to `budget` accounts. Returns `true` while more work remains.
    pub fn step(&mut self, store: &BtreeStore, budget: usize, now_tick: u64) -> bool {
        if !self.is_running() {
            return false;
        }
        let budget = budget.max(16);

        // 1. Next chunk of accounts.
        self.acc_buf.clear();
        let mut acc_exhausted = true;
        if let Some(start) = self.next_acc {
            let buf = &mut self.acc_buf;
            store.account.for_each_while(start, |acc, info| {
                buf.push((acc, info.block_count, info.head, info.modified.as_u64()));
                if buf.len() >= budget {
                    acc_exhausted = false;
                    return false;
                }
                true
            });
        }
        // Inclusive upper bound of this chunk (`None` = to the end of the table).
        let mut upper: Option<Account> = if acc_exhausted {
            None
        } else {
            self.acc_buf.last().map(|e| e.0)
        };

        // 2. Matching confirmation height entries (capped so orphans can't blow up a step).
        self.conf_buf.clear();
        let conf_cap = budget * 2;
        let mut conf_capped = false;
        let mut conf_exhausted = true;
        if let Some(start) = self.next_conf {
            let buf = &mut self.conf_buf;
            store.confirmation_height.for_each_while(start, |acc, info| {
                if let Some(u) = upper {
                    if acc > u {
                        conf_exhausted = false;
                        return false;
                    }
                }
                buf.push((acc, info.height, info.frontier));
                if buf.len() >= conf_cap {
                    conf_capped = true;
                    conf_exhausted = false;
                    return false;
                }
                true
            });
        }
        if conf_capped {
            // Shrink this step to the last conf entry read; remaining accounts are revisited.
            let last_conf = self.conf_buf.last().map(|e| e.0).unwrap_or_default();
            upper = Some(last_conf);
            let keep = self.acc_buf.partition_point(|e| e.0 <= last_conf);
            if keep < self.acc_buf.len() {
                acc_exhausted = false;
                self.acc_buf.truncate(keep);
            }
        }

        // 3. Merge-join.
        let (mut i, mut j) = (0usize, 0usize);
        while i < self.acc_buf.len() || j < self.conf_buf.len() {
            let a = self.acc_buf.get(i).copied();
            let c = self.conf_buf.get(j).copied();
            match (a, c) {
                (Some((acc, bc, _, _)), Some((cacc, h, _))) if acc == cacc => {
                    self.record_account(acc, bc, Some(h));
                    i += 1;
                    j += 1;
                }
                (Some((acc, bc, head, modified)), Some((cacc, _, _))) if acc < cacc => {
                    self.record_account(acc, bc, None);
                    if self.repair && !head.is_zero() {
                        self.missing_conf_queue.push((acc, bc, head, modified));
                    }
                    i += 1;
                }
                (Some(_), Some((cacc, h, fr))) => {
                    self.record_orphan_conf(cacc, h);
                    if self.repair && !fr.is_zero() {
                        self.repair_queue.push((cacc, fr));
                    }
                    j += 1;
                }
                (Some((acc, bc, head, modified)), None) => {
                    self.record_account(acc, bc, None);
                    if self.repair && !head.is_zero() {
                        self.missing_conf_queue.push((acc, bc, head, modified));
                    }
                    i += 1;
                }
                (None, Some((cacc, h, fr))) => {
                    self.record_orphan_conf(cacc, h);
                    if self.repair && !fr.is_zero() {
                        self.repair_queue.push((cacc, fr));
                    }
                    j += 1;
                }
                (None, None) => break,
            }
        }

        // 4. Advance cursors.
        self.next_acc = if acc_exhausted && !conf_capped {
            None
        } else {
            match upper {
                Some(u) => u.inc(),
                None => None,
            }
        };
        self.next_conf = if conf_exhausted {
            None
        } else {
            match upper {
                Some(u) => u.inc(),
                None => None,
            }
        };

        if self.next_acc.is_none() && self.next_conf.is_none() {
            self.done = true;
            self.end_tick = now_tick;
            let (b, c, a) = Self::snapshot_counters(store);
            self.end_block_count = b;
            self.end_confirmed_count = c;
            self.end_account_count = a;
            self.acc_buf = Vec::new();
            self.conf_buf = Vec::new();
            return false;
        }
        true
    }

    #[inline]
    fn record_orphan_conf(&mut self, account: Account, h: u64) {
        self.orphan_hist[account.as_bytes()[0] as usize] += 1;
        // Spread samples over the scan: keep the first few, then every 4096th.
        if self.orphan_samples_len < RECOUNT_SAMPLES
            && (self.orphan_samples_len < 4 || self.orphan_conf_entries % 4096 == 0)
        {
            self.orphan_samples[self.orphan_samples_len] = account;
            self.orphan_samples_len += 1;
        }
        self.conf_entries += 1;
        self.sum_conf_height += h;
        self.orphan_conf_entries += 1;
        self.orphan_conf_height += h;
    }

    fn record_account(&mut self, account: Account, block_count: u64, conf: Option<u64>) {
        self.accounts += 1;
        self.sum_block_count += block_count;
        let h = match conf {
            Some(h) => {
                self.conf_entries += 1;
                self.sum_conf_height += h;
                h
            }
            None => {
                if self.missing_conf_samples_len < RECOUNT_SAMPLES
                    && (self.missing_conf_samples_len < 4 || self.missing_conf_accounts % 2048 == 0)
                {
                    self.missing_conf_samples[self.missing_conf_samples_len] = account;
                    self.missing_conf_samples_len += 1;
                }
                self.missing_conf_accounts += 1;
                self.missing_conf_blocks += block_count;
                0
            }
        };
        if h > block_count {
            self.conf_over_accounts += 1;
            self.conf_over_blocks += h - block_count;
            return;
        }
        let gap = block_count - h;
        if gap == 0 {
            return;
        }
        self.unconfirmed_accounts += 1;
        self.unconfirmed_blocks += gap;
        self.insert_top(RecountGap {
            account,
            block_count,
            conf_height: h,
        });
    }

    fn insert_top(&mut self, e: RecountGap) {
        let gap = e.gap();
        if self.top_len == RECOUNT_TOP_N && self.top[RECOUNT_TOP_N - 1].gap() >= gap {
            return;
        }
        // Insertion into a small descending-sorted array.
        let mut pos = self.top_len.min(RECOUNT_TOP_N - 1);
        if self.top_len < RECOUNT_TOP_N {
            self.top_len += 1;
        }
        while pos > 0 && self.top[pos - 1].gap() < gap {
            self.top[pos] = self.top[pos - 1];
            pos -= 1;
        }
        self.top[pos] = e;
    }
}

impl Default for LedgerRecount {
    fn default() -> Self {
        Self::new()
    }
}

/// Global recount state (driven by Hart 4, read by `/api/metrics` with `try_lock`).
pub static LEDGER_RECOUNT: spin::Mutex<LedgerRecount> = spin::Mutex::new(LedgerRecount::new());

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(n: u8) -> Account {
        let mut b = [0u8; 32];
        b[31] = n;
        Account::from_bytes(b)
    }

    #[test]
    fn top_gaps_sorted_and_bounded() {
        let mut r = LedgerRecount::new();
        for n in 0..40u8 {
            r.record_account(acc(n), 100 + n as u64, Some(0));
        }
        assert_eq!(r.top_len, RECOUNT_TOP_N);
        assert_eq!(r.top[0].gap(), 139);
        for k in 1..RECOUNT_TOP_N {
            assert!(r.top[k - 1].gap() >= r.top[k].gap());
        }
        assert_eq!(r.accounts, 40);
    }

    #[test]
    fn classifies_missing_over_and_orphans() {
        let mut r = LedgerRecount::new();
        r.record_account(acc(1), 10, None);
        r.record_account(acc(2), 5, Some(7));
        r.record_account(acc(3), 8, Some(8));
        r.record_orphan_conf(acc(9), 4);
        assert_eq!(r.missing_conf_accounts, 1);
        assert_eq!(r.missing_conf_blocks, 10);
        assert_eq!(r.conf_over_accounts, 1);
        assert_eq!(r.conf_over_blocks, 2);
        assert_eq!(r.orphan_conf_entries, 1);
        assert_eq!(r.sum_conf_height, 7 + 8 + 4);
        assert_eq!(r.sum_block_count, 23);
        assert_eq!(r.unconfirmed_blocks, 10);
        assert_eq!(r.orphan_samples_len, 1);
        assert_eq!(r.orphan_hist[0], 1);
        assert_eq!(r.missing_conf_samples_len, 1);
    }
}
