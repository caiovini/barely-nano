use alloc::sync::Arc;
use spin::Mutex;
use alloc::boxed::Box;

use rsnano_core::{Block, SavedBlock};
use rsnano_ledger::BlockError;
use rsnano_network::ChannelId;

use super::BlockSource;

pub type BlockProcessorCallback = Box<dyn Fn(Result<(), BlockError>) + Send + Sync>;

pub struct BlockContext {
    pub block: Block,
    pub saved_block: Mutex<Option<SavedBlock>>,
    pub source: BlockSource,
    pub callback: Option<BlockProcessorCallback>,
    pub waiter: Arc<BlockProcessorWaiter>,
    pub channel_id: ChannelId,
}

impl BlockContext {
    pub fn new(block: Block, source: BlockSource, channel_id: ChannelId) -> Self {
        Self {
            block,
            saved_block: Mutex::new(None),
            source,
            callback: None,
            waiter: Arc::new(BlockProcessorWaiter::new()),
            channel_id,
        }
    }

    pub fn new_with_callback(
        block: Block,
        source: BlockSource,
        channel_id: ChannelId,
        callback: BlockProcessorCallback,
    ) -> Self {
        Self {
            block,
            saved_block: Mutex::new(None),
            source,
            callback: Some(callback),
            waiter: Arc::new(BlockProcessorWaiter::new()),
            channel_id,
        }
    }

    pub fn new_test_instance() -> Self {
        Self::new(
            Block::new_test_instance(),
            BlockSource::Live,
            ChannelId::LOOPBACK,
        )
    }

    pub fn set_result(&self, result: Result<(), BlockError>) {
        self.waiter.set_result(result);
    }

    pub fn get_waiter(&self) -> Arc<BlockProcessorWaiter> {
        self.waiter.clone()
    }
}

impl Drop for BlockContext {
    fn drop(&mut self) {
        self.waiter.cancel()
    }
}

pub struct BlockProcessorWaiter {
    result: Mutex<(Option<Result<(), BlockError>>, bool)>, // (status, done)
}

impl BlockProcessorWaiter {
    pub fn new() -> Self {
        Self {
            result: Mutex::new((None, false)),
        }
    }

    pub fn set_result(&self, result: Result<(), BlockError>) {
        let mut guard = self.result.lock();
        *guard = (Some(result), true);
    }

    pub fn cancel(&self) {
        self.result.lock().1 = true;
    }

    pub fn wait_result(&self) -> Option<Result<(), BlockError>> {
        loop {
            let guard = self.result.lock();
            if guard.1 {
                return guard.0.clone();
            }
            drop(guard);
            core::hint::spin_loop();
        }
    }
}
