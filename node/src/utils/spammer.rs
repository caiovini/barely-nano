use alloc::sync::Arc;
use rsnano_core::{PrivateKey, Amount, BlockHash, TestBlockBuilder, DEV_GENESIS_KEY};
use rsnano_network::ChannelId;
use crate::block_processing::{BlockProcessor, BlockSource, BlockContext};
use spin::Mutex;

pub struct Spammer {
    processor: Arc<BlockProcessor>,
    key: PrivateKey,
    previous: Mutex<BlockHash>,
    balance: Mutex<Amount>,
}

impl Spammer {
    pub fn new(processor: Arc<BlockProcessor>, head: BlockHash, balance: Amount) -> Self {
        Self {
            processor,
            key: DEV_GENESIS_KEY.clone(),
            previous: Mutex::new(head),
            balance: Mutex::new(balance),
        }
    }

    pub fn generate_one(&self) -> bool {
        if self.processor.total_queue_len() > 4096 {
            return false;
        }

        let mut prev_guard = self.previous.lock();
        let mut bal_guard = self.balance.lock();

        let next_bal = bal_guard.wrapping_sub(Amount::from(1));
        let block = TestBlockBuilder::state()
            .account(self.key.public_key())
            .previous(*prev_guard)
            .balance(next_bal)
            .representative(self.key.public_key())
            .link(rsnano_core::Account::from(1))
            .key(&self.key)
            .work(rsnano_core::WorkNonce::from(0))
            .build();

        let hash = block.hash();
        *prev_guard = hash;
        *bal_guard = next_bal;

        let context = Arc::new(BlockContext::new(block, BlockSource::Local, ChannelId::LOOPBACK));
        self.processor.push(context)
    }
    /// Generates up to `count` blocks in a single lock acquisition and submits them to the
    /// block processor. Returns the number of blocks actually pushed (may be less than `count`
    /// if the processor queue is full).
    pub fn generate_batch(&self, count: usize) -> usize {
        let queue_len = match self.processor.try_total_queue_len() {
            Ok(len) => len,
            Err(_) => return 0, // Blocked, skip this batch
        };
        
        if queue_len >= 4096 {
            return 0;
        }
        let available = (4096 - queue_len).min(count);
        let mut pushed = 0;

        if let Some(mut prev_guard) = self.previous.try_lock() {
            if let Some(mut bal_guard) = self.balance.try_lock() {
                for _ in 0..available {
                    let next_bal = bal_guard.wrapping_sub(Amount::from(1));
                    let block = TestBlockBuilder::state()
                        .account(self.key.public_key())
                        .previous(*prev_guard)
                        .balance(next_bal)
                        .representative(self.key.public_key())
                        .link(rsnano_core::Account::from(1))
                        .key(&self.key)
                        .work(rsnano_core::WorkNonce::from(0))
                        .build();

                    let hash = block.hash();
                    let context = Arc::new(BlockContext::new(block, BlockSource::Local, ChannelId::LOOPBACK));
                    
                    if let Ok(added) = self.processor.try_push(context) {
                        if added {
                            *prev_guard = hash;
                            *bal_guard = next_bal;
                            pushed += 1;
                        } else {
                            break; // Queue full
                        }
                    } else {
                        break; // Queue locked, stop pushing this batch
                    }
                }
            }
        }
        
        pushed
    }
}
