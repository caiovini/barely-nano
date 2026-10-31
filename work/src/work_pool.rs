use core::time::Duration;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Root, WorkNonce,
};

use super::CpuWorkGenerator;

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

    pub fn cancel(&self, _root: &Root) {}

    pub fn generate(&self, root: Root, difficulty: u64) -> Option<WorkNonce> {
        if self.thread_count == 0 {
            return None;
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
