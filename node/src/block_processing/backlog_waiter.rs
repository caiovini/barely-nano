#![allow(dead_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use spin::Mutex;
use core::time::Duration;
use core::cmp::min;

use tracing::warn;

use rsnano_ledger::Ledger;
use rsnano_nullable_clock::{SteadyClock, Timestamp};

use super::BlockProcessorQueue;
use rsnano_stats::{StatsCollection, StatsSource};

/// Waits for the backlog to fall below the backlog limit
pub(crate) struct BacklogWaiter {
    queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    max_backlog: u64,
    call_count: AtomicUsize,
    cooldown_count: AtomicUsize,
    last_log: Mutex<Option<Timestamp>>,
    clock: Arc<SteadyClock>,
}

impl BacklogWaiter {
    pub fn new(
        queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        clock: Arc<SteadyClock>,
        max_backlog: u64,
    ) -> Self {
        Self {
            queue,
            ledger,
            max_backlog,
            call_count: AtomicUsize::new(0),
            cooldown_count: AtomicUsize::new(0),
            last_log: Mutex::new(None),
            clock,
        }
    }

    #[allow(dead_code)]
    pub fn new_null() -> Self {
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let ledger = Arc::new(Ledger::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(queue, ledger, clock, 1000)
    }

    pub fn wait_for_backlog(&self) {
        self.call_count.fetch_add(1, Relaxed);
        let backlog_count = self.ledger.backlog_count();
        let throttle_wait = throttle_wait(backlog_count as u64, self.max_backlog);
        if throttle_wait.is_zero() {
            return;
        }

        let now = self.clock.now();
        if self.should_log(now) {
            warn!(
                throttle_ms = throttle_wait.as_millis(),
                backlog_size = backlog_count,
                "Backlog exceeded. Throttling!"
            );
        }

        self.cooldown_count.fetch_add(1, Relaxed);
        self.queue.wait(throttle_wait);
    }

    fn should_log(&self, now: Timestamp) -> bool {
        let mut last_log = self.last_log.lock();
        let should_log = match *last_log {
            Some(i) => i.elapsed(now) >= Duration::from_secs(15),
            None => true,
        };

        if should_log {
            *last_log = Some(now);
        }

        should_log
    }

    #[allow(dead_code)]
    pub fn call_count(&self) -> usize {
        self.call_count.load(Relaxed)
    }
}

impl StatsSource for BacklogWaiter {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "block_processor",
            "cooldown_backlog",
            self.cooldown_count.load(Relaxed),
        );
    }
}

fn throttle_wait(backlog_count: u64, max_backlog: u64) -> Duration {
    const BACKLOG_THROTTLE_MS: u64 = 100;
    const BACKLOG_THROTTLE_MAX_MS: u64 = 1000;

    let backlog_factor = backlog_factor(backlog_count, max_backlog);

    if backlog_factor < 1.0 {
        return Duration::ZERO;
    }

    // This uses a power of approximately 3.32, which gives ~1x at 1.0 and ~10x at 2.0
    let scaling = libm::pow(backlog_factor, 3.32);
    let throttle_wait_ms = min(
        (BACKLOG_THROTTLE_MS as f64 * scaling) as u64,
        BACKLOG_THROTTLE_MAX_MS,
    );

    Duration::from_millis(throttle_wait_ms)
}

fn backlog_factor(backlog_count: u64, max_backlog: u64) -> f64 {
    const BACKLOG_THRESHOLD: f64 = 1.5;

    if max_backlog == 0 || backlog_count <= max_backlog {
        return 0.0;
    }

    let max_with_threshold = max_backlog as f64 * BACKLOG_THRESHOLD;
    let factor = backlog_count as f64 / max_with_threshold;
    factor
}
