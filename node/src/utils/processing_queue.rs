use alloc::sync::Arc;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::boxed::Box;
use spin::Mutex;
use core::sync::atomic::{ AtomicBool, Ordering };

use rsnano_stats::{DetailType, Direction, StatType, Stats};

/**
 * Queue that processes enqueued elements in (possibly parallel) batches
 */
pub struct ProcessingQueue<T: Send + 'static> {
    max_queue_size: usize,
    shared_state: Arc<SharedState<T>>,
    stats: Arc<Stats>,
    stat_type: StatType,
}

impl<T: Send + 'static> ProcessingQueue<T> {
    /**
     * @param thread_count Number of processing threads
     * @param max_queue_size Max number of items enqueued, items beyond this value will be discarded
     * @param max_batch_size Max number of elements processed in single batch, 0 for unlimited (default)
     */
    pub fn new(
        stats: Arc<Stats>,
        stat_type: StatType,
        _thread_name: String,
        _thread_count: usize,
        max_queue_size: usize,
        max_batch_size: usize,
        process_batch: Box<dyn Fn(VecDeque<T>) + Send + Sync>,
    ) -> Self {
        Self {
            stats: Arc::clone(&stats),
            stat_type,
            max_queue_size,
            shared_state: Arc::new(SharedState::new(
                max_batch_size,
                process_batch,
            )),
        }
    }

    pub fn start(&self) {}

    pub fn stop(&self) {
        let _guard = self.shared_state.queue.lock();
        self.shared_state.stopped.store(true, Ordering::SeqCst);
    }

    /// Queues item for batch processing
    pub fn add(&self, item: T) {
        let mut queue = self.shared_state.queue.lock();
        if queue.len() < self.max_queue_size {
            queue.push_back(item);
            drop(queue);
            self.stats
                .inc_dir(self.stat_type, DetailType::Queue, Direction::In);
        } else {
            self.stats
                .inc_dir(self.stat_type, DetailType::Overfill, Direction::In);
        }
    }

    pub fn len(&self) -> usize {
        self.shared_state.queue.lock().len()
    }

    pub fn run_once(&self) -> bool {
        if self.shared_state.stopped.load(Ordering::SeqCst) {
            return false;
        }
        let batch = {
            let mut guard = self.shared_state.queue.lock();
            if guard.is_empty() {
                return false;
            }
            if self.shared_state.max_batch_size == 0
                || guard.len() <= self.shared_state.max_batch_size
            {
                let mut queue_l = VecDeque::new();
                core::mem::swap(&mut queue_l, &mut *guard);
                queue_l
            } else {
                let mut queue_l = VecDeque::with_capacity(self.shared_state.max_batch_size);
                for _ in 0..self.shared_state.max_batch_size {
                    if let Some(item) = guard.pop_front() {
                        queue_l.push_back(item);
                    }
                }
                queue_l
            }
        };
        if !batch.is_empty() {
            self.stats
                .inc_dir(self.stat_type, DetailType::Batch, Direction::In);
            (self.shared_state.process_batch)(batch);
            true
        } else {
            false
        }
    }
}

impl<T: Send + 'static> Drop for ProcessingQueue<T> {
    fn drop(&mut self) {
        self.stop()
    }
}

struct SharedState<T> {
    queue: Mutex<VecDeque<T>>,
    stopped: AtomicBool,
    max_batch_size: usize,
    process_batch: Box<dyn Fn(VecDeque<T>) + Send + Sync>,
}

impl<T> SharedState<T> {
    pub fn new(
        max_batch_size: usize,
        process_batch: Box<dyn Fn(VecDeque<T>) + Send + Sync>,
    ) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            stopped: AtomicBool::new(false),
            max_batch_size,
            process_batch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_queue() {
        let fixture = create_fixture();
        assert_eq!(fixture.queue.len(), 0);
    }

    #[test]
    fn max_queue_size() {
        let fixture = create_fixture();
        fixture.queue.stop();
        for _ in 0..2 * MAX_TEST_QUEUE_LEN {
            fixture.queue.add(1);
        }
        assert_eq!(fixture.queue.len(), MAX_TEST_QUEUE_LEN)
    }

    struct TestFixture {
        _processed: Arc<Mutex<usize>>,
        queue: ProcessingQueue<i32>,
    }

    const MAX_TEST_QUEUE_LEN: usize = 16;

    fn create_fixture() -> TestFixture {
        let processed = Arc::new(Mutex::new(0));
        let processed_clone = Arc::clone(&processed);

        let queue = ProcessingQueue::new(
            Arc::new(Stats::new(Default::default())),
            StatType::BootstrapServer,
            "processing test thread".to_string(),
            4,
            MAX_TEST_QUEUE_LEN,
            2,
            Box::new(move |i| {
                *processed_clone.lock() += i.len();
            }),
        );
        queue.start();
        TestFixture {
            queue,
            _processed: processed,
        }
    }
}

