use alloc::sync::Arc;
use core::time::Duration;
use alloc::vec::Vec;
use rsnano_core::{Amount, Networks};
use rsnano_ledger::Ledger;
use rsnano_nullable_clock::Timestamp;

pub struct TrendResult {
    pub trended: Amount,
    pub sample_count: usize,
}

pub struct OnlineWeightSampler {
    ledger: Arc<Ledger>,
    cutoff: Duration,
}

impl OnlineWeightSampler {
    pub fn new(ledger: Arc<Ledger>, network: Networks) -> Self {
        Self {
            ledger,
            cutoff: Self::cutoff_for(network),
        }
    }

    fn cutoff_for(network: Networks) -> Duration {
        match network {
            Networks::NanoLiveNetwork | Networks::NanoTestNetwork => {
                Duration::from_secs(60 * 60 * 24 * 7 * 2)
            }
            _ => {
                Duration::from_secs(60 * 60 * 24)
            }
        }
    }

    pub fn calculate_trend(&self) -> TrendResult {
        let samples = self.load_samples();
        let sample_count = samples.len();
        let trended = self.medium_weight(samples);
        TrendResult {
            trended,
            sample_count,
        }
    }

    fn load_samples(&self) -> Vec<Amount> {
        self.ledger
            .store
            .online_weight
            .iter()
            .map(|(_, amount)| amount)
            .collect()
    }

    fn medium_weight(&self, mut items: Vec<Amount>) -> Amount {
        if items.is_empty() {
            Amount::zero()
        } else {
            let median_idx = items.len() / 2;
            items.sort();
            items[median_idx]
        }
    }

    pub fn add_sample(&self, current_online_weight: Amount, now: Timestamp) {
        self.sanitize_samples(now);
        self.insert_new_sample(current_online_weight, now);
    }

    pub fn sanitize(&self, now: Timestamp) {
        self.sanitize_samples(now);
    }

    fn sanitize_samples(&self, now: Timestamp) {
        let to_delete = self.samples_to_delete(now);
        for timestamp in to_delete {
            self.ledger.store.online_weight.del(timestamp);
        }
    }

    fn samples_to_delete(&self, now: Timestamp) -> Vec<u64> {
        let mut to_delete = Vec::new();
        to_delete.extend(self.old_samples(now));
        to_delete.extend(self.future_samples(now));
        to_delete
    }

    fn old_samples(&self, now: Timestamp) -> Vec<u64> {
        let timestamp_cutoff = (now - self.cutoff).as_secs();
        self.ledger
            .store
            .online_weight
            .iter()
            .map(|(ts, _)| ts)
            .take_while(move |ts| *ts < timestamp_cutoff)
            .collect()
    }

    fn future_samples(&self, now: Timestamp) -> Vec<u64> {
        let timestamp_now = now.as_secs();
        self.ledger
            .store
            .online_weight
            .iter_rev()
            .map(|(ts, _)| ts)
            .take_while(move |ts| *ts > timestamp_now)
            .collect()
    }

    fn insert_new_sample(
        &self,
        current_online_weight: Amount,
        now: Timestamp,
    ) {
        self.ledger.store.online_weight.put(
            now.as_secs(),
            &current_online_weight,
        );
    }
}
