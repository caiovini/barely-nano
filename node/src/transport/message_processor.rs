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
        // let start = Instant::now();
        let _batch_size = batch.len();
        for (_, (message, channel)) in batch {
            self.realtime_handler.process(message, &channel);
        }

        // let elapsed_millis = start.elapsed().as_millis();
        // if elapsed_millis > 100 {
        //     debug!(
        //         "Processed {} messages in {} milliseconds (rate of {} messages per second)",
        //         batch_size,
        //         elapsed_millis,
        //         batch_size as u128 * 1000 / elapsed_millis
        //     );
        // }
    }
}
