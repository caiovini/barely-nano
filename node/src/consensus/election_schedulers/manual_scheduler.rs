use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::{ Mutex, RwLock };
use crate::utils::{Condvar, JoinHandle};
use core::mem::size_of;

use rsnano_core::{utils::ContainerInfo, Amount, Block, BlockHash, SavedBlock};
use rsnano_stats::{DetailType, StatType, Stats};

use crate::consensus::{election::ElectionBehavior, ActiveElectionsContainer, AecInsertRequest};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_nullable_clock::SteadyClock;

pub struct ManualScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    mutex: Mutex<ManualSchedulerImpl>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    clock: Arc<SteadyClock>,
    ledger: Arc<Ledger>,
}

impl ManualScheduler {
    pub fn new(
        stats: Arc<Stats>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        clock: Arc<SteadyClock>,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            thread: Mutex::new(None),
            condition: Condvar::new(),
            stats,
            active_elections,
            clock,
            ledger,
            mutex: Mutex::new(ManualSchedulerImpl {
                queue: Default::default(),
                stopped: false,
            }),
        }
    }

    pub fn stop(&self) {
        self.mutex.lock().stopped = true;
        self.notify();
        let _handle = self.thread.lock().take();
        // if let Some(handle) = handle {
        //     handle.join().unwrap();
        // }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.mutex
            .lock()
            .queue
            .iter()
            .any(|block| block.hash() == *hash)
    }

    pub fn notify(&self) {
        self.condition.notify_all();
    }

    pub fn push(&self, block: SavedBlock) {
        {
            let mut guard = self.mutex.lock();
            guard.queue.push_back(block);
        }
        self.notify();
    }

    pub fn run_once(&self) {
        let mut guard = self.mutex.lock();
        if !guard.stopped && guard.predicate() {
            self.stats
                .inc(StatType::ElectionScheduler, DetailType::Loop);

            let block = guard.queue.pop_front().unwrap();
            drop(guard);

            let hash = block.hash();
            let priority = self.ledger.any().block_priority(&block);
            self.stats
                .inc(StatType::ElectionScheduler, DetailType::InsertManual);

            let now = self.clock.now();

            let mut aec = self.active_elections.write();
            if aec
                .insert(AecInsertRequest::new_manual(block, priority), now)
                .is_ok()
            {
                aec.transition_active(&hash);
            }
        }
    }

    #[allow(dead_code)]
    fn run(&self) {
        let mut guard = self.mutex.lock();
        while !guard.stopped {
            guard = self
                .condition
                .wait_while(guard, |g| !g.stopped && !g.predicate())
                .unwrap();

            if !guard.stopped {
                self.stats
                    .inc(StatType::ElectionScheduler, DetailType::Loop);

                if guard.predicate() {
                    let block = guard.queue.pop_front().unwrap();
                    drop(guard);

                    let hash = block.hash();
                    let priority = self.ledger.any().block_priority(&block);
                    self.stats
                        .inc(StatType::ElectionScheduler, DetailType::InsertManual);

                    let now = self.clock.now();

                    let mut aec = self.active_elections.write();
                    if aec
                        .insert(AecInsertRequest::new_manual(block, priority), now)
                        .is_ok()
                    {
                        aec.transition_active(&hash);
                    }
                } else {
                    drop(guard);
                }
                self.notify();
                guard = self.mutex.lock();
            }
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.mutex.lock();
        [(
            "queue",
            guard.queue.len(),
            size_of::<Arc<Block>>() + size_of::<Option<Amount>>() + size_of::<ElectionBehavior>(),
        )]
        .into()
    }
}

impl Drop for ManualScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        // debug_assert!(self.thread.lock().is_none());
    }
}

pub trait ManualSchedulerExt {
    fn start(&self);
}

impl ManualSchedulerExt for Arc<ManualScheduler> {
    fn start(&self) {
        self.mutex.lock().stopped = false;
    }
}

struct ManualSchedulerImpl {
    queue: VecDeque<SavedBlock>,
    stopped: bool,
}

impl ManualSchedulerImpl {
    fn predicate(&self) -> bool {
        !self.queue.is_empty()
    }
}
