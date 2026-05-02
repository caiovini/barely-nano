use core::time::Duration;
use rsnano_nullable_clock::Timestamp as Instant;
use alloc::sync::Arc;
use super::{OnlineReps, OnlineWeightSampler};
use rsnano_core::utils::{CancellationToken, Tickable};
use rsnano_nullable_clock::SteadyClock;
use spin::{ Mutex };
// // // // // // // use std::{    time::Duration, time::Instant    }; // TODO: fix this
use tracing::info;

pub struct OnlineWeightCalculation {
    sampler: OnlineWeightSampler,
    online_reps: Arc<Mutex<OnlineReps>>,
    clock: Arc<SteadyClock>,
    first_run: bool,
    last_sample: Instant,
}

impl OnlineWeightCalculation {
    pub fn new(
        sampler: OnlineWeightSampler,
        online_reps: Arc<Mutex<OnlineReps>>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            sampler,
            online_reps,
            clock,
            first_run: true,
            last_sample: Instant::now(),
        }
    }

    fn calculate_trended_weight(&mut self) {
        let result = self.sampler.calculate_trend();
        info!(
            "Trended weight updated: {}, samples: {}",
            result.trended.format_balance(0),
            result.sample_count
        );
        self.online_reps.lock().set_trended(result.trended);
    }
}

impl Tickable for OnlineWeightCalculation {
    fn tick(&mut self, _: &CancellationToken) {
        if self.first_run {
            // Don't sample online weight on first run, because it is always 0
            self.sampler.sanitize(self.clock.now());
            self.last_sample = Instant::now();
            self.calculate_trended_weight();
            self.first_run = false;
        } else {
            {
                let mut online = self.online_reps.lock();
                online.trim(self.clock.now());
                online.calculate_online_weight();
            }
            if self.clock.now().elapsed(self.last_sample) > Duration::from_secs(60) {
                let online_weight = self.online_reps.lock().online_weight();
                self.sampler.add_sample(online_weight, self.clock.now());
                self.calculate_trended_weight();
                self.last_sample = Instant::now();
            }
        }
    }
}
