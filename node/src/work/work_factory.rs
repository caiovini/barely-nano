use alloc::{sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use spin::Mutex;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider, Peer},
    Difficulty, DifficultyV1, Root, WorkNonce,
};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_work::{WorkPool, WorkPoolBuilder};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkRequest {
    pub root: Root,
    pub difficulty: u64,
}

impl WorkRequest {
    pub fn new(root: Root, difficulty: u64) -> Self {
        Self { root, difficulty }
    }

    pub fn new_test_instance() -> Self {
        Self::new(Root::from(100), 0)
    }

    pub fn is_valid_work(&self, work: WorkNonce) -> bool {
        self.difficulty_of(work) >= self.difficulty
    }

    pub fn difficulty_of(&self, work: WorkNonce) -> u64 {
        DifficultyV1 {}.get_difficulty(&self.root, work)
    }
}

pub struct WorkFactory {
    pub local_work_pool: WorkPool,
    work_peers: Mutex<Vec<Peer>>,
    cancel_listener: OutputListenerMt<Root>,
    stopped: AtomicBool,
}

impl WorkFactory {
    pub fn new(work_pool: WorkPool, work_peers: Vec<Peer>, _timeout: Duration) -> Self {
        Self {
            local_work_pool: work_pool,
            work_peers: Mutex::new(work_peers),
            cancel_listener: OutputListenerMt::new(),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn disabled() -> Self {
        Self::new(
            WorkPool::builder().disabled().finish(),
            Vec::new(),
            Duration::ZERO,
        )
    }

    pub fn generate_work(&self, request: WorkRequest) -> Option<WorkNonce> {
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        self.local_work_pool
            .generate(request.root, request.difficulty)
    }

    pub fn cancel(&self, root: Root) {
        self.cancel_listener.emit(root);
        self.local_work_pool.cancel(&root);
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn work_generation_enabled(&self) -> bool {
        self.local_work_pool.work_generation_enabled()
    }

    pub fn track_cancellations(&self) -> Arc<OutputTrackerMt<Root>> {
        self.cancel_listener.track()
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.work_peers.lock().clone()
    }

    pub fn add_peer(&self, peer: Peer) {
        self.work_peers.lock().push(peer);
    }

    pub fn clear_peers(&self) {
        self.work_peers.lock().clear();
    }
}

impl ContainerInfoProvider for WorkFactory {
    fn container_info(&self) -> ContainerInfo {
        self.local_work_pool.container_info()
    }
}

impl WorkFactory {
    pub fn builder() -> WorkFactoryBuilder {
        WorkFactoryBuilder {
            local_work_pool: None,
            work_peers: Vec::new(),
        }
    }

    pub fn new_null() -> Self {
        Self::disabled()
    }
}

pub struct WorkFactoryBuilder {
    local_work_pool: Option<WorkPool>,
    work_peers: Vec<Peer>,
}

impl WorkFactoryBuilder {
    pub fn local_work_pool(mut self, f: impl FnOnce(WorkPoolBuilder) -> WorkPoolBuilder) -> Self {
        self.local_work_pool = Some(f(WorkPool::builder()).finish());
        self
    }

    pub fn finish(self) -> WorkFactory {
        WorkFactory::new(
            self.local_work_pool.unwrap_or_default(),
            self.work_peers,
            Duration::from_secs(30),
        )
    }
}
