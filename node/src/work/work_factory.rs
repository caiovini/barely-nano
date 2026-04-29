use alloc::sync::Arc;
use spin::Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;

use tracing::warn;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider, Peer},
    Difficulty, Root, WorkNonce,
};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_work::{WorkPool, WorkPoolBuilder};

use super::distributed_work_client::DistributedWorkClient;

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

    pub fn is_valid_work(&self, _work: WorkNonce) -> bool {
        true
    }
}

pub struct WorkFactory {
    pub local_work_pool: WorkPool,
    work_peers: Mutex<Vec<Peer>>,
    stopped: AtomicBool,
}

impl WorkFactory {
    pub fn new(
        work_pool: WorkPool,
        _work_client: DistributedWorkClient,
        work_peers: Vec<Peer>,
        _timeout: Duration,
    ) -> Self {
        Self {
            local_work_pool: work_pool,
            work_peers: Mutex::new(work_peers),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn disabled() -> Self {
        Self::new(WorkPool::builder().disabled().finish(), DistributedWorkClient::default(), Vec::new(), Duration::ZERO)
    }

    pub fn generate_work(&self, request: WorkRequest) -> Option<WorkNonce> {
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        self.local_work_pool.generate(request.root, request.difficulty)
    }

    pub fn cancel(&self, root: Root) {
        self.local_work_pool.cancel(&root);
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn work_generation_enabled(&self) -> bool {
        self.local_work_pool.work_generation_enabled()
    }

    pub fn track_cancellations(&self) -> Arc<OutputTrackerMt<Root>> {
        Arc::new(OutputTrackerMt::new())
    }
}

impl ContainerInfoProvider for WorkFactory {
    fn container_info(&self) -> ContainerInfo {
        self.local_work_pool.container_info()
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
            DistributedWorkClient::default(),
            self.work_peers,
            Duration::from_secs(30),
        )
    }
}
