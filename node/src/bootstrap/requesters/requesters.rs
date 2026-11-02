use crate::utils::Condvar;
use alloc::{sync::Arc, vec::Vec};
use spin::{Mutex, RwLock};

use rsnano_ledger::Ledger;
use rsnano_network::token_bucket::TokenBucket;
use rsnano_network::Network;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{Stats, StatsCollection, StatsSource};

use super::{
    channel_waiter::ChannelWaiter, dependency_requester::DependencyRequester,
    frontier_requester::FrontierRequester, priority::PriorityRequester,
    query_sender::QuerySender, send_queries_promise::SendQueriesPromise,
};
use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::{
        state::BootstrapState, AscPullQuerySpec, BootstrapConfig, BootstrapPromise, PollResult,
    },
    transport::MessageSender,
};

/// Manages the async tasks that send out AscPullReqs
pub(crate) struct Requesters {
    limiter: Arc<Mutex<TokenBucket>>,
    config: BootstrapConfig,
    stats: Arc<Stats>,
    message_sender: MessageSender,
    state: Arc<Mutex<BootstrapState>>,
    state_changed: Arc<Condvar>,
    clock: Arc<SteadyClock>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    pub(crate) network: Arc<RwLock<Network>>,
    stats_sources: Mutex<Vec<Arc<dyn StatsSource + Send + Sync>>>,
}

impl Requesters {
    pub(crate) fn new(
        limiter: Arc<Mutex<TokenBucket>>,
        config: BootstrapConfig,
        stats: Arc<Stats>,
        message_sender: MessageSender,
        state: Arc<Mutex<BootstrapState>>,
        state_changed: Arc<Condvar>,
        clock: Arc<SteadyClock>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        network: Arc<RwLock<Network>>,
    ) -> Self {
        Self {
            limiter,
            config,
            stats,
            message_sender,
            state,
            state_changed,
            clock,
            ledger,
            block_processor_queue,
            network,
            stats_sources: Mutex::new(Vec::new()),
        }
    }

    pub fn start(&self) {
        let limiter = self.limiter.clone();
        let max_requests = self.config.max_requests;
        let channel_waiter =
            ChannelWaiter::new(self.network.clone(), limiter.clone(), max_requests);

        if self.config.enable_frontier_scan {
            self.spawn_query(
                "Bootstrap front",
                FrontierRequester::new(
                    self.stats.clone(),
                    self.clock.clone(),
                    self.config.frontier_rate_limit,
                    channel_waiter.clone(),
                ),
            );
        }

        if self.config.enable_priorities {
            let mut requester = PriorityRequester::new(
                self.block_processor_queue.clone(),
                channel_waiter.clone(),
                self.clock.clone(),
                self.ledger.clone(),
                &self.config,
            );
            requester.block_processor_threshold = self.config.block_processor_theshold;
            self.stats_sources.lock().push(requester.stats());

            self.spawn_priorities("Bootstrap", requester);
        }

        if self.config.enable_dependency_walker {
            self.spawn_query(
                "Bootstrap walker",
                DependencyRequester::new(self.stats.clone(), channel_waiter),
            );
        }
    }

    pub fn stop(&self) {
        self.state.lock().stopped = true;
        self.state_changed.notify_all();
    }

    fn spawn_priorities(&self, name: &'static str, mut requester: PriorityRequester) {
        tracing::info!("Spawning bootstrap requester: {}", name);
        let message_sender = self.message_sender.clone();
        let clock = self.clock.clone();
        let stats = self.stats.clone();
        let state_arc = self.state.clone();
        let request_timeout = self.config.request_timeout;
        kernel_common::async_rt::spawn_named(
            async move {
                let mut query_sender = QuerySender::new(message_sender, clock.clone(), stats);
                query_sender.set_request_timeout(request_timeout);

                loop {
                    if state_arc.lock().stopped {
                        break;
                    }

                    let mut sent_any = false;
                    for _ in 0..64 {
                        let now = clock.now();
                        let result = requester.poll_step(&state_arc, now);

                        match result {
                            PollResult::Finished(spec) => {
                                let mut state = state_arc.lock();
                                query_sender.send(spec, &mut state);
                                sent_any = true;
                            }
                            PollResult::Wait => break,
                            PollResult::Progress => {
                                sent_any = true;
                            }
                        }
                    }

                    if sent_any {
                        kernel_common::async_rt::yield_now().await;
                    } else {
                        kernel_common::async_rt::sleep(core::time::Duration::from_millis(2)).await;
                    }
                }
            },
            Some(name),
        );
    }

    fn spawn_query<T>(&self, name: &'static str, query_factory: T)
    where
        T: BootstrapPromise<AscPullQuerySpec> + Send + 'static,
    {
        tracing::info!("Spawning bootstrap requester: {}", name);
        let message_sender = self.message_sender.clone();
        let clock = self.clock.clone();
        let stats = self.stats.clone();
        let state_arc = self.state.clone();
        let request_timeout = self.config.request_timeout;
        kernel_common::async_rt::spawn_named(
            async move {
                let mut query_sender = QuerySender::new(message_sender, clock.clone(), stats);
                query_sender.set_request_timeout(request_timeout);
                let mut promise = SendQueriesPromise::new(query_factory, query_sender);

                loop {
                    if state_arc.lock().stopped {
                        break;
                    }

                    let mut sent_any = false;
                    for _ in 0..32 {
                        let now = clock.now();
                        let result = {
                            let mut state = state_arc.lock();
                            promise.poll(&mut state, now)
                        };

                        match result {
                            PollResult::Finished(_) => {}
                            PollResult::Wait => break,
                            PollResult::Progress => {
                                sent_any = true;
                            }
                        }
                    }

                    if sent_any {
                        kernel_common::async_rt::yield_now().await;
                    } else {
                        kernel_common::async_rt::sleep(core::time::Duration::from_millis(2)).await;
                    }
                }
            },
            Some(name),
        );
    }
}

impl StatsSource for Requesters {
    fn collect_stats(&self, result: &mut StatsCollection) {
        for s in self.stats_sources.lock().iter() {
            s.collect_stats(result);
        }
    }
}
