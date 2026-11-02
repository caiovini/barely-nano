use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::{
    history::RebroadcastHistoryConfig,
    rebroadcast_processor::{RebroadcastProcessor, RebroadcastStats},
    VoteRebroadcastQueue,
};
use crate::transport::MessageFlooder;
use rsnano_ledger::RepWeightCache;
use rsnano_nullable_clock::SteadyClock;

/// Rebroadcasts votes that were created by other nodes
pub struct VoteRebroadcaster {
    queue: Arc<VoteRebroadcastQueue>,
    processor: Mutex<RebroadcastProcessor>,
    started: AtomicBool,
    vote_processed_callback: Option<Box<dyn Fn() + Send + Sync>>,
    pub(crate) stats: Arc<RebroadcastStats>,
}

impl VoteRebroadcaster {
    pub(crate) fn new(
        queue: Arc<VoteRebroadcastQueue>,
        message_flooder: MessageFlooder,
        rep_weights: Arc<RepWeightCache>,
        clock: Arc<SteadyClock>,
        history_config: RebroadcastHistoryConfig,
    ) -> Self {
        let stats = Arc::new(RebroadcastStats::default());
        let processor = RebroadcastProcessor::new(
            message_flooder,
            rep_weights,
            clock,
            stats.clone(),
            history_config,
        );
        Self {
            queue,
            processor: Mutex::new(processor),
            started: AtomicBool::new(false),
            vote_processed_callback: None,
            stats,
        }
    }

    pub fn start(&self) {
        self.started.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.started.store(false, Ordering::SeqCst);
        self.queue.stop();
    }

    pub fn run_once(&self) -> bool {
        if !self.started.load(Ordering::Relaxed) || self.queue.stopped() {
            return false;
        }
        let Some((tier, vote)) = self.queue.try_dequeue() else {
            return false;
        };
        let _ = self.processor.lock().rebroadcast(tier, &vote);
        if let Some(cb) = &self.vote_processed_callback {
            cb();
        }
        true
    }

    #[cfg(test)]
    pub fn on_vote_processed(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        self.vote_processed_callback = Some(Box::new(callback));
    }
}

impl Drop for VoteRebroadcaster {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        consensus::{RepTiers, RepTiersConsumer},
        transport::FloodEvent,
    };
    use rsnano_core::{utils::OneShotNotification, Vote};
    use rsnano_output_tracker::OutputTrackerMt;

    #[test]
    fn rebroadcast() {
        let (mut rebroadcaster, queue, flood_tracker, _rep_weights) = create_fixture();

        let done = OneShotNotification::new();
        let done2 = done.clone();
        rebroadcaster.on_vote_processed(move || done2.notify(()));
        rebroadcaster.start();

        let vote = Arc::new(Vote::new_test_instance());
        let mut rep_tiers = RepTiers::default();
        rep_tiers.tier1.insert(vote.voter);
        queue.update_rep_tiers(rep_tiers);
        queue.enqueue(vote);

        assert!(rebroadcaster.run_once());
        done.wait();

        assert_eq!(flood_tracker.output().len(), 1);
    }

    fn create_fixture() -> (
        VoteRebroadcaster,
        Arc<VoteRebroadcastQueue>,
        Arc<OutputTrackerMt<FloodEvent>>,
        Arc<RepWeightCache>,
    ) {
        let queue = Arc::new(VoteRebroadcastQueue::build().finish());
        let message_flooder = MessageFlooder::new_null();
        let flood_tracker = message_flooder.track_floods();
        let rep_weights = Arc::new(RepWeightCache::new());
        let clock = Arc::new(SteadyClock::new_null());
        let rebroadcaster = VoteRebroadcaster::new(
            queue.clone(),
            message_flooder,
            rep_weights.clone(),
            clock,
            Default::default(),
        );
        (rebroadcaster, queue, flood_tracker, rep_weights)
    }
}
