use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::sync::atomic::{ AtomicBool, Ordering };
use core::cmp::{max, min};


use rsnano_messages::Message;
use rsnano_network::{Channel, ChannelId};
use rsnano_network_protocol::InboundMessageQueue;

use super::RealtimeMessageHandler;
use crate::config::NodeConfig;

#[derive(Clone, Debug, PartialEq)]
pub struct MessageProcessorConfig {
    pub threads: usize,
    pub max_queue: usize,
}

impl MessageProcessorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            threads: min(2, max(parallelism / 4, 1)),
            max_queue: 64,
        }
    }
}

/// Process inbound messages from other nodes
pub struct MessageProcessor {
    state: Arc<State>,
}

impl MessageProcessor {
    pub fn new(
        _config: NodeConfig,
        inbound_queue: Arc<InboundMessageQueue>,
        realtime_handler: Arc<RealtimeMessageHandler>,
    ) -> Self {
        Self {
            state: Arc::new(State {
                inbound_queue,
                realtime_handler,
                stopped: AtomicBool::new(false),
                total_processed: core::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    pub fn start(&mut self) {
        // Dedicated worker harts (Harts 2 & 3) drive `run_batch` directly so inbound message
        // processing never blocks the network polling hart (Hart 1) on the shared async executor.
    }

    pub fn stop(&mut self) {
        self.state.stopped.store(true, Ordering::SeqCst);
        self.state.inbound_queue.stop();
    }
    
    pub fn run(&self) {
        while !self.state.stopped.load(Ordering::SeqCst) {
            let batch = self.state.inbound_queue.next_batch(State::MAX_BATCH_SIZE);
            if !batch.is_empty() {
                self.state.handle_batch(batch);
            } else {
                core::hint::spin_loop();
            }
        }
    }

    pub fn run_once(&self) {
        if self.state.stopped.load(Ordering::SeqCst) {
            return;
        }
        let batch = self.state.inbound_queue.next_batch(State::MAX_BATCH_SIZE);
        if !batch.is_empty() {
            self.state.handle_batch(batch);
        }
    }

    /// Pops and processes one batch while holding the `MessageProcessor` lock so that
    /// consecutive batches from the same TCP peer are pushed to `BlockProcessorQueue` in FIFO order.
    pub fn run_batch(mp: &spin::Mutex<Self>) -> bool {
        let Some(guard) = mp.try_lock() else {
            return false;
        };
        if guard.state.inbound_queue.is_empty_fast() || guard.state.stopped.load(Ordering::Relaxed)
        {
            return false;
        }
        let batch = guard.state.inbound_queue.next_batch(State::MAX_BATCH_SIZE);
        if batch.is_empty() {
            return false;
        }
        guard.state.handle_batch(batch);
        true
    }

    pub fn queue_len(&self) -> usize {
        self.state.inbound_queue.size()
    }

    pub fn total_processed(&self) -> u64 {
        self.state.total_processed.load(Ordering::Relaxed)
    }
}

impl Drop for MessageProcessor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Diagnostics: per message class [AscPullAck, Publish, ConfirmAck, ConfirmReq, Keepalive,
/// AscPullReq, other]: entries 0..7 are counts, 7..14 are CLINT ticks spent in `process`.
pub static MSG_PROF: [core::sync::atomic::AtomicU64; 14] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 14];

fn msg_class(t: &rsnano_messages::Message) -> usize {
    use rsnano_messages::Message;
    match t {
        Message::AscPullAck(_) => 0,
        Message::Publish(_) => 1,
        Message::ConfirmAck(_) => 2,
        Message::ConfirmReq(_) => 3,
        Message::Keepalive(_) => 4,
        Message::AscPullReq(_) => 5,
        _ => 6,
    }
}

struct State {
    stopped: AtomicBool,
    realtime_handler: Arc<RealtimeMessageHandler>,
    inbound_queue: Arc<InboundMessageQueue>,
    total_processed: core::sync::atomic::AtomicU64,
}

impl State {
    const MAX_BATCH_SIZE: usize = 256;

    fn handle_batch(&self, batch: VecDeque<(ChannelId, (Message, Arc<Channel>))>) {
        let _start = kernel_common::clint::mtime();
        let batch_size = batch.len();
        let mut publish_batch = Vec::new();

        for (_, (message, channel)) in batch {
            match message {
                Message::Publish(publish) => {
                    publish_batch.push((publish, channel.channel_id()));
                }
                other => {
                    if !publish_batch.is_empty() {
                        let count = publish_batch.len() as u64;
                        let t0 = kernel_common::clint::mtime();
                        self.realtime_handler
                            .process_publish_batch(core::mem::take(&mut publish_batch));
                        let dt = kernel_common::clint::mtime() - t0;
                        MSG_PROF[1].fetch_add(count, Ordering::Relaxed);
                        MSG_PROF[7 + 1].fetch_add(dt, Ordering::Relaxed);
                    }
                    let class = msg_class(&other);
                    let t0 = kernel_common::clint::mtime();
                    self.realtime_handler.process(other, &channel);
                    let dt = kernel_common::clint::mtime() - t0;
                    MSG_PROF[class].fetch_add(1, Ordering::Relaxed);
                    MSG_PROF[7 + class].fetch_add(dt, Ordering::Relaxed);
                }
            }
        }
        if !publish_batch.is_empty() {
            let count = publish_batch.len() as u64;
            let t0 = kernel_common::clint::mtime();
            self.realtime_handler.process_publish_batch(publish_batch);
            let dt = kernel_common::clint::mtime() - t0;
            MSG_PROF[1].fetch_add(count, Ordering::Relaxed);
            MSG_PROF[7 + 1].fetch_add(dt, Ordering::Relaxed);
        }
        self.total_processed.fetch_add(batch_size as u64, Ordering::Relaxed);
    }
}
