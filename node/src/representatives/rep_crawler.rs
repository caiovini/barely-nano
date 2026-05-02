use alloc::sync::Arc;
use spin::{Mutex, RwLock};
use core::time::Duration;
use alloc::vec::Vec;
use alloc::vec;
use core::ops::DerefMut;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::utils::BoundedVecDeque;
use tracing::{debug, info, warn};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, BlockHash, Root, Vote,
};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_messages::{ConfirmReq, Message};
use rsnano_network::{Channel, ChannelId, Network, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_stats::{DetailType, Direction, Sample, StatType, Stats};

use super::{InsertResult, OnlineReps};
use crate::{
    config::{NetworkParams, NodeConfig},
    consensus::{ActiveElectionsContainer, ReceivedVote},
    transport::{
        keepalive::{KeepalivePublisher, PreconfiguredPeersKeepalive},
        MessageSender,
    },
};

/// Crawls the network for representatives. Queries are performed by requesting confirmation of a
/// random block and observing the corresponding vote.
pub struct RepCrawler {
    rep_crawler_impl: Mutex<RepCrawlerImpl>,
    online_reps: Arc<Mutex<OnlineReps>>,
    stats: Arc<Stats>,
    config: NodeConfig,
    network_params: NetworkParams,
    network: Arc<RwLock<Network>>,
    ledger: Arc<Ledger>,
    steady_clock: Arc<SteadyClock>,
    message_sender: Mutex<MessageSender>,
    preconfigured_peers: Arc<PreconfiguredPeersKeepalive>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    keepalive_in_progress: AtomicBool,
}

impl RepCrawler {
    const MAX_RESPONSES: usize = 1024 * 4;

    pub(crate) fn new(
        online_reps: Arc<Mutex<OnlineReps>>,
        stats: Arc<Stats>,
        query_timeout: Duration,
        config: NodeConfig,
        network_params: NetworkParams,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        steady_clock: Arc<SteadyClock>,
        message_sender: MessageSender,
        keepalive_publisher: Arc<KeepalivePublisher>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    ) -> Self {
        let is_dev_network = network_params.network.is_dev_network();
        Self {
            online_reps: Arc::clone(&online_reps),
            stats: Arc::clone(&stats),
            config: config.clone(),
            network_params,
            network,
            ledger,
            steady_clock: Arc::clone(&steady_clock),
            message_sender: Mutex::new(message_sender),
            preconfigured_peers: {
                info!("Initializing RepCrawler with {} preconfigured peers", config.preconfigured_peers.len());
                Arc::new(PreconfiguredPeersKeepalive::new(
                    config.preconfigured_peers,
                    keepalive_publisher,
                ))
            },
            rep_crawler_impl: Mutex::new(RepCrawlerImpl {
                is_dev_network,
                queries: OrderedQueries::new(),
                online_reps,
                stats,
                query_timeout,
                stopped: false,
                last_query: None,
                responses: BoundedVecDeque::new(Self::MAX_RESPONSES),
                prioritized: Default::default(),
                clock: Arc::clone(&steady_clock),
            }),
            active_elections,
            keepalive_in_progress: AtomicBool::new(false),
        }
    }

    pub fn stop(&self) {
        let mut guard = self.rep_crawler_impl.lock();
        guard.stopped = true;
    }

    /// Called when a non-replay vote arrives that might be of interest to rep crawler.
    /// @return true, if the vote was of interest and was processed, this indicates that the rep is likely online and voting
    pub fn process(&self, vote: &ReceivedVote) -> bool {
        let Some(channel) = vote.channel.clone() else {
            return false;
        };
        let mut guard = self.rep_crawler_impl.lock();
        let mut processed = false;

        let query_timeout = guard.query_timeout;
        let x = guard.deref_mut();
        let queries = &mut x.queries;
        let responses = &mut x.responses;
        queries.modify_for_channel(channel.channel_id(), |query| {
            let target_hash = query.hash;
            let found = vote.hashes.iter().any(|h| *h == target_hash);
            let done;

            if found {
                debug!(
                    "Processing response for block: {} from: {}",
                    target_hash,
                    channel.peer_addr()
                );
                self.stats
                    .inc_dir(StatType::RepCrawler, DetailType::Response, Direction::In);

                self.stats.sample(
                    Sample::RepResponseTime,
                    self.steady_clock.now().elapsed(query.time).as_millis() as i64,
                    (0, query_timeout.as_millis() as i64),
                );

                responses.push_back(vote.clone());
                query.replies += 1;
                processed = true;
                done = true
            } else {
                done = false
            }

            done
        });

        processed
    }

    /// Attempt to determine if the peer manages one or more representative accounts
    pub fn query(&self, target_channels: Vec<Arc<Channel>>) {
        let hash_root = self.prepare_query_target();

        let mut guard = self.rep_crawler_impl.lock();

        for channel in target_channels {
            guard.track_rep_request(hash_root, channel.channel_id(), self.steady_clock.now());
            self.stats
                .inc_dir(StatType::RepCrawler, DetailType::QuerySent, Direction::In);

            let req = Message::ConfirmReq(ConfirmReq::new(vec![hash_root]));

            self.message_sender
                .lock()
                .try_send(&channel, &req, TrafficType::RepCrawler);
        }
    }

    /// Attempt to determine if the peer manages one or more representative accounts
    pub fn query_with_priority(&self, target_channel: Arc<Channel>) {
        let mut guard = self.rep_crawler_impl.lock();
        guard.prioritized.push(target_channel);
    }

    pub fn force_process2(&self, vote: ReceivedVote) {
        assert!(self.network_params.network.is_dev_network());
        let mut guard = self.rep_crawler_impl.lock();
        guard.responses.push_back(vote);
    }

    pub fn force_query(&self, hash: BlockHash, channel_id: ChannelId) {
        assert!(self.network_params.network.is_dev_network());
        let mut guard = self.rep_crawler_impl.lock();
        guard.queries.insert(QueryEntry {
            hash,
            channel_id,
            time: self.steady_clock.now(),
            replies: 0,
        })
    }

    pub fn run(&self) {
        // tracing::info!("[rep_crawler] run() called");
        let mut guard = self.rep_crawler_impl.lock();
        if guard.stopped {
            return;
        }

        let current_total_weight;
        let sufficient_weight;
        {
            let reps = self.online_reps.lock();
            current_total_weight = reps.peered_weight();
            sufficient_weight = current_total_weight > reps.quorum_delta();
        }

        // If online weight drops below minimum, reach out to preconfigured peers
        if !sufficient_weight {
            if !self.keepalive_in_progress.swap(true, Ordering::SeqCst) {
                info!("[rep_crawler] Insufficient weight, calling keepalive");
                self.stats
                    .inc_dir(StatType::RepCrawler, DetailType::Keepalive, Direction::In);
                let preconfigured = self.preconfigured_peers.clone();
                info!("[rep_crawler] Spawning keepalive task");
                let _in_progress = Arc::new(&self.keepalive_in_progress);
                // unsafe: we know 'self' lives as long as the node
                let self_ptr = self as *const RepCrawler as usize;
                kernel_common::async_rt::spawn(async move {
                    let this = unsafe { &*(self_ptr as *const RepCrawler) };
                    tracing::info!("[node] Keepalive task started");
                    preconfigured.keepalive().await;
                    tracing::info!("[node] Keepalive task finished");
                    this.keepalive_in_progress.store(false, Ordering::SeqCst);
                });
            }
        }

        let interval = self.query_interval(sufficient_weight);
        
        self.stats
            .inc_dir(StatType::RepCrawler, DetailType::Loop, Direction::In);

        if !guard.responses.is_empty() {
            self.validate_and_process(guard);
            guard = self.rep_crawler_impl.lock();
        }

        guard.cleanup();

        if !guard.prioritized.is_empty() {
            let mut prioritized_l = Vec::new();
            core::mem::swap(&mut prioritized_l, &mut guard.prioritized);
            drop(guard);
            self.query(prioritized_l);
            guard = self.rep_crawler_impl.lock();
        }

        if guard.query_predicate(interval, self.steady_clock.now()) {
            guard.last_query = Some(self.steady_clock.now());
            drop(guard);

            const CONSERVATIVE_COUNT: usize = 160;
            const AGGRESSIVE_COUNT: usize = 160;

            let required_peer_count = if sufficient_weight {
                CONSERVATIVE_COUNT
            } else {
                AGGRESSIVE_COUNT
            };

            let mut random_peers = self
                .network
                .read()
                .shuffled_channels(TrafficType::RepCrawler);
            random_peers.truncate(required_peer_count);

            guard = self.rep_crawler_impl.lock();
            let targets = guard.prepare_crawl_targets(
                sufficient_weight,
                random_peers,
                self.steady_clock.now(),
            );
            drop(guard);
            self.query(targets);
            let loopback = self.network.read().loopback().clone();
            self.query(vec![loopback]);
        }
    }

    fn validate_and_process(&self, mut guard: spin::MutexGuard<RepCrawlerImpl>) {
        let mut responses = BoundedVecDeque::new(Self::MAX_RESPONSES);
        core::mem::swap(&mut guard.responses, &mut responses);
        drop(guard);

        let minimum = core::cmp::min(
            self.online_reps.lock().minimum_principal_weight(),
            self.config.rep_crawler_weight_minimum,
        );

        for vote in responses.iter() {
            let Some(channel) = vote.channel.clone() else {
                continue;
            };
            let rep_weight = self.ledger.weight(&vote.voter);
            if rep_weight < minimum {
                debug!(
                    "Ignoring vote from account: {} with too little voting weight: {}",
                    Account::from(vote.voter).encode_account(),
                    rep_weight.to_string_dec()
                );
                continue;
            }

            let result = self.online_reps.lock().vote_observed_directly(
                vote.voter,
                channel.clone(),
                self.steady_clock.now(),
            );

            match result {
                InsertResult::Inserted => {
                    info!(
                        "Found representative: {} at {}",
                        Account::from(vote.voter).encode_account(),
                        channel.peer_addr()
                    );
                }
                InsertResult::ChannelChanged(previous_peer) => {
                    warn!(
                        "Updated representative: {} at : {} (was at: {})",
                        Account::from(vote.voter).encode_account(),
                        channel.peer_addr(),
                        previous_peer
                    )
                }
                InsertResult::Updated => {}
            }
        }
    }

    fn prepare_query_target(&self) -> (BlockHash, Root) {
        const MAX_ATTEMPTS: usize = 32;
        let any = self.ledger.any();
        let random_blocks = any.random_blocks(MAX_ATTEMPTS);

        for block in &random_blocks {
            if !any.confirmed().block_exists(&block.hash()) {
                continue;
            }

            if self
                .active_elections
                .read()
                .was_recently_confirmed(&block.hash())
            {
                continue;
            }

            return (block.hash(), block.root());
        }

        (
            self.network_params.ledger.genesis_block.hash(),
            self.network_params.ledger.genesis_block.root(),
        )
    }

    fn query_interval(&self, sufficient_weight: bool) -> Duration {
        if sufficient_weight {
            self.network_params.network.rep_crawler_normal_interval
        } else {
            self.network_params.network.rep_crawler_warmup_interval
        }
    }
}

impl ContainerInfoProvider for RepCrawler {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.rep_crawler_impl.lock();
        [
            ("queries", guard.queries.len(), OrderedQueries::ELEMENT_SIZE),
            (
                "responses",
                guard.responses.len(),
                core::mem::size_of::<Arc<Vote>>() * 2,
            ),
            ("prioritized", guard.prioritized.len(), 0),
        ]
        .into()
    }
}

struct RepCrawlerImpl {
    queries: OrderedQueries,
    online_reps: Arc<Mutex<OnlineReps>>,
    stats: Arc<Stats>,
    query_timeout: Duration,
    stopped: bool,
    last_query: Option<Timestamp>,
    responses: BoundedVecDeque<ReceivedVote>,
    prioritized: Vec<Arc<Channel>>,
    is_dev_network: bool,
    clock: Arc<SteadyClock>,
}

impl RepCrawlerImpl {
    fn query_predicate(&self, query_interval: Duration, now: Timestamp) -> bool {
        match &self.last_query {
            Some(last) => now - *last >= query_interval,
            None => true,
        }
    }

    fn prepare_crawl_targets(
        &self,
        sufficient_weight: bool,
        mut random_peers: Vec<Arc<Channel>>,
        now: Timestamp,
    ) -> Vec<Arc<Channel>> {
        const CONSERVATIVE_MAX_ATTEMPTS: usize = 4;
        const AGGRESSIVE_MAX_ATTEMPTS: usize = 8;

        let rep_query_interval = if self.is_dev_network {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(60)
        };

        self.stats.inc_dir(
            StatType::RepCrawler,
            if sufficient_weight {
                DetailType::CrawlNormal
            } else {
                DetailType::CrawlAggressive
            },
            Direction::In,
        );

        random_peers.retain(|channel| {
            let elapsed = self
                .online_reps
                .lock()
                .last_request_elapsed(channel.channel_id(), now);

            match elapsed {
                Some(last_request_elapsed) => {
                    last_request_elapsed >= rep_query_interval
                }
                None => {
                    let max_attemts = if sufficient_weight {
                        CONSERVATIVE_MAX_ATTEMPTS
                    } else {
                        AGGRESSIVE_MAX_ATTEMPTS
                    };
                    self.queries.count_by_channel(channel.channel_id()) < max_attemts
                }
            }
        });

        random_peers
    }

    fn track_rep_request(
        &mut self,
        hash_root: (BlockHash, Root),
        channel_id: ChannelId,
        now: Timestamp,
    ) {
        self.queries.insert(QueryEntry {
            hash: hash_root.0,
            channel_id,
            time: now,
            replies: 0,
        });
        self.online_reps
            .lock()
            .on_rep_request(channel_id, now);
    }

    fn cleanup(&mut self) {
        let now = self.clock.now();
        self.queries.retain(|query| {
            now.elapsed(query.time) < self.query_timeout
        });
    }
}

struct QueryEntry {
    hash: BlockHash,
    channel_id: ChannelId,
    time: Timestamp,
    replies: usize,
}

struct OrderedQueries {
    entries: hashbrown::HashMap<usize, QueryEntry>,
    sequenced: Vec<usize>,
    by_channel: hashbrown::HashMap<ChannelId, Vec<usize>>,
    by_hash: hashbrown::HashMap<BlockHash, Vec<usize>>,
    next_id: usize,
}

impl OrderedQueries {
    fn new() -> Self {
        Self {
            entries: hashbrown::HashMap::new(),
            sequenced: Vec::new(),
            by_channel: hashbrown::HashMap::new(),
            by_hash: hashbrown::HashMap::new(),
            next_id: 1,
        }
    }

    pub const ELEMENT_SIZE: usize =
        core::mem::size_of::<QueryEntry>() + core::mem::size_of::<BlockHash>() + core::mem::size_of::<usize>() * 3;

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn insert(&mut self, entry: QueryEntry) {
        let entry_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.sequenced.push(entry_id);
        self.by_channel
            .entry(entry.channel_id)
            .or_default()
            .push(entry_id);
        self.by_hash.entry(entry.hash).or_default().push(entry_id);
        self.entries.insert(entry_id, entry);
    }

    fn retain(&mut self, mut predicate: impl FnMut(&QueryEntry) -> bool) {
        let mut to_delete = Vec::new();
        for (&id, entry) in &self.entries {
            if !predicate(entry) {
                to_delete.push(id);
            }
        }
        for id in to_delete {
            self.remove(id);
        }
    }

    fn remove(&mut self, entry_id: usize) {
        if let Some(entry) = self.entries.remove(&entry_id) {
            self.sequenced.retain(|id| *id != entry_id);
            if let Some(mut by_channel) = self.by_channel.remove(&entry.channel_id) {
                if by_channel.len() > 1 {
                    by_channel.retain(|i| *i != entry_id);
                    self.by_channel.insert(entry.channel_id, by_channel);
                }
            }
            if let Some(mut by_hash) = self.by_hash.remove(&entry.hash) {
                if by_hash.len() > 1 {
                    by_hash.retain(|i| *i != entry_id);
                    self.by_hash.insert(entry.hash, by_hash);
                }
            }
        }
    }

    fn count_by_channel(&self, channel_id: ChannelId) -> usize {
        self.by_channel
            .get(&channel_id)
            .map(|i| i.len())
            .unwrap_or_default()
    }

    fn modify_for_channel(
        &mut self,
        channel_id: ChannelId,
        mut f: impl FnMut(&mut QueryEntry) -> bool,
    ) {
        if let Some(ids) = self.by_channel.get(&channel_id) {
            for id in ids {
                if let Some(entry) = self.entries.get_mut(id) {
                    let done = f(entry);
                    if done {
                        return;
                    }
                }
            }
        }
    }
}

pub trait RepCrawlerExt {
    fn start(&self);
}

impl RepCrawlerExt for Arc<RepCrawler> {
    fn start(&self) {
        let this = self.clone();
        kernel_common::async_rt::spawn(async move {
            loop {
                this.run();
                kernel_common::async_rt::sleep(Duration::from_secs(3)).await;
            }
        });
    }
}
