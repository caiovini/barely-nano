use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use core::time::Duration;
use rsnano_nullable_clock::{SteadyClock, Timestamp as Instant};

use strum::{EnumCount, IntoEnumIterator};
use tracing::{debug, warn};

use rsnano_core::{
    BlockType, Epoch, UncheckedInfo,
    utils::{BackpressureSender, backpressure_channel},
};
use rsnano_ledger::{BlockError, Ledger};
use rsnano_stats::{StatsCollection, StatsSource};

use super::{BlockContext, BlockSource, LedgerEvent, UncheckedMap};
use crate::block_processing::ProcessedResult;

pub(crate) struct BlockBatchProcessor {
    pub ledger: Arc<Ledger>,
    pub unchecked: Arc<UncheckedMap>,
    pub stats: Arc<BlockBatchProcessorStats>,
    pub event_publisher: BackpressureSender<LedgerEvent>,
    pub clock: Arc<SteadyClock>,
    pub signature_checker: Arc<crate::signatures::SignatureChecker>,
}

impl BlockBatchProcessor {
    #[allow(dead_code)]
    pub fn new_null() -> Self {
        let (sender, _) = backpressure_channel(0);
        Self {
            ledger: Arc::new(Ledger::new_null()),
            unchecked: Arc::new(UncheckedMap::default()),
            stats: Arc::new(BlockBatchProcessorStats::default()),
            event_publisher: sender,
            clock: Arc::new(SteadyClock::new_null()),
            signature_checker: Arc::new(crate::signatures::SignatureChecker::new()),
        }
    }

    pub(crate) fn process_blocks(&self, mut batch: VecDeque<Arc<BlockContext>>) {
        let timer = Instant::new_test_instance();
        

        self.roll_back_competitor_blocks(&batch);

        // 1. Submit all blocks for parallel signature verification
        let verifications: Vec<_> = batch.iter().map(|ctx| {
            // For State blocks, we can derive the account immediately.
            // For Legacy blocks, we might need a B-Tree lookup (Unknown).
            let account = ctx.block.account_field();
            self.signature_checker.push(Arc::new(ctx.block.clone()), account)
        }).collect();

        // 2. Help verify while waiting for the batch to finish
        for (_, finished) in &verifications {
                    while !finished.load(core::sync::atomic::Ordering::SeqCst) {
                        if !self.signature_checker.help_verify() {
                            core::hint::spin_loop();
                        }
                    }
        }

        // 3. Process the batch in the ledger, using pre-verified signatures where available
        let mut processed = vec![(Err(BlockError::BadSignature), None); batch.len()];
        let mut valid_indices = Vec::new();
        let mut ledger_input = Vec::new();

        for ((i, ctx), (result, _)) in batch.iter().enumerate().zip(verifications) {
            let res_u8 = result.load(core::sync::atomic::Ordering::Acquire);
            valid_indices.push(i);
            ledger_input.push((&ctx.block, res_u8 == crate::signatures::SignatureResult::Valid as u8));
        }

        let ledger_res = self.ledger.process_batch_with_verifications(ledger_input);
        for (i, res) in valid_indices.into_iter().zip(ledger_res.processed) {
            processed[i] = res;
        }

        let mut result = rsnano_ledger::BatchProcessResult { processed };

        let processed_result: Vec<_> = result
            .processed
            .iter()
            .zip(&batch)
            .map(|((result, block), ctx)| ProcessedResult {
                block: ctx.block.clone(),
                source: ctx.source,
                status: *result,
                saved_block: block.clone(),
                channel_id: ctx.channel_id,
            })
            .collect();

        if !processed_result.is_empty() {
            // Use try_send to avoid deadlocking Hart 2 if the event consumer is busy.
            // Diagnostic events should never stall the block processor.
            let _ = self.event_publisher.try_send(LedgerEvent::BlocksProcessed(processed_result));
        }

        if result.processed.len() > 0
            && timer.elapsed(self.clock.now()) > Duration::from_millis(100)
        {
            debug!(
                "Processed {} blocks in {} ms",
                result.processed.len(),
                timer.elapsed(self.clock.now()).as_millis(),
            );
        }

        assert_eq!(result.processed.len(), batch.len());
        let mut result: Vec<(Result<(), BlockError>, Arc<BlockContext>)> = result
            .processed
            .drain(..)
            .zip(batch.drain(..))
            .map(|((status, saved_block), block_ctx)| {
                if saved_block.is_some() {
                    *block_ctx.saved_block.lock() = saved_block;
                }

                (status, block_ctx)
            })
            .collect();

        for (status, block_ctx) in &result {
            let hash = &block_ctx.block.hash();
            let block = &block_ctx.block;
            let saved_block = block_ctx.saved_block.lock().clone();

            match status {
                Ok(()) => {
                    self.stats.progress.fetch_add(1, Relaxed);
                }
                Err(e) => {
                    self.stats.errors[*e as usize].fetch_add(1, Relaxed);
                }
            }

            self.stats.sources[block_ctx.source as usize].fetch_add(1, Relaxed);

            match status {
                Ok(()) => {
                    self.unchecked.trigger(&hash.into());

                    /*
                     * For send blocks check epoch open unchecked (gap pending).
                     * For state blocks check only send subtype and only if block epoch is not last epoch.
                     * If epoch is last, then pending entry shouldn't trigger same epoch open block for destination account.
                     * */
                    let block = if let Some(b) = saved_block {
                        b
                    } else {
                        warn!("Saved block was missing after Ok process result");
                        continue;
                    };
                    if block.block_type() == BlockType::LegacySend
                        || block.block_type() == BlockType::State
                            && block.is_send()
                            && block.epoch() < Epoch::MAX
                    {
                        self.unchecked.trigger(&block.destination_or_link().into());
                    }
                }
                Err(BlockError::GapPrevious) => {
                    if block_ctx.source != BlockSource::Bootstrap {
                        self.unchecked
                            .put(block.previous().into(), UncheckedInfo::new(block.clone()));
                    }
                }
                Err(BlockError::GapSource) => {
                    if block_ctx.source != BlockSource::Bootstrap {
                        self.unchecked.put(
                            block
                                .source_field()
                                .unwrap_or(block.link_field().unwrap_or_default().into())
                                .into(),
                            UncheckedInfo::new(block.clone()),
                        );
                    }
                }
                Err(BlockError::GapEpochOpenPending) => {
                    if block_ctx.source != BlockSource::Bootstrap {
                        // Specific unchecked key starting with epoch open block account public key
                        self.unchecked.put(
                            block.account_field().unwrap().into(),
                            UncheckedInfo::new(block.clone()),
                        );
                    }
                }
                Err(BlockError::Old) => {
                    debug!("Block is old: {}", hash)
                }
                Err(BlockError::Conflict) => {
                    debug!("Block conflict: {}", hash)
                }
                // These are unexpected and indicate erroneous/malicious behavior, log debug info to highlight the issue
                Err(BlockError::BadSignature) => {
                    debug!("Block signature is invalid: {}", hash)
                }
                Err(BlockError::NegativeSpend) => {
                    debug!("Block spends negative amount: {}", hash)
                }
                Err(BlockError::Unreceivable) => {
                    debug!("Block is unreceivable: {}", hash)
                }
                Err(BlockError::Fork) => {
                    debug!("Block is a fork: {}", hash)
                }
                Err(BlockError::OpenedBurnAccount) => {
                    debug!("Block opens burn account: {}", hash)
                }
                Err(BlockError::BalanceMismatch) => {
                    debug!("Block balance mismatch: {}", hash)
                }
                Err(BlockError::RepresentativeMismatch) => {
                    debug!("Block representative mismatch: {}", hash)
                }
                Err(BlockError::BlockPosition) => {
                    debug!("Block is in incorrect position: {}", hash)
                }
                Err(BlockError::InsufficientWork) => {
                    debug!("Block has insufficient work: {}", hash)
                }
            }
        }

        // Set results for futures when not holding the lock
        for (res, context) in result.iter_mut() {
            if let Some(cb) = &context.callback {
                cb(*res);
            }
            context.set_result(*res);
        }
    }

    fn roll_back_competitor_blocks(&self, batch: &VecDeque<Arc<BlockContext>>) {
        let fork_blocks = batch.iter().filter_map(|i| {
            if i.source == BlockSource::Forced {
                Some(&i.block)
            } else {
                None
            }
        });
        self.ledger.roll_back_competitors(fork_blocks, |results| {
            let _ = self.event_publisher.try_send(LedgerEvent::BlocksRolledBack(results));
        });
    }
}

#[derive(Default)]
pub(crate) struct BlockBatchProcessorStats {
    pub(crate) progress: AtomicU64,
    errors: [AtomicU64; BlockError::COUNT],
    sources: [AtomicU64; BlockSource::COUNT],
}

impl BlockBatchProcessorStats {
    pub fn total_processed(&self) -> u64 {
        self.progress.load(core::sync::atomic::Ordering::Relaxed)
    }
}

impl StatsSource for BlockBatchProcessorStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "block_processor_result",
            "progress",
            self.progress.load(Relaxed),
        );

        for e in BlockError::iter() {
            result.insert(
                "block_processor_result",
                e.into(),
                self.errors[e as usize].load(Relaxed),
            );
        }

        for s in BlockSource::iter() {
            result.insert(
                "block_processor_source",
                s.into(),
                self.sources[s as usize].load(Relaxed),
            );
        }
    }
}
