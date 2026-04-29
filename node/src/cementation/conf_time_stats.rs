use alloc::sync::Arc;
use spin::Mutex;
use core::time::Duration;
use alloc::vec::Vec;

use rsnano_stats::{StatsCollection, StatsSource};
use rsnano_nullable_clock::SteadyClock;

use crate::{
    block_processing::LedgerEvent, ledger_event_processor::LedgerEventProcessorPlugin,
    utils::BoundedVecDeque,
};

const STATS_KEY: &str = "confirmation_time";
const DEFAULT_SAMPLE_SIZE: usize = 1000;

/// Tracks duration for p90 p95 and p99 of the last 1000 confirmations
pub(crate) struct ConfTimeStats {
    durations: BoundedVecDeque<u64>,
}

impl ConfTimeStats {
    pub fn new(sample_size: usize) -> Self {
        Self {
            durations: BoundedVecDeque::new(sample_size),
        }
    }

    pub fn add(&mut self, duration: Duration) {
        self.durations.push_back(duration.as_millis() as u64);
    }
}

impl Default for ConfTimeStats {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_SIZE)
    }
}

impl StatsSource for ConfTimeStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        let mut sorted: Vec<_> = self.durations.iter().cloned().collect();
        sorted.sort();

        let percentile = |perc: usize| -> u64 {
            if sorted.is_empty() {
                0
            } else {
                sorted[sorted.len() * perc / 100]
            }
        };

        result.insert(STATS_KEY, "p50", percentile(50));
        result.insert(STATS_KEY, "p90", percentile(90));
        result.insert(STATS_KEY, "p95", percentile(95));
        result.insert(STATS_KEY, "p99", percentile(99));
    }
}

pub(crate) struct TrackConfirmationTimes {
    stats: Arc<Mutex<ConfTimeStats>>,
    clock: Arc<SteadyClock>,
}

impl TrackConfirmationTimes {
    pub fn new(clock: Arc<SteadyClock>) -> Self {
        Self {
            stats: Arc::new(Mutex::new(Default::default())),
            clock,
        }
    }

    pub fn stats(&self) -> Arc<Mutex<ConfTimeStats>> {
        self.stats.clone()
    }
}

impl LedgerEventProcessorPlugin for TrackConfirmationTimes {
    fn process(&mut self, event: &LedgerEvent) {
        if let LedgerEvent::BlocksConfirmed(blocks) = event {
            let now = self.clock.now();
            let mut stats = self.stats.lock();

            for (block, _) in blocks {
                let conf_time = now - block.sideband().timestamp;
                stats.add(conf_time.to_duration());
            }
        }
    }
}
