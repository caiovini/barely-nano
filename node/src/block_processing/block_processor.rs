use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{ Mutex };
use crate::utils::JoinHandle;

use rsnano_core::utils::BackpressureSender;
use rsnano_ledger::Ledger;
use rsnano_stats::{StatsCollection, StatsSource};

use super::{
    backlog_waiter::BacklogWaiter, block_batch_processor::BlockBatchProcessorStats,
    BlockContext, BlockProcessorQueue, LedgerEvent, UncheckedMap,
};
use crate::block_processing::block_batch_processor::BlockBatchProcessor;

pub struct BlockProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    unchecked: Arc<UncheckedMap>,
    process_stats: Arc<BlockBatchProcessorStats>,
    backlog_waiter: Arc<BacklogWaiter>,
    event_publisher: Mutex<Option<BackpressureSender<LedgerEvent>>>,
    pub signature_checker: Arc<crate::signatures::SignatureChecker>,
    cached_loop: Mutex<Option<BlockProcessorLoop>>,
}

impl BlockProcessor {
    pub(crate) fn new(
        queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<UncheckedMap>,
        backlog_waiter: Arc<BacklogWaiter>,
        event_publisher: BackpressureSender<LedgerEvent>,
        signature_checker: Arc<crate::signatures::SignatureChecker>,
    ) -> Self {
        let process_stats = Arc::new(BlockBatchProcessorStats::default());
        let process = BlockBatchProcessor {
            ledger: ledger.clone(),
            unchecked: unchecked.clone(),
            stats: process_stats.clone(),
            event_publisher: event_publisher.clone(),
            clock: Arc::new(rsnano_nullable_clock::SteadyClock::default()),
            signature_checker: signature_checker.clone(),
        };
        let cached_loop = BlockProcessorLoop {
            queue: queue.clone(),
            process,
            backlog_waiter: backlog_waiter.clone(),
        };

        Self {
            queue,
            ledger,
            unchecked,
            process_stats,
            threads: Mutex::new(Vec::new()),
            backlog_waiter,
            event_publisher: Mutex::new(Some(event_publisher)),
            signature_checker,
            cached_loop: Mutex::new(Some(cached_loop)),
        }
    }

    /// In bare-metal mode, the BlockProcessor is driven manually
    /// by Hart 2 in main/src/bin.rs. This start() method remains as a 
    /// no-op placeholder for architectural consistency.
    pub fn start(&self, _thread_count: usize) {
        // debug_assert!(self.threads.lock().is_empty());
        // for _ in 0..thread_count {
        //     let mut processor_loop = self.create_loop();
        //
        //     self.threads.lock().lock().push(
        //         /* /* std::/* /* /* thread::Builder::new()
        //             .name("Blck processing".to_string())
        //             .spawn(move || {
        //                 processor_loop.run();
        //             })
        //             .unwrap(),
        //     );
        // }
    }

    pub fn run(&self) {
        let mut loop_state = self.create_loop();
        loop_state.run();
    }

    pub fn run_once(&self) {
        if let Some(mut guard) = self.cached_loop.try_lock() {
            if let Some(loop_state) = guard.as_mut() {
                loop_state.run_once();
                return;
            }
        }

        let mut loop_state = self.create_loop();
        loop_state.run_once();
    }

    fn create_loop(&self) -> BlockProcessorLoop {
        BlockProcessorLoop {
            queue: self.queue.clone(),
            process: self.create_block_batch_processor(),
            backlog_waiter: self.backlog_waiter.clone(),
        }
    }

    fn create_block_batch_processor(&self) -> BlockBatchProcessor {
        BlockBatchProcessor {
            ledger: self.ledger.clone(),
            unchecked: self.unchecked.clone(),
            stats: self.process_stats.clone(),
            event_publisher: self
                .event_publisher
                .lock()
                .as_ref()
                .unwrap()
                .clone(),
            clock: Arc::new(rsnano_nullable_clock::SteadyClock::default()), // TODO: use real clock
            signature_checker: self.signature_checker.clone(),
        }
    }

    pub fn stop(&self) {
        drop(self.event_publisher.lock().take());
        drop(self.cached_loop.lock().take());
        self.queue.stop();
        let mut threads = self.threads.lock();
        for _join_handle in threads.drain(..) {
            // join_handle.join().unwrap(); */ */ */ */ */
        }
    }

    pub fn total_queue_len(&self) -> usize {
        self.queue.total_queue_len()
    }

    pub fn try_total_queue_len(&self) -> Result<usize, ()> {
        if let Some(guard) = self.queue.try_lock_internal() {
            Ok(guard.total_queue_len())
        } else {
            Err(())
        }
    }

    pub fn total_processed(&self) -> u64 {
        self.process_stats.total_processed()
    }

    pub fn push(&self, context: Arc<BlockContext>) -> bool {
        self.queue.push(context)
    }

    pub fn try_push(&self, context: Arc<BlockContext>) -> Result<bool, ()> {
        self.queue.try_push(context)
    }
}

impl Drop for BlockProcessor {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StatsSource for BlockProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.process_stats.collect_stats(result);
    }
}

struct BlockProcessorLoop {
    queue: Arc<BlockProcessorQueue>,
    process: BlockBatchProcessor,
    #[allow(dead_code)]
    backlog_waiter: Arc<BacklogWaiter>,
}

impl BlockProcessorLoop {
    #[allow(dead_code)]
    pub fn run(&mut self) {
        while let Some(blocks) = self.queue.pop_blocking() {
            if self.queue.stopped() {
                break;
            }

            self.process.process_blocks(blocks);
        }
    }

    pub fn run_once(&mut self) {
        let mut count = 0;
        while let Some(blocks) = self.queue.pop() {
            self.process.process_blocks(blocks);
            
            count += 1;
            if count >= 16 || self.queue.stopped() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_processing::BlockContext;

    #[test]
    fn wait_for_backlog() {
        let queue = Arc::new(BlockProcessorQueue::new_null_with(vec![
            BlockContext::new_test_instance().into(),
        ]));
        let process = BlockBatchProcessor::new_null();
        let backlog_waiter = Arc::new(BacklogWaiter::new_null());

        let mut processor_loop = BlockProcessorLoop {
            queue,
            process,
            backlog_waiter: backlog_waiter.clone(),
        };

        processor_loop.run();

        assert_eq!(backlog_waiter.call_count(), 1);
    }
}
