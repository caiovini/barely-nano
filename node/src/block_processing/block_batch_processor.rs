use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use core::time::Duration;
use rsnano_nullable_clock::SteadyClock;

use strum::{EnumCount, IntoEnumIterator};
use tracing::{debug, warn};

use rsnano_core::{
    Block, BlockType, Epoch, UncheckedInfo,
    utils::{BackpressureSender, backpressure_channel},
};
use rsnano_ledger::{BlockError, Ledger};
use rsnano_stats::{StatsCollection, StatsSource};

use super::{BlockContext, BlockSource, LedgerEvent, UncheckedMap};
use crate::block_processing::ProcessedResult;

/// Profiling counters (CLINT ticks) for process_blocks stages:
/// [0]=signature wait, [1]=ledger process, [2]=post-processing/unchecked, [3]=batches, [4]=blocks, [5]=blocks needing verification
pub static PROF: [AtomicU64; 8] = [
    AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

const FAST_HASH_SLOTS: usize = 1024;
const FAST_HASH_MASK: usize = FAST_HASH_SLOTS - 1;
const FAST_HASH_MAX_INDEXED: usize = 768;

struct FastBlockHashMap<V> {
    slots: [u16; FAST_HASH_SLOTS],
    entries: Vec<(rsnano_core::BlockHash, V)>,
}

impl<V> FastBlockHashMap<V> {
    #[inline]
    fn with_capacity(cap: usize) -> Self {
        Self {
            slots: [0u16; FAST_HASH_SLOTS],
            entries: Vec::with_capacity(cap),
        }
    }

    #[inline(always)]
    fn hash_idx(k: &rsnano_core::BlockHash) -> usize {
        (u64::from_ne_bytes(k.as_bytes()[0..8].try_into().unwrap()) as usize) & FAST_HASH_MASK
    }

    #[inline]
    fn find_index(&self, key: &rsnano_core::BlockHash) -> Option<usize> {
        let mut h = Self::hash_idx(key);
        loop {
            let s = self.slots[h];
            if s == 0 {
                break;
            }
            let idx = (s - 1) as usize;
            if &self.entries[idx].0 == key {
                return Some(idx);
            }
            h = (h + 1) & FAST_HASH_MASK;
        }
        if self.entries.len() > FAST_HASH_MAX_INDEXED {
            for idx in FAST_HASH_MAX_INDEXED..self.entries.len() {
                if &self.entries[idx].0 == key {
                    return Some(idx);
                }
            }
        }
        None
    }

    #[inline]
    fn contains_key(&self, key: &rsnano_core::BlockHash) -> bool {
        self.find_index(key).is_some()
    }

    #[inline]
    fn get(&self, key: &rsnano_core::BlockHash) -> Option<&V> {
        self.find_index(key).map(|idx| &self.entries[idx].1)
    }

    #[inline]
    fn insert(&mut self, key: rsnano_core::BlockHash, val: V) {
        if self.entries.len() < FAST_HASH_MAX_INDEXED {
            let mut h = Self::hash_idx(&key);
            loop {
                let s = self.slots[h];
                if s == 0 {
                    let idx = self.entries.len();
                    self.entries.push((key, val));
                    self.slots[h] = (idx + 1) as u16;
                    return;
                }
                let idx = (s - 1) as usize;
                if self.entries[idx].0 == key {
                    self.entries[idx].1 = val;
                    return;
                }
                h = (h + 1) & FAST_HASH_MASK;
            }
        } else if let Some(idx) = self.find_index(&key) {
            self.entries[idx].1 = val;
        } else {
            self.entries.push((key, val));
        }
    }
}

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
        let timer = self.clock.now();
        

        let t0 = kernel_common::clint::mtime();
        PROF[3].fetch_add(1, Relaxed);
        PROF[4].fetch_add(batch.len() as u64, Relaxed);
        self.roll_back_competitor_blocks(&batch);

        // Parallel signature verification across harts (skipping blocks guaranteed to fail as GapPrevious)
        let batch_len = batch.len();
        let mut playable_hashes: FastBlockHashMap<Option<rsnano_core::Amount>> =
            FastBlockHashMap::with_capacity(batch_len);
        let mut to_verify = alloc::vec::Vec::with_capacity(batch_len);
        for (idx, ctx) in batch.iter().enumerate() {
            if idx + 4 < batch_len {
                let ahead = &batch[idx + 4].block;
                self.ledger.store.block.prefetch_may_exist(&ahead.hash());
                self.ledger.store.block.prefetch_may_exist(&ahead.previous());
                self.ledger
                    .store
                    .block
                    .prefetch_may_exist(&ahead.source_or_link());
            }
            let b = &ctx.block;
            let hash = b.hash();
            let prev = b.previous();

            if !cfg!(test)
                && self.ledger.store.block.may_exist(&hash)
                && self.ledger.store.block.exists(&hash)
            {
                playable_hashes.insert(hash, b.balance_field());
                continue;
            }

            if !prev.is_zero()
                && !playable_hashes.contains_key(&prev)
                && !self.ledger.store.block.may_exist(&prev)
            {
                // Predecessor does not exist in the ledger or earlier in this batch;
                // BlockValidator::validate() will reject as GapPrevious before checking signature.
                continue;
            }

            let mut gap_source = false;
            match b {
                Block::LegacyReceive(_) | Block::LegacyOpen(_) => {
                    let source = b.source_or_link();
                    if !source.is_zero()
                        && !playable_hashes.contains_key(&source)
                        && !self.ledger.store.block.may_exist(&source)
                    {
                        gap_source = true;
                    }
                }
                Block::State(s) => {
                    if !s.link().is_zero()
                        && !self.ledger.constants.epochs.is_epoch_link(&s.link())
                    {
                        let source: rsnano_core::BlockHash = s.link().into();
                        if !playable_hashes.contains_key(&source)
                            && !self.ledger.store.block.may_exist(&source)
                        {
                            let is_receive = if prev.is_zero() {
                                true
                            } else if let Some(Some(prev_bal)) = playable_hashes.get(&prev) {
                                s.balance() >= *prev_bal
                            } else if !cfg!(test) {
                                if let Some(prev_block) = self.ledger.store.block.get(&prev) {
                                    s.balance() >= prev_block.balance()
                                } else {
                                    true
                                }
                            } else {
                                false
                            };
                            if is_receive {
                                gap_source = true;
                            }
                        }
                    }
                }
                _ => {}
            }

            if !b.is_signature_verified()
                && ctx.source != BlockSource::Local
                && (cfg!(test) || !gap_source)
            {
                let verify_account = b
                    .link_field()
                    .and_then(|link| self.ledger.constants.epochs.epoch_signer(&link))
                    .or_else(|| b.account_field())
                    .or(ctx.account)
                    .filter(|a| !a.is_zero());
                to_verify.push((ctx.clone(), verify_account));
            }

            if !gap_source {
                playable_hashes.insert(hash, b.balance_field());
            }
        }

        let verification_tasks = self.signature_checker.push_batch_front(to_verify);
        let verification_count = verification_tasks.len();
        if !verification_tasks.is_empty() {
            while verification_tasks.iter().any(|ctx| {
                ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                    != crate::block_processing::SIG_STATE_DONE
                    && !ctx.block.is_signature_verified()
            }) {
                if self.signature_checker.help_verify_sync() == 0 {
                    core::hint::spin_loop();
                }
            }
        }

        PROF[5].fetch_add(verification_count as u64, Relaxed);
        let t1 = kernel_common::clint::mtime();
        PROF[0].fetch_add(t1 - t0, Relaxed);
        let ledger_input: Vec<(&Block, bool)> = batch
            .iter()
            .map(|ctx| {
                let is_valid = ctx.block.is_signature_verified() || ctx.source == BlockSource::Local;
                (&ctx.block, is_valid)
            })
            .collect();

        let mut result = self.ledger.process_batch_with_verifications(ledger_input);
        let t2 = kernel_common::clint::mtime();
        PROF[1].fetch_add(t2 - t1, Relaxed);

        let processed_result: Vec<_> = result
            .processed
            .iter()
            .zip(&batch)
            .enumerate()
            .filter(|(_, ((status, _), ctx))| {
                cfg!(test)
                    || !matches!(status, Err(BlockError::GapPrevious))
                    || ctx.source == BlockSource::Live
            })
            .map(|(idx, ((status, block), ctx))| {
                if !cfg!(test) && matches!(status, Err(BlockError::GapEpochOpenPending)) {
                    let acc = ctx
                        .account
                        .or_else(|| ctx.block.account_field())
                        .unwrap_or_default();
                    if !acc.is_zero() {
                        if let Some(next_recv) = batch.iter().skip(idx + 1).find(|next_ctx| {
                            let same_acc = next_ctx.account == Some(acc)
                                || next_ctx.block.account_field() == Some(acc);
                            let link = next_ctx.block.source_or_link();
                            same_acc
                                && !link.is_zero()
                                && !self.ledger.constants.epochs.is_epoch_link(&link.into())
                        }) {
                            return ProcessedResult {
                                block: next_recv.block.clone(),
                                account: Some(acc),
                                source: ctx.source,
                                status: Err(BlockError::GapSource),
                                saved_block: None,
                                channel_id: ctx.channel_id,
                            };
                        }
                    }
                }
                ProcessedResult {
                    block: ctx.block.clone(),
                    account: ctx.account,
                    source: ctx.source,
                    status: *status,
                    saved_block: block.clone(),
                    channel_id: ctx.channel_id,
                }
            })
            .collect();

        let t_send = kernel_common::clint::mtime();
        if !processed_result.is_empty() {
            // Use try_send to avoid deadlocking Hart 2 if the event consumer is busy.
            // Diagnostic events should never stall the block processor.
            let _ = self.event_publisher.try_send(LedgerEvent::BlocksProcessed(processed_result));
        }

        PROF[6].fetch_add(kernel_common::clint::mtime() - t_send, Relaxed);
        let t_loop = kernel_common::clint::mtime();
        let _ = t_loop;
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
        let result: Vec<(Result<(), BlockError>, Arc<BlockContext>)> = result
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

        let mut keep_in_unchecked = alloc::vec::Vec::with_capacity(result.len());
        let mut in_batch_failed: FastBlockHashMap<(usize, Option<rsnano_core::Amount>)> =
            FastBlockHashMap::with_capacity(result.len());

        for (status, block_ctx) in &result {
            let b = &block_ctx.block;
            let hash = b.hash();
            let prev = b.previous();
            let keep = match status {
                Ok(()) => true,
                Err(BlockError::GapPrevious) if !cfg!(test) => {
                    if let Some(&(prev_depth, prev_bal)) = in_batch_failed.get(&prev) {
                        const MAX_TAIL_DEPTH: usize = 64;
                        let missing_source = match b {
                            Block::LegacyReceive(_) | Block::LegacyOpen(_) => {
                                let source = b.source_or_link();
                                !source.is_zero()
                                    && !playable_hashes.contains_key(&source)
                                    && !self.ledger.store.block.may_exist(&source)
                            }
                            Block::State(s) => {
                                let is_receive = prev_bal.is_some_and(|pb| s.balance() >= pb);
                                let source = s.link().into();
                                is_receive
                                    && !s.link().is_zero()
                                    && !self.ledger.constants.epochs.is_epoch_link(&s.link())
                                    && !playable_hashes.contains_key(&source)
                                    && !self.ledger.store.block.may_exist(&source)
                            }
                            _ => false,
                        };
                        if prev_depth < MAX_TAIL_DEPTH {
                            let next_depth = if missing_source {
                                usize::MAX
                            } else {
                                prev_depth + 1
                            };
                            in_batch_failed.insert(hash, (next_depth, b.balance_field()));
                            true
                        } else {
                            in_batch_failed.insert(hash, (usize::MAX, b.balance_field()));
                            false
                        }
                    } else {
                        in_batch_failed.insert(hash, (0, b.balance_field()));
                        true
                    }
                }
                Err(_) => {
                    in_batch_failed.insert(hash, (0, b.balance_field()));
                    true
                }
            };
            keep_in_unchecked.push(keep);
        }

        for ((status, block_ctx), &keep) in result.iter().zip(&keep_in_unchecked) {
            if !keep {
                continue;
            }
            let hash = &block_ctx.block.hash();
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
                Err(BlockError::GapPrevious)
                | Err(BlockError::GapSource)
                | Err(BlockError::GapEpochOpenPending) => {
                    // Inserted in reverse order after the loop so chain heads (GapSource / B_1)
                    // receive higher sequence numbers than chain tails (B_2..B_k) and are evicted last.
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

        // Insert unchecked blocks in reverse batch order so that if UncheckedMap ever overflows
        // and evicts via FIFO (pop_front), tail blocks (B_k..B_2) are evicted before the head block (B_1).
        for ((status, block_ctx), &keep) in result.iter().zip(&keep_in_unchecked).rev() {
            if !keep {
                continue;
            }
            let block = &block_ctx.block;
            match status {
                Err(BlockError::GapPrevious) => {
                    self.unchecked
                        .put(block.previous().into(), UncheckedInfo::new(block.clone()));
                }
                Err(BlockError::GapSource) => {
                    self.unchecked.put(
                        block
                            .source_field()
                            .unwrap_or(block.link_field().unwrap_or_default().into())
                            .into(),
                        UncheckedInfo::new(block.clone()),
                    );
                }
                Err(BlockError::GapEpochOpenPending) => {
                    self.unchecked.put(
                        block.account_field().unwrap().into(),
                        UncheckedInfo::new(block.clone()),
                    );
                }
                _ => {}
            }
        }

        PROF[2].fetch_add(kernel_common::clint::mtime() - t2, Relaxed);
        PROF[7].fetch_add(kernel_common::clint::mtime() - t_loop, Relaxed);
        // Set results for futures when not holding the lock
        for idx in 0..result.len() {
            let res = result[idx].0;
            let cb_res = if !cfg!(test)
                && result[idx].1.callback.is_some()
                && result[idx].1.source == BlockSource::Bootstrap
            {
                let acc = result[idx].1.account;
                let has_source_gap = acc.is_some()
                    && result.iter().any(|(s, c)| {
                        c.account == acc
                            && matches!(
                                s,
                                Err(BlockError::GapSource) | Err(BlockError::GapEpochOpenPending)
                            )
                    });
                let has_ok = acc.is_some()
                    && result.iter().any(|(s, c)| c.account == acc && s.is_ok());
                let all_old = acc.is_some()
                    && result
                        .iter()
                        .filter(|(_, c)| c.account == acc)
                        .all(|(s, _)| matches!(s, Err(BlockError::Old)));
                if has_source_gap {
                    Err(BlockError::GapSource)
                } else if res.is_ok() || (has_ok && matches!(res, Err(BlockError::Old))) {
                    Ok(())
                } else if all_old {
                    Err(BlockError::Old)
                } else {
                    res
                }
            } else {
                res
            };
            if let Some(cb) = &result[idx].1.callback {
                cb(cb_res);
            }
            result[idx].1.set_result(res);
        }
    }

    fn roll_back_competitor_blocks(&self, batch: &VecDeque<Arc<BlockContext>>) {
        if !batch.iter().any(|i| i.source == BlockSource::Forced) {
            return;
        }
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
