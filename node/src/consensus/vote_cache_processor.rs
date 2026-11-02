use alloc::{collections::VecDeque, sync::Arc};
use hashbrown::HashSet;
use spin::Mutex;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    BlockHash, VoteSource,
};
use rsnano_stats::{DetailType, StatType, Stats};

use super::{VoteCache, VoteProcessorConfig, VoteProcessorQueue};

pub struct VoteCacheProcessor {
    state: Arc<Mutex<State>>,
    pending_count: core::sync::atomic::AtomicUsize,
    stats: Arc<Stats>,
    vote_cache: Arc<Mutex<VoteCache>>,
    vote_queue: Arc<VoteProcessorQueue>,
    config: VoteProcessorConfig,
}

impl VoteCacheProcessor {
    pub fn new(
        stats: Arc<Stats>,
        vote_cache: Arc<Mutex<VoteCache>>,
        vote_queue: Arc<VoteProcessorQueue>,
        config: VoteProcessorConfig,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                stopped: false,
                triggered: VecDeque::new(),
            })),
            pending_count: core::sync::atomic::AtomicUsize::new(0),
            stats,
            vote_queue,
            vote_cache,
            config,
        }
    }

    pub fn start(&self) {
        self.state.lock().stopped = false;
    }

    #[inline]
    pub fn is_empty_relaxed(&self) -> bool {
        self.pending_count.load(core::sync::atomic::Ordering::Relaxed) == 0
    }

    pub fn run_once(&self) -> bool {
        if self.is_empty_relaxed() {
            return false;
        }
        let triggered = {
            let Some(mut state) = self.state.try_lock() else {
                return false;
            };
            if state.stopped || state.triggered.is_empty() {
                self.pending_count
                    .store(0, core::sync::atomic::Ordering::Relaxed);
                return false;
            }
            let mut triggered = VecDeque::new();
            core::mem::swap(&mut triggered, &mut state.triggered);
            self.pending_count
                .store(0, core::sync::atomic::Ordering::Relaxed);
            triggered
        };

        let hashes: HashSet<BlockHash> = triggered.into_iter().collect();

        self.stats.add(
            StatType::VoteCacheProcessor,
            DetailType::Processed,
            hashes.len() as u64,
        );

        for hash in hashes {
            let cached = self.vote_cache.lock().find(&hash);
            for cached_vote in cached {
                self.vote_queue
                    .enqueue(cached_vote, None, VoteSource::Cache, Some(hash));
            }
        }
        true
    }

    pub fn stop(&self) {
        self.state.lock().stopped = true;
    }

    pub fn trigger(&self, hash: BlockHash) {
        {
            let mut state = self.state.lock();
            if state.triggered.len() > self.config.max_triggered {
                state.triggered.pop_front();
                self.stats
                    .inc(StatType::VoteCacheProcessor, DetailType::Overfill);
            }
            state.triggered.push_back(hash);
            self.pending_count
                .store(state.triggered.len(), core::sync::atomic::Ordering::Release);
        }
        self.stats
            .inc(StatType::VoteCacheProcessor, DetailType::Triggered);
    }

    pub fn len(&self) -> usize {
        self.state.lock().triggered.len()
    }
}

impl ContainerInfoProvider for VoteCacheProcessor {
    fn container_info(&self) -> ContainerInfo {
        [("triggered", self.len(), core::mem::size_of::<BlockHash>())].into()
    }
}

struct State {
    stopped: bool,
    triggered: VecDeque<BlockHash>,
}
