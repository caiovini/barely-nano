use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use core::cmp::{max, min};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider, FairQueue},
    BlockHash, Root,
};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_network::{Channel, ChannelId, DeadChannelCleanupStep, TrafficType};
use rsnano_stats::{DetailType, Direction, StatType, Stats};

use super::{
    request_aggregator_impl::{AggregateResult, RequestAggregatorImpl},
    VoteGenerators,
};
use crate::consensus::election::VoteType;

#[derive(Clone, Debug, PartialEq)]
pub struct RequestAggregatorConfig {
    pub threads: usize,
    pub max_queue: usize,
    pub batch_size: usize,
}

impl RequestAggregatorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            threads: max(1, min(parallelism / 2, 4)),
            max_queue: 128,
            batch_size: 16,
        }
    }
}

/**
 * Pools together confirmation requests, separately for each endpoint.
 * Requests are added from network messages, and aggregated to minimize bandwidth and vote generation. Example:
 * * Two votes are cached, one for hashes {1,2,3} and another for hashes {4,5,6}
 * * A request arrives for hashes {1,4,5}. Another request arrives soon afterwards for hashes {2,3,6}
 * * The aggregator will reply with the two cached votes
 * Votes are generated for uncached hashes.
 */
pub struct RequestAggregator {
    config: RequestAggregatorConfig,
    stats: Arc<Stats>,
    vote_generators: Arc<VoteGenerators>,
    ledger: Arc<Ledger>,
    pub(crate) state: Arc<Mutex<RequestAggregatorState>>,
}

impl RequestAggregator {
    pub fn new(
        config: RequestAggregatorConfig,
        stats: Arc<Stats>,
        vote_generators: Arc<VoteGenerators>,
        ledger: Arc<Ledger>,
    ) -> Self {
        let max_queue = config.max_queue;
        Self {
            stats,
            vote_generators,
            ledger,
            config,
            state: Arc::new(Mutex::new(RequestAggregatorState {
                queue: FairQueue::new(move |_| max_queue, |_| 1),
                stopped: false,
            })),
        }
    }

    pub fn start(&self) {}

    pub fn request(&self, request: AggregatorRequest) -> bool {
        if request.roots_hashes.is_empty() {
            return false;
        }

        let request_len = request.roots_hashes.len();

        let added = {
            self.state
                .lock()
                .queue
                .push(request.channel.channel_id(), request)
        };

        if added {
            self.stats
                .inc(StatType::RequestAggregator, DetailType::Request);
            self.stats.add(
                StatType::RequestAggregator,
                DetailType::RequestHashes,
                request_len as u64,
            );
        } else {
            self.stats
                .inc(StatType::RequestAggregator, DetailType::Overfill);
            self.stats.add(
                StatType::RequestAggregator,
                DetailType::OverfillHashes,
                request_len as u64,
            );
        }

        // TODO: This stat is for compatibility with existing tests and is in principle unnecessary
        self.stats.inc(
            StatType::Aggregator,
            if added {
                DetailType::AggregatorAccepted
            } else {
                DetailType::AggregatorDropped
            },
        );

        added
    }

    pub fn stop(&self) {
        self.state.lock().stopped = true;
    }

    /// Returns the number of currently queued request pools
    pub fn len(&self) -> usize {
        self.state.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn run_once(&self) -> bool {
        let batch = {
            let mut state = self.state.lock();
            if state.stopped || state.queue.is_empty() {
                return false;
            }
            state.queue.next_batch(self.config.batch_size)
        };
        if batch.is_empty() {
            return false;
        }
        let mut any = self.ledger.any();
        for (_, request) in &batch {
            if any.should_refresh() {
                any = self.ledger.any();
            }
            if !request.channel.should_drop(TrafficType::VoteReply) {
                self.process(&any, request);
            } else {
                self.stats.inc_dir(
                    StatType::RequestAggregator,
                    DetailType::ChannelFull,
                    Direction::Out,
                );
            }
        }
        true
    }

    fn process(&self, any: &dyn AnySet, request: &AggregatorRequest) {
        let remaining = self.aggregate(any, request);

        if !remaining.remaining_normal.is_empty() {
            self.stats
                .inc(StatType::RequestAggregatorReplies, DetailType::NormalVote);

            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_normal,
                &request.channel,
                VoteType::NonFinal,
            );
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsCannotVote,
                Direction::In,
                (remaining.remaining_normal.len() - generated) as u64,
            );
        }

        if !remaining.remaining_final.is_empty() {
            self.stats
                .inc(StatType::RequestAggregatorReplies, DetailType::FinalVote);

            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_final,
                &request.channel,
                VoteType::Final,
            );
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsCannotVote,
                Direction::In,
                (remaining.remaining_final.len() - generated) as u64,
            );
        }
    }

    fn aggregate(&self, any: &dyn AnySet, requests: &AggregatorRequest) -> AggregateResult {
        let mut aggregator = RequestAggregatorImpl::new(&self.stats, any);
        aggregator.add_votes(&requests.roots_hashes);
        aggregator.get_result()
    }
}

impl ContainerInfoProvider for RequestAggregator {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.state.lock();
        ContainerInfo::builder()
            .node("queue", guard.queue.container_info())
            .finish()
    }
}

#[derive(Clone)]
pub struct AggregatorRequest {
    pub channel: Arc<Channel>,
    pub roots_hashes: Vec<(BlockHash, Root)>,
}

pub(crate) struct RequestAggregatorState {
    queue: FairQueue<ChannelId, AggregatorRequest>,
    stopped: bool,
}

pub(crate) struct RequestAggregatorCleanup {
    state: Arc<Mutex<RequestAggregatorState>>,
}

impl RequestAggregatorCleanup {
    pub(crate) fn new(state: Arc<Mutex<RequestAggregatorState>>) -> Self {
        Self { state }
    }
}

impl DeadChannelCleanupStep for RequestAggregatorCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut guard = self.state.lock();
        for channel_id in dead_channel_ids {
            guard.queue.remove(channel_id);
        }
    }
}
