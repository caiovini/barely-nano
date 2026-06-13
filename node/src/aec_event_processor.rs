use crate::utils::SyncSender;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, RwLock};

use tracing::debug;

use rsnano_core::{Block, VoteError, VoteSource, utils::MemoryStream};
use rsnano_messages::NetworkFilter;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{Sample, Stats};

use crate::{
    NodeEvent,
    block_processing::{BlockContext, BlockProcessorQueue, BlockSource},
    cementation::ConfirmingSet,
    consensus::{
        ActiveElectionsContainer, AecCooldownReason, AecEvent, BootstrapElectionActivator,
        ForkProcessor, LocalVotesRemover, ReceivedVote, VoteCache, VoteCacheProcessor,
        VoteProcessor, VoteRebroadcastQueue, WinnerBlockBroadcaster, aggregate_vote_results,
        election_schedulers::ElectionSchedulers,
    },
    recently_cemented_inserter::RecentlyCementedInserter,
    representatives::{OnlineReps, RepCrawler},
    utils::BackpressureEventProcessor,
};

pub(crate) trait AecEventHandler {
    fn handle(&mut self, event: &AecEvent);
}

/// Processes events from the active election container (AEC)
pub(crate) struct AecEventProcessor {
    pub(crate) vote_cache_processor: Arc<VoteCacheProcessor>,
    pub(crate) vote_processor: Arc<VoteProcessor>,
    pub(crate) vote_cache: Arc<Mutex<VoteCache>>,
    pub(crate) node_observer: Option<SyncSender<NodeEvent>>,
    pub(crate) election_schedulers: Arc<ElectionSchedulers>,
    pub(crate) network_filter: Arc<NetworkFilter>,
    pub(crate) bootstrap_election_activator: BootstrapElectionActivator,
    pub(crate) recently_cemented_inserter: RecentlyCementedInserter,
    pub(crate) vote_rebroadcast_queue: Arc<VoteRebroadcastQueue>,
    pub(crate) block_processor_queue: Arc<BlockProcessorQueue>,
    pub(crate) confirming_set: Arc<ConfirmingSet>,
    pub(crate) online_reps: Arc<Mutex<OnlineReps>>,
    pub(crate) active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    pub(crate) rep_crawler: Arc<RepCrawler>,
    pub(crate) clock: Arc<SteadyClock>,
    pub(crate) local_votes_remover: LocalVotesRemover,
    pub(crate) stats: Arc<Stats>,
    pub(crate) fork_processor: Arc<ForkProcessor>,
    pub(crate) winner_block_broadcaster: Arc<Mutex<WinnerBlockBroadcaster>>,
    pub(crate) plugins: Vec<Box<dyn AecEventHandler + Send>>,
}

impl BackpressureEventProcessor<AecEvent> for AecEventProcessor {
    fn cool_down(&mut self) {
        self.active_elections
            .write()
            .set_cooldown(true, AecCooldownReason::AecEventQueueFull);
        self.vote_processor.cool_down();
    }

    fn recovered(&mut self) {
        self.active_elections
            .write()
            .set_cooldown(false, AecCooldownReason::AecEventQueueFull);
        self.vote_processor.recovered();
    }

    fn process(&mut self, event: AecEvent) {
        tracing::debug!("AecEventProcessor: processing event {:?}", event);
        for plugin in &mut self.plugins {
            plugin.handle(&event)
        }

        match event {
            AecEvent::ElectionStarted(hash, root) => {
                self.fork_processor.try_add_cached_forks(&root);
                self.bootstrap_election_activator.election_started(hash);
                self.vote_cache_processor.trigger(hash);
                if let Some(tx) = &self.node_observer {
                    let _ = tx.send(NodeEvent::ElectionStarted(hash));
                }
            }
            AecEvent::ElectionConfirmed(election) => {
                self.confirming_set.add(election.clone());
                // Ensure election winner is broadcasted
                self.winner_block_broadcaster
                    .lock()
                    .try_broadcast_winner(&election.winner, &election.votes);
            }
            AecEvent::ElectionEnded(election) => {
                self.election_schedulers.notify();

                let now = self.clock.now();
                let elapsed = election.start().elapsed(now);
                // Track election duration
                self.stats.sample(
                    Sample::ActiveElectionDuration,
                    elapsed.as_millis() as i64,
                    (0, 1000 * 60 * 10),
                ); // 0-10 minutes range

                for (hash, block) in election.candidate_blocks() {
                    // Notify observers about dropped elections & blocks lost confirmed elections
                    if !election.is_confirmed() || *hash != election.winner().hash() {
                        if let Some(tx) = &self.node_observer {
                            let _ = tx.send(NodeEvent::ElectionStopped(*hash));
                        }
                    }

                    if !election.is_confirmed() {
                        self.clear_network_filter(block);
                    }
                }
            }
            AecEvent::BlockAddedToElection(hash) => self.vote_cache_processor.trigger(hash),
            AecEvent::BlockDiscarded(block) => {
                self.clear_network_filter(&block);
            }
            AecEvent::WinnerChanged(previous_winner, new_winner) => {
                debug!(from = ?previous_winner, to = ?new_winner.hash(), "Winning fork changed");
                self.local_votes_remover
                    .remove_local_votes(&previous_winner, &new_winner.qualified_root());

                // Roll back the previous winner and add the new winner to the ledger
                self.block_processor_queue.push(BlockContext::new(
                    new_winner.clone(),
                    BlockSource::Forced,
                    ChannelId::LOOPBACK,
                ));
            }
            AecEvent::VoteProcessed(vote, voter_weight, results) => {
                // Cache the votes that didn't match any election
                if vote.source != VoteSource::Cache {
                    self.vote_cache
                        .lock()
                        .insert(&vote.vote, voter_weight, &results);
                }

                self.vote_rebroadcast_queue
                    .try_enqueue(&vote.vote, &results);

                let result = aggregate_vote_results(&results);
                tracing::debug!("AecEventProcessor: vote processed from {} source {:?} result {:?}", vote.voter.as_account().encode_account(), vote.source, result);
                self.try_update_online_reps(&vote, result);

                if let Some(tx) = &self.node_observer {
                    let _ = tx.send(NodeEvent::VoteProcessed(vote.vote, result));
                }
            }
            AecEvent::BlockConfirmed(block, election) => {
                if let Some(tx) = &self.node_observer {
                    let _ = tx.send(NodeEvent::BlockConfirmed(block, election.clone()));
                }
                self.recently_cemented_inserter.insert(election);
            }
            AecEvent::Recovered => self.election_schedulers.notify(),
        }
    }
}

impl AecEventProcessor {
    #[allow(dead_code)]
    pub fn add(&mut self, handler: impl AecEventHandler + Send + 'static) {
        self.plugins.push(Box::new(handler));
    }

    fn clear_network_filter(&mut self, block: &Block) {
        let mut buf = MemoryStream::new();
        block.serialize_without_block_type(&mut buf);
        self.network_filter.clear_bytes(buf.as_bytes());
    }

    fn try_update_online_reps(&mut self, vote: &ReceivedVote, result: Result<(), VoteError>) {
        // Track rep weight voting on live elections
        let mut should_observe = matches!(
            result,
            Ok(())
                | Err(VoteError::Replay)
                | Err(VoteError::Ignored)
                | Err(VoteError::Indeterminate)
        );

        // Ignore republished votes when rep crawling
        if vote.source == VoteSource::Live || vote.source == VoteSource::Rebroadcast {
            let processed = self.rep_crawler.process(vote);
            if processed {
                tracing::debug!("AecEventProcessor: vote processed by RepCrawler from {}", vote.voter.as_account().encode_account());
            }
            should_observe |= processed;
        }

        if should_observe {
            // Representative is defined as online if replying to live votes or rep_crawler queries
            self.online_reps
                .lock()
                .vote_observed(vote.voter, self.clock.now());
        }
    }
}
