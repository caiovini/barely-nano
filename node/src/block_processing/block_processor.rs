use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{ Mutex };
use crate::utils::JoinHandle;

use rsnano_core::utils::BackpressureSender;
use rsnano_ledger::Ledger;
use rsnano_stats::{StatsCollection, StatsSource};

use super::{
    backlog_waiter::BacklogWaiter, block_batch_processor::BlockBatchProcessorStats,
    BlockProcessorQueue, LedgerEvent, UncheckedMap,
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
}

impl BlockProcessor {
    pub(crate) fn new(
        queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<UncheckedMap>,
        backlog_waiter: Arc<BacklogWaiter>,
        event_publisher: BackpressureSender<LedgerEvent>,
    ) -> Self {
        Self {
            queue,
            ledger,
            unchecked,
            process_stats: Arc::new(BlockBatchProcessorStats::default()),
            threads: Mutex::new(Vec::new()),
            backlog_waiter,
            event_publisher: Mutex::new(Some(event_publisher)),
        }
    }

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
        }
    }

    pub fn stop(&self) {
        drop(self.event_publisher.lock().take());
        self.queue.stop();
        let mut threads = self.threads.lock();
        for _join_handle in threads.drain(..) {
            // join_handle.join().unwrap(); */ */ */ */ */
        }
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
    backlog_waiter: Arc<BacklogWaiter>,
}

impl BlockProcessorLoop {
    #[allow(dead_code)]
    pub fn run(&mut self) {
        while let Some(blocks) = self.queue.pop_blocking() {
            tracing::info!("Block Processor: processing batch of {} blocks", blocks.len());
            self.backlog_waiter.wait_for_backlog();

            if self.queue.stopped() {
                break;
            }

            self.process.process_blocks(blocks);
        }
    }

    pub fn run_once(&mut self) {
        if let Some(blocks) = self.queue.pop() {
            tracing::info!("Block Processor: processing batch of {} blocks", blocks.len());
            self.backlog_waiter.wait_for_backlog();
            self.process.process_blocks(blocks);
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
