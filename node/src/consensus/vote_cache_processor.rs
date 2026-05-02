use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::{ Mutex, MutexGuard };
use hashbrown::{ HashSet };
use crate::utils::{Condvar, JoinHandle};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    BlockHash, VoteSource,
};
use rsnano_stats::{DetailType, StatType, Stats};

use super::{VoteCache, VoteProcessorConfig, VoteProcessorQueue};

pub(crate) struct VoteCacheProcessor {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    stats: Arc<Stats>,
    vote_cache: Arc<Mutex<VoteCache>>,
    vote_queue: Arc<VoteProcessorQueue>,
    config: VoteProcessorConfig,
}

impl VoteCacheProcessor {
    pub(crate) fn new(
        stats: Arc<Stats>,
        vote_cache: Arc<Mutex<VoteCache>>,
        vote_queue: Arc<VoteProcessorQueue>,
        config: VoteProcessorConfig,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                thread: None,
                stopped: false,
                triggered: VecDeque::new(),
            })),
            condition: Arc::new(Condvar::new()),
            stats,
            vote_queue,
            vote_cache,
            config,
        }
    }
}

impl VoteCacheProcessor {
    pub fn start(&self) {
        // debug_assert!(self.state.lock().thread.is_none());
        // let cache_loop = VoteCacheLoop {
        //     state: self.state.clone(),
        //     condition: self.condition.clone(),
        //     stats: self.stats.clone(),
        //     vote_cache: self.vote_cache.clone(),
        //     vote_queue: self.vote_queue.clone(),
        // };
        //
        // self.state.lock().thread = Some(
        //     std::/* /* /* thread::Builder::new()
        //         .name("Vote cache proc".to_owned())
        //         .spawn(move || cache_loop.run())
        //         .unwrap(),
        // );
    }

    pub fn stop(&self) {
        {
            let mut state = self.state.lock();
            state.stopped = true;
            state.thread.take();
        }

        self.condition.notify_all();

        // if let Some(handle) = thread {
        //     handle.join().unwrap(); */ */ */
        // }
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
        }
        self.condition.notify_all();
        self.stats
            .inc(StatType::VoteCacheProcessor, DetailType::Triggered);
    }

    pub fn len(&self) -> usize {
        self.state.lock().triggered.len()
    }
}

impl Drop for VoteCacheProcessor {
    fn drop(&mut self) {
        // debug_assert!(self.state.lock().thread.is_none())
    }
}

impl ContainerInfoProvider for VoteCacheProcessor {
    fn container_info(&self) -> ContainerInfo {
        [("triggered", self.len(), core::mem::size_of::<BlockHash>())].into()
    }
}

struct State {
    thread: Option<JoinHandle<()>>,
    stopped: bool,
    triggered: VecDeque<BlockHash>,
}

#[allow(dead_code)]
struct VoteCacheLoop {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    stats: Arc<Stats>,
    vote_cache: Arc<Mutex<VoteCache>>,
    vote_queue: Arc<VoteProcessorQueue>,
}

impl VoteCacheLoop {
    #[allow(dead_code)]
    fn run(&self) {
        let mut guard = self.state.lock();
        while !guard.stopped {
            if !guard.triggered.is_empty() {
                self.run_batch(guard);
                guard = self.state.lock();
            } else {
                guard = self
                    .condition
                    .wait_while(guard, |i| !i.stopped && i.triggered.is_empty())
                    .unwrap();
            }
        }
    }

    #[allow(dead_code)]
    fn run_batch(&self, mut state: MutexGuard<'_, State>) {
        let mut triggered = VecDeque::new();
        core::mem::swap(&mut triggered, &mut state.triggered);
        drop(state);

        //deduplicate
        let hashes: HashSet<BlockHash> = triggered.drain(..).collect();

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
    }
}
