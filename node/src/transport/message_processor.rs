use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use crate::utils::JoinHandle;
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
    config: NodeConfig,
    processing_threads: Vec<JoinHandle<()>>,
    state: Arc<State>,
}

impl MessageProcessor {
    pub fn new(
        config: NodeConfig,
        inbound_queue: Arc<InboundMessageQueue>,
        realtime_handler: Arc<RealtimeMessageHandler>,
    ) -> Self {
        Self {
            config,
            processing_threads: Vec::new(),
            state: Arc::new(State {
                inbound_queue,
                realtime_handler,
                stopped: AtomicBool::new(false),
                total_processed: core::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    pub fn start(&mut self) {
        let state = self.state.clone();
        kernel_common::async_rt::spawn(async move {
            state.run().await;
        });
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

struct State {
    stopped: AtomicBool,
    realtime_handler: Arc<RealtimeMessageHandler>,
    inbound_queue: Arc<InboundMessageQueue>,
    total_processed: core::sync::atomic::AtomicU64,
}

impl State {
    const MAX_BATCH_SIZE: usize = 1024 * 4;

    async fn run(&self) {
        while !self.stopped.load(Ordering::SeqCst) {
            let batch = self.inbound_queue.next_batch(Self::MAX_BATCH_SIZE);
            if !batch.is_empty() {
                self.handle_batch(batch);
            } else {
                kernel_common::async_rt::sleep(core::time::Duration::from_millis(10)).await;
            }
        }
    }

    #[allow(dead_code)]
    fn handle_batch(&self, batch: VecDeque<(ChannelId, (Message, Arc<Channel>))>) {
        let start = kernel_common::clint::mtime();
        let batch_size = batch.len();
        
        kernel_common::uart::putchar(b'~');

        for (_, (message, channel)) in batch {
            self.realtime_handler.process(message, &channel);
        }
        self.total_processed.fetch_add(batch_size as u64, Ordering::Relaxed);
        
        kernel_common::uart::putchar(b'}');
    }
}
