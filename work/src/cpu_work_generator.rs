use core::time::Duration;

use rsnano_core::{Difficulty, DifficultyV1, Root, WorkNonce};

use super::{WorkGenerator, WorkRng, WorkTicket, XorShift1024Star};

pub(crate) trait Sleeper {
    fn sleep(&mut self, duration: Duration);
}

pub(crate) struct NoopSleeper {}

impl NoopSleeper {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl Sleeper for NoopSleeper {
    fn sleep(&mut self, _duration: Duration) {
        // In a bare-metal environment, we might want a busy wait or a yield
        core::hint::spin_loop();
    }
}

pub(crate) struct CpuWorkGenerator<
    Rng = XorShift1024Star,
    Diff = DifficultyV1,
    Sleep = NoopSleeper,
> where
    Rng: WorkRng,
    Diff: Difficulty,
    Sleep: Sleeper,
{
    // Quick RNG for work attempts.
    rng: Rng,
    difficulty: Diff,
    sleeper: Sleep,
    rate_limiter: Duration,
    pub iteration_size: usize,
}

const DEFAULT_ITERATION_SIZE: usize = 256;

pub(crate) fn create_cpu_work_generator<R, D, S>(
    rng: R,
    difficulty: D,
    sleeper: S,
    rate_limiter: Duration,
) -> CpuWorkGenerator<R, D, S>
where
    R: WorkRng,
    D: Difficulty,
    S: Sleeper,
{
    CpuWorkGenerator {
        rng,
        difficulty,
        sleeper,
        rate_limiter,
        iteration_size: DEFAULT_ITERATION_SIZE,
    }
}

impl CpuWorkGenerator {
    pub fn new(rate_limiter: Duration) -> Self {
        create_cpu_work_generator(
            XorShift1024Star::new(),
            DifficultyV1::default(),
            NoopSleeper::new(),
            rate_limiter,
        )
    }
}

// Single threaded PoW generation on the CPU
impl<Rng, Diff, Sleep> CpuWorkGenerator<Rng, Diff, Sleep>
where
    Rng: WorkRng,
    Diff: Difficulty,
    Sleep: Sleeper,
{
    fn next(&mut self, item: &Root) -> (WorkNonce, u64) {
        let work = WorkNonce::from(self.rng.next_work());
        let difficulty = self.difficulty.get_difficulty(item, work);
        (work, difficulty)
    }

    /// Tries to create PoW in a batch of 256 iterations
    fn try_create_batch(&mut self, item: &Root, min_difficulty: u64) -> Option<WorkNonce> {
        let mut iteration = self.iteration_size;
        let mut work = WorkNonce::from(0);
        let mut difficulty = 0;
        while difficulty < min_difficulty && iteration > 0 {
            (work, difficulty) = self.next(item);
            iteration -= 1;
        }

        if difficulty >= min_difficulty {
            Some(work)
        } else {
            None
        }
    }
}

impl<Rng, Diff, Sleep> WorkGenerator for CpuWorkGenerator<Rng, Diff, Sleep>
where
    Rng: WorkRng,
    Diff: Difficulty,
    Sleep: Sleeper,
{
    fn create(
        &mut self,
        item: &Root,
        min_difficulty: u64,
        work_ticket: &WorkTicket,
    ) -> Option<WorkNonce> {
        while !work_ticket.expired() {
            let result = self.try_create_batch(item, min_difficulty);
            if result.is_some() {
                return result.map(|i| i.into());
            }

            if !self.rate_limiter.is_zero() {
                self.sleeper.sleep(self.rate_limiter);
            }
        }
        None
    }
}
