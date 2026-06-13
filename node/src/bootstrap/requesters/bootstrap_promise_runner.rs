use alloc::sync::Arc;
use crate::bootstrap::{state::BootstrapState, BootstrapPromise, PollResult};
use rsnano_nullable_clock::SteadyClock;
use spin::{ Mutex };
use crate::utils::Condvar;
use core::time::Duration;
use core::cmp::min;

/// Calls a requester to create a bootstrap request and then sends it to
/// the peered node
#[allow(dead_code)]
pub(crate) struct BootstrapPromiseRunner {
    pub state: Arc<Mutex<BootstrapState>>,
    pub throttle_wait: Duration,
    pub state_changed: Arc<Condvar>,
    pub clock: Arc<SteadyClock>,
}

#[allow(dead_code)]
impl BootstrapPromiseRunner {
    const INITIAL_INTERVAL: Duration = Duration::from_millis(5);

    pub fn run<P, T>(&self, mut promise: P) -> Option<T>
    where
        P: BootstrapPromise<T>,
    {
        let mut interval = Self::INITIAL_INTERVAL;

        while !self.stopped() {
            let now = self.clock.now();
            let result = {
                let mut state = self.state.lock();
                promise.poll(&mut state, now)
            };

            match result {
                PollResult::Finished(val) => return Some(val),
                PollResult::Wait => {
                    self.wait(interval);
                    interval = min(interval * 2, self.throttle_wait);
                }
                PollResult::Progress => {
                    interval = Self::INITIAL_INTERVAL;
                }
            }
        }

        None
    }

    fn stopped(&self) -> bool {
        self.state.lock().stopped
    }

    fn wait(&self, duration: Duration) {
        let guard = self.state.lock();
        let _ = self.state_changed.wait_timeout(guard, duration);
    }
}
