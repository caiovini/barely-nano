use core::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use spin::Mutex;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    DifficultyV1, Root, WorkNonce,
};

use super::{CpuWorkGenerator, WorkRng, XorShift1024Star};

/// Number of 16-lane `pow_search_16` iterations per worker chunk (`64 * 16 = 1,024` nonces).
const A100_POW_CHUNK_ITERS: u32 = 64;
const A100_POW_CHUNK_NONCES: u64 = (A100_POW_CHUNK_ITERS as u64) * 16;

struct SharedWorkJob {
    epoch: u64,
    root: Root,
    min_difficulty: u64,
    winner: Option<WorkNonce>,
    cancelled: bool,
}

static WORK_ACTIVE_EPOCH: AtomicU64 = AtomicU64::new(0);
static WORK_NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
static WORK_NEXT_NONCE: AtomicU64 = AtomicU64::new(0);
static WORK_SLOT: Mutex<Option<SharedWorkJob>> = Mutex::new(None);

/// Called by idle SpacemiT K3 A100 crypto worker harts (`Harts 8..14`, `VLEN=1024`) to
/// cooperatively solve any active `WorkPool::generate` request in lock-free 1,024-nonce strides.
/// Returns `true` if a work chunk was executed.
pub fn try_help_work() -> bool {
    let epoch = WORK_ACTIVE_EPOCH.load(Ordering::Acquire);
    if epoch == 0 {
        return false;
    }

    let (root, min_difficulty) = {
        let Some(guard) = WORK_SLOT.try_lock() else {
            return false;
        };
        let Some(job) = guard.as_ref() else {
            return false;
        };
        if job.epoch != epoch || job.winner.is_some() || job.cancelled {
            return false;
        }
        (job.root, job.min_difficulty)
    };

    let start_nonce = WORK_NEXT_NONCE.fetch_add(A100_POW_CHUNK_NONCES, Ordering::Relaxed);
    if let Some(found) =
        DifficultyV1::pow_search_16(&root, start_nonce, min_difficulty, A100_POW_CHUNK_ITERS)
    {
        let mut guard = WORK_SLOT.lock();
        if let Some(job) = guard.as_mut() {
            if job.epoch == epoch && job.winner.is_none() {
                job.winner = Some(found);
                WORK_ACTIVE_EPOCH.store(0, Ordering::Release);
            }
        }
    }
    true
}

#[inline]
pub fn has_active_work() -> bool {
    WORK_ACTIVE_EPOCH.load(Ordering::Relaxed) != 0
}

pub struct WorkPoolBuilder {
    cpu_thread_count: Option<usize>,
    cpu_rate_limiter: Option<Duration>,
}

impl WorkPoolBuilder {
    pub fn threads(mut self, count: usize) -> Self {
        self.cpu_thread_count = Some(count);
        self
    }

    pub fn one_cpu_only(mut self) -> Self {
        self.cpu_thread_count = Some(1);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.cpu_thread_count = Some(0);
        self
    }

    pub fn cpu_rate_limit(mut self, limit: Duration) -> Self {
        self.cpu_rate_limiter = Some(limit);
        self
    }

    pub fn finish(self) -> WorkPool {
        let thread_count = self.cpu_thread_count.unwrap_or(1);
        let cpu_rate_limiter = self.cpu_rate_limiter.unwrap_or(Duration::ZERO);

        WorkPool::new(thread_count, cpu_rate_limiter)
    }
}

pub struct WorkPool {
    cpu_rate_limiter: Duration,
    thread_count: usize,
}

impl WorkPool {
    fn new(thread_count: usize, cpu_rate_limiter: Duration) -> Self {
        Self {
            cpu_rate_limiter,
            thread_count,
        }
    }

    pub fn builder() -> WorkPoolBuilder {
        WorkPoolBuilder {
            cpu_thread_count: None,
            cpu_rate_limiter: None,
        }
    }

    pub fn new_null(_configured_work: WorkNonce) -> Self {
        Self {
            cpu_rate_limiter: Duration::ZERO,
            thread_count: 1,
        }
    }

    pub fn disabled() -> Self {
        Self {
            cpu_rate_limiter: Duration::ZERO,
            thread_count: 0,
        }
    }

    pub fn work_generation_enabled(&self) -> bool {
        self.thread_count > 0
    }

    pub fn cancel(&self, root: &Root) {
        let mut guard = WORK_SLOT.lock();
        if let Some(job) = guard.as_mut() {
            if &job.root == root {
                job.cancelled = true;
                WORK_ACTIVE_EPOCH.store(0, Ordering::Release);
            }
        }
    }

    pub fn generate(&self, root: Root, difficulty: u64) -> Option<WorkNonce> {
        if self.thread_count == 0 {
            return None;
        }

        if self.cpu_rate_limiter.is_zero() {
            // Try to claim the shared A100 multi-hart cooperative PoW accelerator slot
            let claimed_epoch = {
                let mut guard = WORK_SLOT.lock();
                let busy = guard
                    .as_ref()
                    .is_some_and(|j| WORK_ACTIVE_EPOCH.load(Ordering::Relaxed) == j.epoch);
                if !busy {
                    let mut epoch = WORK_NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
                    if epoch == 0 {
                        epoch = WORK_NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
                    }
                    let mut rng = XorShift1024Star::new();
                    WORK_NEXT_NONCE.store(rng.next_work(), Ordering::Relaxed);
                    *guard = Some(SharedWorkJob {
                        epoch,
                        root,
                        min_difficulty: difficulty,
                        winner: None,
                        cancelled: false,
                    });
                    WORK_ACTIVE_EPOCH.store(epoch, Ordering::Release);
                    Some(epoch)
                } else {
                    None
                }
            };

            if let Some(epoch) = claimed_epoch {
                loop {
                    if WORK_ACTIVE_EPOCH.load(Ordering::Acquire) != epoch {
                        let mut guard = WORK_SLOT.lock();
                        if let Some(job) = guard.as_ref() {
                            if job.epoch == epoch {
                                let res = job.winner;
                                *guard = None;
                                return res;
                            }
                        }
                        return None;
                    }

                    let start_nonce =
                        WORK_NEXT_NONCE.fetch_add(A100_POW_CHUNK_NONCES, Ordering::Relaxed);
                    if let Some(found) = DifficultyV1::pow_search_16(
                        &root,
                        start_nonce,
                        difficulty,
                        A100_POW_CHUNK_ITERS,
                    ) {
                        let mut guard = WORK_SLOT.lock();
                        WORK_ACTIVE_EPOCH.store(0, Ordering::Release);
                        let res = if let Some(job) = guard.as_ref() {
                            if job.epoch == epoch && !job.cancelled {
                                job.winner.or(Some(found))
                            } else {
                                None
                            }
                        } else {
                            Some(found)
                        };
                        *guard = None;
                        return res;
                    }
                }
            }
        }

        let mut generator = CpuWorkGenerator::new(self.cpu_rate_limiter);
        generator.create(&root, difficulty)
    }
}

impl ContainerInfoProvider for WorkPool {
    fn container_info(&self) -> ContainerInfo {
        [("pending", 0, 0)].into()
    }
}

impl Default for WorkPool {
    fn default() -> Self {
        Self::builder().finish()
    }
}

pub(crate) trait WorkGenerator {
    fn create(&mut self, item: &Root, min_difficulty: u64) -> Option<WorkNonce>;
}
