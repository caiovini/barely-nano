use alloc::sync::Arc;
use core::sync::atomic::AtomicU8;
use spin::Mutex;
use alloc::boxed::Box;

use rsnano_core::{Account, Block, SavedBlock};
use rsnano_ledger::BlockError;
use rsnano_network::ChannelId;

use super::BlockSource;

pub type BlockProcessorCallback = Box<dyn Fn(Result<(), BlockError>) + Send + Sync>;

pub const SIG_STATE_UNCLAIMED: u8 = 0;
pub const SIG_STATE_IN_PROGRESS: u8 = 1;
pub const SIG_STATE_DONE: u8 = 2;

pub struct BlockContext {
    pub block: Block,
    pub account: Option<Account>,
    pub saved_block: Mutex<Option<SavedBlock>>,
    pub source: BlockSource,
    pub callback: Option<BlockProcessorCallback>,
    pub waiter: Option<Arc<BlockProcessorWaiter>>,
    pub channel_id: ChannelId,
    pub sig_state: AtomicU8,
}

impl BlockContext {
    pub fn new(block: Block, source: BlockSource, channel_id: ChannelId) -> Self {
        Self {
            block,
            account: None,
            saved_block: Mutex::new(None),
            source,
            callback: None,
            waiter: None,
            channel_id,
            sig_state: AtomicU8::new(SIG_STATE_UNCLAIMED),
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
            account: None,
            saved_block: Mutex::new(None),
            source,
            callback: Some(callback),
            waiter: None,
            channel_id,
            sig_state: AtomicU8::new(SIG_STATE_UNCLAIMED),
        }
    }

    pub fn with_account(mut self, account: Account) -> Self {
        if !account.is_zero() {
            self.account = Some(account);
        }
        self
    }

    pub fn new_blocking(block: Block, source: BlockSource, channel_id: ChannelId) -> (Self, Arc<BlockProcessorWaiter>) {
        let waiter = Arc::new(BlockProcessorWaiter::new());
        let ctx = Self {
            block,
            account: None,
            saved_block: Mutex::new(None),
            source,
            callback: None,
            waiter: Some(waiter.clone()),
            channel_id,
            sig_state: AtomicU8::new(SIG_STATE_UNCLAIMED),
        };
        (ctx, waiter)
    }

    pub fn new_test_instance() -> Self {
        Self::new(
            Block::new_test_instance(),
            BlockSource::Live,
            ChannelId::LOOPBACK,
        )
    }

    pub fn set_result(&self, result: Result<(), BlockError>) {
        if let Some(waiter) = &self.waiter {
            waiter.set_result(result);
        }
    }
}

impl Drop for BlockContext {
    fn drop(&mut self) {
        if let Some(waiter) = &self.waiter {
            waiter.cancel();
        }
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
