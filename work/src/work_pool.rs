use core::{
    mem::size_of,
    time::Duration,
};
use alloc::{
    sync::Arc,
    boxed::Box,
};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Root, WorkNonce,
};

use super::{
    CpuWorkGenerator, WorkItem, WorkQueueCoordinator, WorkThread, WorkTicket,
};

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

        WorkPool::new(
            thread_count,
            cpu_rate_limiter,
        )
    }
}

pub struct WorkPool {
    work_queue: Arc<WorkQueueCoordinator>,
    cpu_rate_limiter: Duration,
    thread_count: usize,
}

impl WorkPool {
    fn new(
        thread_count: usize,
        cpu_rate_limiter: Duration,
    ) -> Self {
        Self {
            work_queue: Arc::new(WorkQueueCoordinator::new()),
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
            work_queue: Arc::new(WorkQueueCoordinator::new()),
            cpu_rate_limiter: Duration::ZERO,
            thread_count: 1,
        }
    }

    pub fn disabled() -> Self {
        Self {
            work_queue: Arc::new(WorkQueueCoordinator::new()),
            cpu_rate_limiter: Duration::ZERO,
            thread_count: 0,
        }
    }

    /// In no_std, the caller must manually run the work loop in a background task or hart.
    pub fn run(&self) {
        let generator = CpuWorkGenerator::new(self.cpu_rate_limiter);
        WorkThread::new(generator, Arc::clone(&self.work_queue)).work_loop();
    }

    pub fn has_opencl(&self) -> bool {
        false
    }

    pub fn work_generation_enabled(&self) -> bool {
        self.thread_count > 0
    }

    pub fn cancel(&self, root: &Root) {
        self.work_queue.cancel(root);
    }

    pub fn stop(&self) {
        self.work_queue.stop();
    }

    pub fn size(&self) -> usize {
        self.work_queue.lock_work_queue().len()
    }

    pub fn pending_value_size() -> usize {
        size_of::<WorkItem>()
    }

    pub fn thread_count(&self) -> usize {
        self.thread_count
    }

    pub fn generate_async(
        &self,
        root: Root,
        difficulty: u64,
        done: Option<Box<dyn FnOnce(Option<WorkNonce>) + Send>>,
    ) {
        debug_assert!(!root.is_zero());
        if self.thread_count > 0 {
            self.work_queue.enqueue(WorkItem {
                root,
                min_difficulty: difficulty,
                callback: done,
            });
        } else if let Some(callback) = done {
            callback(None);
        }
    }

    pub fn generate(&self, root: Root, difficulty: u64) -> Option<WorkNonce> {
        if self.thread_count == 0 {
            return None;
        }

        let mut generator = CpuWorkGenerator::new(self.cpu_rate_limiter);
        generator.create(&root, difficulty, &WorkTicket::never_expires())
    }
}

impl ContainerInfoProvider for WorkPool {
    fn container_info(&self) -> ContainerInfo {
        [("pending", self.size(), Self::pending_value_size())].into()
    }
}

impl Default for WorkPool {
    fn default() -> Self {
        Self::builder().finish()
    }
}

pub(crate) trait WorkGenerator {
    fn create(
        &mut self,
        item: &Root,
        min_difficulty: u64,
        work_ticket: &WorkTicket,
    ) -> Option<WorkNonce>;
}
