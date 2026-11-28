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

const FAST_HASH_SLOTS: usize = 4096;
const FAST_HASH_MASK: usize = FAST_HASH_SLOTS - 1;
const FAST_HASH_MAX_INDEXED: usize = 3072;

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
        let mut any_unclaimed = false;
        let mut any_in_progress = false;
        for ctx in &batch {
            if !ctx.block.is_signature_verified() && ctx.source != BlockSource::Local {
                let st = ctx.sig_state.load(core::sync::atomic::Ordering::Acquire);
                if st == crate::block_processing::SIG_STATE_UNCLAIMED {
                    any_unclaimed = true;
                    break;
                } else if st != crate::block_processing::SIG_STATE_DONE {
                    any_in_progress = true;
                }
            }
        }
        let any_unverified = any_unclaimed && cfg!(test);
        let mut playable_hashes: FastBlockHashMap<Option<rsnano_core::Amount>> =
            FastBlockHashMap::with_capacity(if any_unverified { batch_len } else { 0 });
        let mut verification_tasks = alloc::vec::Vec::new();

        if any_unclaimed && !cfg!(test) {
            let mut to_verify = alloc::vec::Vec::with_capacity(64);
            for ctx in &batch {
                let b = &ctx.block;
                if b.is_signature_verified()
                    || ctx.source == BlockSource::Local
                    || ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                        != crate::block_processing::SIG_STATE_UNCLAIMED
                {
                    continue;
                }
                let verify_account = b
                    .link_field()
                    .and_then(|link| self.ledger.constants.epochs.epoch_signer(&link))
                    .or_else(|| b.account_field())
                    .or(ctx.account)
                    .filter(|a| !a.is_zero());
                if verify_account.is_some() {
                    to_verify.push((ctx.clone(), verify_account));
                } else {
                    ctx.sig_state.store(
                        crate::block_processing::SIG_STATE_DONE,
                        core::sync::atomic::Ordering::Release,
                    );
                }
            }
            if !to_verify.is_empty() {
                verification_tasks = self.signature_checker.push_batch_front(to_verify);
            }
        } else if any_unclaimed {
            let mut to_verify = alloc::vec::Vec::with_capacity(batch_len.min(32));
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
                let already_verified = b.is_signature_verified()
                    || ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                        == crate::block_processing::SIG_STATE_DONE
                    || ctx.source == BlockSource::Local;

                if !prev.is_zero()
                    && !playable_hashes.contains_key(&prev)
                    && !self.ledger.store.block.may_exist(&prev)
                {
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

                if !already_verified
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
                    if to_verify.len() >= 16 {
                        let chunk = self
                            .signature_checker
                            .push_batch_front(core::mem::take(&mut to_verify));
                        verification_tasks.extend(chunk);
                    }
                }

                if !gap_source {
                    playable_hashes.insert(hash, b.balance_field());
                }
            }
            if !to_verify.is_empty() {
                let chunk = self.signature_checker.push_batch_front(to_verify);
                verification_tasks.extend(chunk);
            }
        }

        let verification_count = verification_tasks.len();
        if cfg!(test) && !verification_tasks.is_empty() {
            while verification_tasks.iter().any(|ctx| {
                ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                    != crate::block_processing::SIG_STATE_DONE
                    && !ctx.block.is_signature_verified()
            }) {
                if self.signature_checker.help_verify_sync() > 0 {
                    continue;
                }
                core::hint::spin_loop();
            }
        }

        PROF[5].fetch_add(verification_count as u64, Relaxed);
        let t1 = kernel_common::clint::mtime();
        let mut sig_wait_ticks = 0u64;
        let need_lazy_wait = !cfg!(test) && (!verification_tasks.is_empty() || any_in_progress);

        let mut result = self.ledger.process_batch_with_lazy_verifications(
            batch.iter().map(|ctx| {
                let is_valid =
                    ctx.block.is_signature_verified() || ctx.source == BlockSource::Local;
                (&ctx.block, is_valid)
            }),
            |idx, initial_verified| {
                if initial_verified || !need_lazy_wait {
                    return (initial_verified, 0);
                }
                let ctx = &batch[idx];
                if ctx.block.is_signature_verified() || ctx.source == BlockSource::Local {
                    return (true, 0);
                }
                let mut waited = 0u64;
                if ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                    != crate::block_processing::SIG_STATE_DONE
                {
                    let ts0 = kernel_common::clint::mtime();
                    while ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                        != crate::block_processing::SIG_STATE_DONE
                        && !ctx.block.is_signature_verified()
                    {
                        if self.signature_checker.sync_len() > 16
                            && self.signature_checker.help_verify_sync() > 0
                        {
                            continue;
                        }
                        core::hint::spin_loop();
                    }
                    waited = kernel_common::clint::mtime() - ts0;
                    sig_wait_ticks += waited;
                }
                (ctx.block.is_signature_verified(), waited)
            },
        );
        let t2 = kernel_common::clint::mtime();
        PROF[0].fetch_add((t1 - t0) + sig_wait_ticks, Relaxed);
        PROF[1].fetch_add((t2 - t1).saturating_sub(sig_wait_ticks), Relaxed);

        assert_eq!(result.processed.len(), batch.len());
        let mut has_gap_previous = false;
        let mut has_unchecked_err = false;
        let mut has_gap_epoch_open = false;
        let mut processed_result = alloc::vec::Vec::with_capacity(batch.len());
        let mut result_meta = alloc::vec::Vec::with_capacity(batch.len());

        for ((status, mut saved_block), block_ctx) in
            result.processed.drain(..).zip(batch.drain(..))
        {
            if saved_block.is_some() && (cfg!(test) || block_ctx.waiter.is_some()) {
                *block_ctx.saved_block.lock() = saved_block.clone();
            }
            match status {
                Err(BlockError::GapPrevious) => {
                    if !cfg!(test) {
                        has_gap_previous = true;
                    }
                    has_unchecked_err = true;
                }
                Err(BlockError::GapSource) => {
                    has_unchecked_err = true;
                }
                Err(BlockError::GapEpochOpenPending) => {
                    has_unchecked_err = true;
                    has_gap_epoch_open = true;
                }
                _ => {}
            }

            let is_epoch_open_trigger = saved_block.as_ref().is_some_and(|block| {
                block.block_type() == BlockType::LegacySend
                    || (block.block_type() == BlockType::State
                        && block.is_send()
                        && block.epoch() < Epoch::MAX)
            });
            let had_saved_block = saved_block.is_some();

            if cfg!(test)
                || !matches!(status, Err(BlockError::GapPrevious))
                || block_ctx.source == BlockSource::Live
            {
                processed_result.push(ProcessedResult {
                    block: block_ctx.block.clone(),
                    account: block_ctx.account,
                    source: block_ctx.source,
                    status,
                    saved_block: saved_block.take(),
                    channel_id: block_ctx.channel_id,
                });
            }

            result_meta.push((status, had_saved_block, is_epoch_open_trigger, block_ctx));
        }

        if !cfg!(test) && has_gap_epoch_open {
            for item in &mut processed_result {
                if matches!(item.status, Err(BlockError::GapEpochOpenPending)) {
                    let acc = item
                        .account
                        .or_else(|| item.block.account_field())
                        .unwrap_or_default();
                    if !acc.is_zero() {
                        if let Some((_, _, _, next_ctx)) =
                            result_meta.iter().find(|(_, _, _, next_ctx)| {
                                let same_acc = next_ctx.account == Some(acc)
                                    || next_ctx.block.account_field() == Some(acc);
                                let link = next_ctx.block.source_or_link();
                                same_acc
                                    && !link.is_zero()
                                    && !self.ledger.constants.epochs.is_epoch_link(&link.into())
                            })
                        {
                            *item = ProcessedResult {
                                block: next_ctx.block.clone(),
                                account: Some(acc),
                                source: item.source,
                                status: Err(BlockError::GapSource),
                                saved_block: None,
                                channel_id: item.channel_id,
                            };
                        }
                    }
                }
            }
        }

        let t_send = kernel_common::clint::mtime();
        if !processed_result.is_empty() {
            // Use try_send to avoid deadlocking Hart 2 if the event consumer is busy.
            // Diagnostic events should never stall the block processor.
            let _ = self
                .event_publisher
                .try_send(LedgerEvent::BlocksProcessed(processed_result));
        }

        PROF[6].fetch_add(kernel_common::clint::mtime() - t_send, Relaxed);
        let t_loop = kernel_common::clint::mtime();
        let _ = t_loop;
        if !result_meta.is_empty()
            && timer.elapsed(self.clock.now()) > Duration::from_millis(100)
        {
            debug!(
                "Processed {} blocks in {} ms",
                result_meta.len(),
                timer.elapsed(self.clock.now()).as_millis(),
            );
        }

        let keep_in_unchecked = if has_gap_previous {
            if !any_unverified {
                for (status, _, _, block_ctx) in &result_meta {
                    if status.is_ok() || matches!(status, Err(BlockError::Old)) {
                        playable_hashes
                            .insert(block_ctx.block.hash(), block_ctx.block.balance_field());
                    }
                }
            }
            let mut keep_vec = alloc::vec::Vec::with_capacity(result_meta.len());
            let mut in_batch_failed: FastBlockHashMap<(usize, Option<rsnano_core::Amount>)> =
                FastBlockHashMap::with_capacity(result_meta.len());

            for (status, _, _, block_ctx) in &result_meta {
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
                keep_vec.push(keep);
            }
            Some(keep_vec)
        } else {
            None
        };

        let unchecked_empty = self.unchecked.is_empty();
        let mut batch_triggers = if unchecked_empty {
            alloc::vec::Vec::new()
        } else {
            alloc::vec::Vec::with_capacity(result_meta.len() * 2)
        };
        let mut skipped_triggers = 0u64;

        for (idx, (status, had_saved_block, is_epoch_open_trigger, block_ctx)) in
            result_meta.iter().enumerate()
        {
            if let Some(ref keep_vec) = keep_in_unchecked {
                if !keep_vec[idx] {
                    continue;
                }
            }

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
                    let hash = block_ctx.block.hash();
                    if unchecked_empty {
                        skipped_triggers += 1;
                    } else {
                        batch_triggers.push(hash.into());
                    }

                    if !*had_saved_block {
                        warn!("Saved block was missing after Ok process result");
                        continue;
                    }
                    if *is_epoch_open_trigger {
                        if unchecked_empty {
                            skipped_triggers += 1;
                        } else {
                            batch_triggers.push(block_ctx.block.destination_or_link().into());
                        }
                    }
                }
                Err(BlockError::GapPrevious)
                | Err(BlockError::GapSource)
                | Err(BlockError::GapEpochOpenPending) => {
                    // Inserted in reverse order after the loop so chain heads (GapSource / B_1)
                    // receive higher sequence numbers than chain tails (B_2..B_k) and are evicted last.
                }
                Err(BlockError::Old) => {
                    debug!("Block is old: {}", block_ctx.block.hash())
                }
                Err(BlockError::Conflict) => {
                    debug!("Block conflict: {}", block_ctx.block.hash())
                }
                Err(BlockError::BadSignature) => {
                    debug!("Block signature is invalid: {}", block_ctx.block.hash())
                }
                Err(BlockError::NegativeSpend) => {
                    debug!("Block spends negative amount: {}", block_ctx.block.hash())
                }
                Err(BlockError::Unreceivable) => {
                    debug!("Block is unreceivable: {}", block_ctx.block.hash())
                }
                Err(BlockError::Fork) => {
                    debug!("Block is a fork: {}", block_ctx.block.hash())
                }
                Err(BlockError::OpenedBurnAccount) => {
                    debug!("Block opens burn account: {}", block_ctx.block.hash())
                }
                Err(BlockError::BalanceMismatch) => {
                    debug!("Block balance mismatch: {}", block_ctx.block.hash())
                }
                Err(BlockError::RepresentativeMismatch) => {
                    debug!("Block representative mismatch: {}", block_ctx.block.hash())
                }
                Err(BlockError::BlockPosition) => {
                    debug!("Block is in incorrect position: {}", block_ctx.block.hash())
                }
                Err(BlockError::InsufficientWork) => {
                    debug!("Block has insufficient work: {}", block_ctx.block.hash())
                }
            }
        }

        if skipped_triggers > 0 {
            self.unchecked.record_triggers(skipped_triggers);
        }
        if !batch_triggers.is_empty() {
            self.unchecked.trigger_batch(&batch_triggers);
        }

        // Insert unchecked blocks in reverse batch order so that if UncheckedMap ever overflows
        // and evicts via FIFO (pop_front), tail blocks (B_k..B_2) are evicted before the head block (B_1).
        if has_unchecked_err {
            if !cfg!(test) && !verification_tasks.is_empty() {
                while verification_tasks.iter().any(|ctx| {
                    ctx.sig_state.load(core::sync::atomic::Ordering::Acquire)
                        != crate::block_processing::SIG_STATE_DONE
                        && !ctx.block.is_signature_verified()
                }) {
                    if self.signature_checker.sync_len() > 16
                        && self.signature_checker.help_verify_sync() > 0
                    {
                        continue;
                    }
                    core::hint::spin_loop();
                }
            }
            for (idx, (status, _, _, block_ctx)) in result_meta.iter().enumerate().rev() {
                if let Some(ref keep_vec) = keep_in_unchecked {
                    if !keep_vec[idx] {
                        continue;
                    }
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
        }

        PROF[2].fetch_add(kernel_common::clint::mtime() - t2, Relaxed);
        PROF[7].fetch_add(kernel_common::clint::mtime() - t_loop, Relaxed);
        // Set results for futures when not holding the lock
        for idx in 0..result_meta.len() {
            let res = result_meta[idx].0;
            let cb_res = if !cfg!(test)
                && result_meta[idx].3.callback.is_some()
                && result_meta[idx].3.source == BlockSource::Bootstrap
            {
                let acc = result_meta[idx].3.account;
                let has_source_gap = acc.is_some()
                    && result_meta.iter().any(|(s, _, _, c)| {
                        c.account == acc
                            && matches!(
                                s,
                                Err(BlockError::GapSource) | Err(BlockError::GapEpochOpenPending)
                            )
                    });
                let has_ok = acc.is_some()
                    && result_meta
                        .iter()
                        .any(|(s, _, _, c)| c.account == acc && s.is_ok());
                let all_old = acc.is_some()
                    && result_meta
                        .iter()
                        .filter(|(_, _, _, c)| c.account == acc)
                        .all(|(s, _, _, _)| matches!(s, Err(BlockError::Old)));
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
            if let Some(cb) = &result_meta[idx].3.callback {
                cb(cb_res);
            }
            result_meta[idx].3.set_result(res);
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
