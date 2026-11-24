use crate::utils::JoinHandle;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::{max, min};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use hashbrown::HashMap;
use spin::Mutex;

use rsnano_core::{BlockHash, Vote, VoteError, VoteSource, utils::BackpressureSender};
use rsnano_network::{Channel, ChannelId};
use rsnano_stats::{DetailType, StatType, Stats};

use super::{AecEvent, FilteredVote, ReceivedVote, RepTier, VoteApplier, VoteProcessorQueue};

type QueuedVoteItem = (
    (RepTier, ChannelId),
    (
        Arc<Vote>,
        VoteSource,
        Option<Arc<Channel>>,
        Option<BlockHash>,
    ),
);

const MAX_PREVERIFIED_VOTES: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct VoteProcessorConfig {
    pub max_pr_queue: usize,
    pub max_non_pr_queue: usize,
    pub pr_priority: usize,
    pub threads: usize,
    pub batch_size: usize,
    pub max_triggered: usize,
}

impl VoteProcessorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            max_pr_queue: 2048,
            max_non_pr_queue: 2048,
            pr_priority: 3,
            threads: max(1, min(4, parallelism / 2)),
            batch_size: 1024,
            max_triggered: 16384,
        }
    }
}

pub type VoteProcessedCallback2 =
    Box<dyn Fn(&Arc<Vote>, Option<&Arc<Channel>>, VoteSource, VoteError) + Send + Sync>;

pub struct VoteProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    queue: Arc<VoteProcessorQueue>,
    verified_queue: Mutex<VecDeque<QueuedVoteItem>>,
    verified_count: AtomicUsize,
    vote_applier: VoteApplier,
    stats: Arc<Stats>,
    pub total_processed: AtomicU64,
    cool_down: AtomicBool,
}

impl VoteProcessor {
    pub(crate) fn new(
        queue: Arc<VoteProcessorQueue>,
        vote_applier: VoteApplier,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            queue,
            verified_queue: Mutex::new(VecDeque::with_capacity(1024)),
            verified_count: AtomicUsize::new(0),
            vote_applier,
            stats,
            threads: Mutex::new(Vec::new()),
            total_processed: AtomicU64::new(0),
            cool_down: AtomicBool::new(false),
        }
    }

    pub fn add_observer(&self, sink: BackpressureSender<AecEvent>) {
        self.vote_applier.add_event_sink(sink);
    }

    pub fn cool_down(&self) {
        self.cool_down.store(true, Ordering::Relaxed);
    }

    pub fn recovered(&self) {
        self.cool_down.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.vote_applier.stop();
        self.queue.stop();
        {
            let mut vq = self.verified_queue.lock();
            vq.clear();
            self.verified_count.store(0, Ordering::Relaxed);
        }

        let mut _handles = Vec::new();
        {
            let mut guard = self.threads.lock();
            core::mem::swap(&mut _handles, &mut guard);
        }
    }

    /// Lock-free check used by SpacemiT K3 A100 crypto worker harts (`Harts 8..14`) to see
    /// whether raw incoming votes are waiting for 16-way `VLEN=1024` signature verification.
    #[inline]
    pub fn has_unverified_work(&self) -> bool {
        !self.queue.is_empty_relaxed()
            && !self.cool_down.load(Ordering::Relaxed)
            && self.verified_count.load(Ordering::Relaxed) < MAX_PREVERIFIED_VOTES
    }

    /// Pops up to 16 (on A100 `VLEN=1024`) or 4 prioritized votes from `VoteProcessorQueue`,
    /// verifies their Blake2b digests and Ed25519 signatures in SIMD lockstep outside queue locks,
    /// and forwards them to `verified_queue` for zero-crypto consensus application on Hart 0.
    pub fn help_verify(&self) -> usize {
        if !self.has_unverified_work() {
            return 0;
        }

        let batch_cap = if rsnano_sha::blake2b::has_vlen_1024() {
            16
        } else {
            4
        };

        let chunk = {
            let Some(mut guard) = self.queue.data.try_lock() else {
                return 0;
            };
            if guard.stopped || guard.queue.is_empty() {
                self.queue
                    .pending_count
                    .store(guard.queue.len(), Ordering::Relaxed);
                return 0;
            }
            let c = guard.queue.next_batch(batch_cap);
            self.queue
                .pending_count
                .store(guard.queue.len(), Ordering::Relaxed);
            c
        };

        let count = chunk.len();
        if count == 0 {
            return 0;
        }

        {
            let vote_refs: Vec<&Vote> = chunk
                .iter()
                .map(|(_, (vote, _, _, _))| vote.as_ref())
                .collect();
            Vote::verify_batch(&vote_refs);
        }

        {
            let mut vq = self.verified_queue.lock();
            vq.extend(chunk);
            self.verified_count.store(vq.len(), Ordering::Release);
        }

        count
    }

    pub fn run(&self) {
        loop {
            if self.cool_down.load(Ordering::Relaxed) {
                if self.queue.stopped() {
                    return;
                }
                core::hint::spin_loop();
                continue;
            }

            self.stats.inc(StatType::VoteProcessor, DetailType::Loop);

            let batch = self.queue.wait_for_votes(self.queue.config.batch_size);
            if batch.is_empty() {
                break; //stopped
            }

            self.handle_batch(batch);
        }
    }

    pub fn run_once(&self) -> bool {
        if self.cool_down.load(Ordering::Relaxed) {
            return false;
        }
        if self.verified_count.load(Ordering::Relaxed) == 0 && self.queue.is_empty_relaxed() {
            return false;
        }

        let max_batch = self.queue.config.batch_size;
        let mut batch = VecDeque::new();

        if self.verified_count.load(Ordering::Acquire) > 0 {
            if let Some(mut vq) = self.verified_queue.try_lock() {
                let n = vq.len().min(max_batch);
                batch.extend(vq.drain(..n));
                self.verified_count.store(vq.len(), Ordering::Relaxed);
            }
        }

        if batch.len() < max_batch && !self.queue.is_empty_relaxed() {
            if let Some(mut guard) = self.queue.data.try_lock() {
                if !guard.stopped && !guard.queue.is_empty() {
                    let rem = max_batch - batch.len();
                    let extra = guard.queue.next_batch(rem);
                    self.queue
                        .pending_count
                        .store(guard.queue.len(), Ordering::Relaxed);
                    batch.extend(extra);
                } else {
                    self.queue
                        .pending_count
                        .store(guard.queue.len(), Ordering::Relaxed);
                }
            }
        }

        if !batch.is_empty() {
            self.handle_batch(batch);
            true
        } else {
            false
        }
    }

    fn handle_batch(&self, batch: VecDeque<QueuedVoteItem>) {
        if batch
            .iter()
            .any(|(_, (vote, _, _, _))| !vote.is_signature_checked())
        {
            let refs: Vec<&Vote> = batch
                .iter()
                .map(|(_, (vote, _, _, _))| vote.as_ref())
                .collect();
            Vote::verify_batch(&refs);
        }

        let total = batch.len() as u64;
        let mut valid_votes = Vec::with_capacity(batch.len());
        for (_, (vote, source, channel, filter)) in batch {
            if vote.validate().is_ok() {
                let filter: BlockHash = filter.unwrap_or_default();
                let received_vote = ReceivedVote::new(vote, source, channel);
                valid_votes.push(FilteredVote::new(received_vote, filter));
            }
        }

        if !valid_votes.is_empty() {
            self.vote_applier.vote_batch(&valid_votes);
        }

        self.total_processed.fetch_add(total, Ordering::SeqCst);
    }

    pub fn vote_blocking(&self, vote: &FilteredVote) -> Result<(), VoteError> {
        let mut result = Err(VoteError::Invalid);
        if vote.validate().is_ok() {
            let vote_results = self.vote_applier.vote(vote);
            result = aggregate_vote_results(&vote_results);
        }
        result
    }
}

impl Drop for VoteProcessor {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        // debug_assert!(self.threads.lock().is_empty());
    }
}

pub trait VoteProcessorExt {
    fn start(&self);
}

impl VoteProcessorExt for Arc<VoteProcessor> {
    fn start(&self) {
        // let mut threads = self.threads.lock();
        // debug_assert!(threads.is_empty());
        // for _ in 0..self.queue.config.threads {
        //     let self_l = Arc::clone(self);
        //     threads.push(
        //         std::thread::Builder::new()
        //             .name("Vote processing".to_string())
        //             .spawn(Box::new(move || {
        //                 self_l.run();
        //             }))
        //             .unwrap(),
        //     )
        // }
    }
}

// Aggregate results for individual hashes
pub fn aggregate_vote_results(
    results: &HashMap<BlockHash, Result<(), VoteError>>,
) -> Result<(), VoteError> {
    let mut ignored = false;
    let mut replay = false;
    let mut processed = false;
    let mut late = false;
    for (_, res) in results {
        ignored |= matches!(res, Err(VoteError::Ignored));
        replay |= matches!(res, Err(VoteError::Replay));
        processed |= res.is_ok();
        late |= matches!(res, Err(VoteError::Late));
    }
    if ignored {
        Err(VoteError::Ignored)
    } else if replay {
        Err(VoteError::Replay)
    } else if processed {
        Ok(())
    } else if late {
        Err(VoteError::Late)
    } else {
        Err(VoteError::Indeterminate)
    }
}
