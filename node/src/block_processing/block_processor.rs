use alloc::sync::Arc;
use spin::Mutex;

use rsnano_core::utils::BackpressureSender;
use rsnano_ledger::Ledger;
use rsnano_stats::{StatsCollection, StatsSource};

use super::{
    block_batch_processor::BlockBatchProcessorStats, BlockContext, BlockProcessorQueue,
    LedgerEvent, UncheckedMap,
};
use crate::block_processing::block_batch_processor::BlockBatchProcessor;

pub struct BlockProcessor {
    queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    unchecked: Arc<UncheckedMap>,
    process_stats: Arc<BlockBatchProcessorStats>,
    event_publisher: Mutex<Option<BackpressureSender<LedgerEvent>>>,
    pub signature_checker: Arc<crate::signatures::SignatureChecker>,
    cached_loop: Mutex<Option<BlockProcessorLoop>>,
}

impl BlockProcessor {
    pub(crate) fn new(
        queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<UncheckedMap>,
        event_publisher: BackpressureSender<LedgerEvent>,
        signature_checker: Arc<crate::signatures::SignatureChecker>,
    ) -> Self {
        let process_stats = Arc::new(BlockBatchProcessorStats::default());
        queue.set_signature_checker(signature_checker.clone());
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
        };

        Self {
            queue,
            ledger,
            unchecked,
            process_stats,
            event_publisher: Mutex::new(Some(event_publisher)),
            signature_checker,
            cached_loop: Mutex::new(Some(cached_loop)),
        }
    }

    pub fn start(&self) {}

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
            clock: Arc::new(rsnano_nullable_clock::SteadyClock::default()),
            signature_checker: self.signature_checker.clone(),
        }
    }

    pub fn stop(&self) {
        drop(self.event_publisher.lock().take());
        drop(self.cached_loop.lock().take());
        self.queue.stop();
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
}

impl BlockProcessorLoop {
    pub fn run_once(&mut self) {
        if let Some(blocks) = self.queue.pop() {
            self.process.process_blocks(blocks);
        }
    }
}
