use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{ Mutex };
use crate::utils::Condvar;
use core::time::Duration;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider, FairQueueInfo},
    Block, SavedBlock,
};
use rsnano_ledger::BlockError;
use rsnano_network::{ChannelId, DeadChannelCleanupStep};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_stats::{StatsCollection, StatsSource};

use super::{
    process_queue::{ProcessQueue, ProcessQueueConfig},
    BlockContext, BlockSource,
};
use strum::{EnumCount, IntoEnumIterator};

pub struct BlockProcessorQueue {
    queue: Mutex<BlockProcessorQueueImpl>,
    condition: Condvar,
    is_nulled: bool,
    wait_listener: OutputListenerMt<Duration>,
    signature_checker: Mutex<Option<Arc<crate::signatures::SignatureChecker>>>,
    approx_len: core::sync::atomic::AtomicUsize,
}

impl BlockProcessorQueue {
    pub fn new(config: ProcessQueueConfig) -> Self {
        Self {
            queue: Mutex::new(BlockProcessorQueueImpl::new(config)),
            condition: Condvar::new(),
            is_nulled: false,
            wait_listener: OutputListenerMt::new(),
            signature_checker: Mutex::new(None),
            approx_len: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn set_signature_checker(
        &self,
        checker: Arc<crate::signatures::SignatureChecker>,
    ) {
        *self.signature_checker.lock() = Some(checker);
    }

    pub fn new_null() -> Self {
        Self::new_null_with(Vec::new())
    }

    pub fn new_null_with(blocks: Vec<Arc<BlockContext>>) -> Self {
        let mut queue_impl = BlockProcessorQueueImpl::new(Default::default());
        for ctx in blocks {
            queue_impl.push(ctx);
        }
        let initial_len = queue_impl.process_queue.len();
        if queue_impl.process_queue.is_empty() {
            queue_impl.stopped = true;
        }
        Self {
            queue: Mutex::new(queue_impl),
            condition: Condvar::new(),
            is_nulled: true,
            wait_listener: OutputListenerMt::new(),
            signature_checker: Mutex::new(None),
            approx_len: core::sync::atomic::AtomicUsize::new(initial_len),
        }
    }

    pub fn track_waits(&self) -> Arc<OutputTrackerMt<Duration>> {
        self.wait_listener.track()
    }

    pub fn stop(&self) {
        {
            let mut queue = self.queue.lock();
            queue.stopped = true;
        }
        self.condition.notify_all();
    }

    pub fn stopped(&self) -> bool {
        self.queue.lock().stopped
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.queue.lock().cool_down = cool_down;
        self.condition.notify_all();
    }

    pub fn is_cooling_down(&self) -> bool {
        self.queue.lock().cool_down
    }

    pub fn wait(&self, duration: Duration) {
        self.wait_listener.emit(duration);

        if self.is_nulled {
            return;
        }

        let guard = self.queue.lock();
        let _ = self
            .condition
            .wait_timeout_while(guard, duration, |i| !i.stopped);
    }

    pub(crate) fn pop(&self) -> Option<VecDeque<Arc<BlockContext>>> {
        if !cfg!(test) {
            let len = self.approx_len.load(core::sync::atomic::Ordering::Relaxed);
            if len == 0 {
                return None;
            }
            if len < 64 {
                if let Some(checker) = self.signature_checker.lock().as_ref() {
                    if checker.bg_len() >= 16 {
                        return None;
                    }
                }
            }
        }
        let mut queue = self.queue.lock();
        if queue.stopped {
            return None;
        }

        if queue.cool_down {
            return None;
        }

        let batch = queue.process_queue.next_batch();
        self.approx_len.store(
            queue.process_queue.len(),
            core::sync::atomic::Ordering::Release,
        );
        if !batch.is_empty() {
            return Some(batch);
        }
        None
    }

    // TODO: Remove and replace all checks with calls to size (block_source)
    pub fn total_queue_len(&self) -> usize {
        self.queue.lock().process_queue.len()
    }

    pub fn queue_len(&self, source: BlockSource) -> usize {
        self.queue.lock().process_queue.source_len(source)
    }

    pub fn push(&self, context: impl Into<Arc<BlockContext>>) -> bool {
        let context: Arc<BlockContext> = context.into();
        let hash = context.block.hash();
        let added = {
            let mut guard = self.queue.lock();
            if guard.can_push(context.source, context.channel_id, &hash) {
                let ok = guard.push(context.clone());
                self.approx_len.store(
                    guard.process_queue.len(),
                    core::sync::atomic::Ordering::Release,
                );
                ok
            } else {
                if !guard.has_capacity(context.source, context.channel_id) {
                    guard.record_overfill(context.source);
                }
                false
            }
        };

        if added {
            if let Some(checker) = self.signature_checker.lock().as_ref() {
                checker.enqueue_bg(&context);
            }
            self.condition.notify_one();
        }

        added
    }

    pub fn push_batch(&self, contexts: impl IntoIterator<Item = BlockContext>) {
        let arc_ctxs: Vec<Arc<BlockContext>> = contexts.into_iter().map(Arc::new).collect();
        if arc_ctxs.is_empty() {
            return;
        }
        if let Some(checker) = self.signature_checker.lock().as_ref() {
            checker.enqueue_bg_batch(&arc_ctxs);
        }
        let mut any_added = false;
        {
            let mut guard = self.queue.lock();
            for arc_ctx in arc_ctxs {
                let source = arc_ctx.source;
                let channel_id = arc_ctx.channel_id;
                if guard.has_capacity(source, channel_id) {
                    if guard.push(arc_ctx) {
                        any_added = true;
                    }
                } else {
                    guard.record_overfill(source);
                }
            }
            self.approx_len.store(
                guard.process_queue.len(),
                core::sync::atomic::Ordering::Release,
            );
        }
        if any_added {
            self.condition.notify_one();
        }
    }

    pub fn push_batch_with_digests(
        &self,
        items: Vec<(BlockContext, u128)>,
        rejected_digests: &mut Vec<u128>,
    ) {
        if items.is_empty() {
            return;
        }
        let arc_items: Vec<(Arc<BlockContext>, u128)> = items
            .into_iter()
            .map(|(ctx, digest)| (Arc::new(ctx), digest))
            .collect();
        if let Some(checker) = self.signature_checker.lock().as_ref() {
            let mut lock = checker.bg_tasks_lock();
            for (ctx, _) in &arc_items {
                crate::signatures::SignatureChecker::enqueue_bg_locked(
                    &mut lock,
                    checker.bg_count_atomic(),
                    ctx,
                );
            }
        }
        let mut any_added = false;
        {
            let mut guard = self.queue.lock();
            for (arc_ctx, digest) in arc_items {
                let source = arc_ctx.source;
                let channel_id = arc_ctx.channel_id;
                if guard.has_capacity(source, channel_id) {
                    if guard.push(arc_ctx) {
                        any_added = true;
                    }
                } else {
                    guard.record_overfill(source);
                    rejected_digests.push(digest);
                }
            }
            self.approx_len.store(
                guard.process_queue.len(),
                core::sync::atomic::Ordering::Release,
            );
        }
        if any_added {
            self.condition.notify_one();
        }
    }

    pub fn try_push(&self, context: impl Into<Arc<BlockContext>>) -> Result<bool, ()> {
        let context = context.into();
        if let Some(mut queue) = self.queue.try_lock() {
            let added = queue.push(context.clone());
            self.approx_len.store(
                queue.process_queue.len(),
                core::sync::atomic::Ordering::Release,
            );
            drop(queue);
            if added {
                if let Some(checker) = self.signature_checker.lock().as_ref() {
                    checker.enqueue_bg(&context);
                }
                self.condition.notify_one();
            }
            Ok(added)
        } else {
            Err(())
        }
    }

    pub fn push_blocking(
        &self,
        block: Arc<Block>,
        source: BlockSource,
    ) -> anyhow::Result<Result<SavedBlock, BlockError>> {
        let channel_id = ChannelId::LOOPBACK;

        let (ctx_inner, waiter) = BlockContext::new_blocking(
            block.as_ref().clone(),
            source,
            channel_id,
        );
        let ctx = Arc::new(ctx_inner);
        self.push(ctx.clone());

        match waiter.wait_result() {
            Some(Ok(())) => Ok(Ok(ctx.saved_block.lock().clone().unwrap())),
            Some(Err(e)) => Ok(Err(e)),
            None => {
                self.queue.lock().timeout += 1;
                Err(anyhow!("Block dropped when processing"))
            }
        }
    }

    pub fn info(&self) -> FairQueueInfo<BlockSource> {
        self.queue.lock().process_queue.info()
    }

    pub(crate) fn try_lock_internal(&self) -> Option<spin::MutexGuard<'_, BlockProcessorQueueImpl>> {
        self.queue.try_lock()
    }
}

impl Default for BlockProcessorQueue {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl DeadChannelCleanupStep for BlockProcessorQueue {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut guard = self.queue.lock();
        for channel_id in dead_channel_ids {
            let iter = BlockSource::iter();
            for source in iter {
                guard.process_queue.remove(source, *channel_id)
            }
        }
    }
}

impl StatsSource for BlockProcessorQueue {
    fn collect_stats(&self, result: &mut StatsCollection) {
        if let Some(guard) = self.queue.try_lock() {
            guard.collect_stats(result);
        }
    }
}

impl ContainerInfoProvider for BlockProcessorQueue {
    fn container_info(&self) -> ContainerInfo {
        #[cfg(test)]
        let Some(guard) = Some(self.queue.lock()) else {
            unreachable!()
        };
        #[cfg(not(test))]
        let Some(guard) = self.queue.try_lock() else {
            return ContainerInfo::default();
        };
        guard.process_queue.container_info()
    }
}

pub(crate) struct BlockProcessorQueueImpl {
    process_queue: ProcessQueue,
    stopped: bool,
    cool_down: bool,
    processed: u64,
    overfill_count: u64,
    overfill_by_source: [u64; BlockSource::COUNT],
    timeout: u64,
}

impl BlockProcessorQueueImpl {
    pub fn new(config: ProcessQueueConfig) -> Self {
        Self {
            process_queue: ProcessQueue::new(config),
            stopped: false,
            cool_down: false,
            processed: 0,
            overfill_count: 0,
            overfill_by_source: Default::default(),
            timeout: 0,
        }
    }

    #[allow(dead_code)]
    pub fn should_wait(&self) -> bool {
        if self.stopped {
            return false;
        }

        if self.cool_down {
            return true;
        }

        self.process_queue.is_empty()
    }

    pub fn total_queue_len(&self) -> usize {
        self.process_queue.len()
    }

    pub fn can_push(
        &self,
        source: BlockSource,
        channel_id: ChannelId,
        hash: &rsnano_core::BlockHash,
    ) -> bool {
        !self.stopped && self.process_queue.can_push(source, channel_id, hash)
    }

    pub fn has_capacity(&self, source: BlockSource, channel_id: ChannelId) -> bool {
        !self.stopped && self.process_queue.has_capacity(source, channel_id)
    }

    pub fn record_overfill(&mut self, source: BlockSource) {
        self.overfill_count += 1;
        self.overfill_by_source[source as usize] += 1;
    }

    pub fn push(&mut self, context: Arc<BlockContext>) -> bool {
        if self.stopped {
            return false;
        }

        let source = context.source;
        let channel_id = context.channel_id;
        let added = self.process_queue.push(context);

        if added {
            self.processed += 1;
        } else if !self.process_queue.has_capacity(source, channel_id) {
            self.overfill_count += 1;
            self.overfill_by_source[source as usize] += 1;
        }

        added
    }
}

impl StatsSource for BlockProcessorQueueImpl {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("block_processor", "process", self.processed);
        result.insert("block_processor", "overfill", self.overfill_count);
        for i in BlockSource::iter() {
            result.insert(
                "block_processor_overfill",
                i.into(),
                self.overfill_by_source[i as usize],
            );
        }
        result.insert("block_processor", "process_blocking_timeout", self.timeout);
    }
}
