use crate::utils::{Condvar, JoinHandle};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, RwLock};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{Stats, StatsCollection, StatsSource};

use super::{
    bootstrap_promise_runner::BootstrapPromiseRunner, channel_waiter::ChannelWaiter,
    dependency_requester::DependencyRequester, frontier_requester::FrontierRequester,
    priority::PriorityRequester, query_sender::QuerySender,
    send_queries_promise::SendQueriesPromise,
};
use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::{
        AscPullQuerySpec, BootstrapConfig, BootstrapPromise, PollResult, state::BootstrapState,
    },
    transport::MessageSender,
};

/// Manages the threads that send out AscPullReqs
pub(crate) struct Requesters {
    limiter: Arc<Mutex<TokenBucket>>,
    config: BootstrapConfig,
    stats: Arc<Stats>,
    message_sender: MessageSender,
    state: Arc<Mutex<BootstrapState>>,
    state_changed: Arc<Condvar>,
    clock: Arc<SteadyClock>,
    threads: Mutex<Option<RequesterThreads>>,
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
            threads: Mutex::new(None),
            stats_sources: Mutex::new(Vec::new()),
        }
    }

    pub fn start(&self) {
        let limiter = self.limiter.clone();
        let max_requests = self.config.max_requests;
        let channel_waiter =
            ChannelWaiter::new(self.network.clone(), limiter.clone(), max_requests);

        let runner = Arc::new(BootstrapPromiseRunner {
            state: self.state.clone(),
            throttle_wait: self.config.throttle_wait,
            state_changed: self.state_changed.clone(),
            clock: self.clock.clone(),
        });

        let frontiers = if self.config.enable_frontier_scan {
            Some(self.spawn_query(
                "Bootstrap front",
                FrontierRequester::new(
                    self.stats.clone(),
                    self.clock.clone(),
                    self.config.frontier_rate_limit,
                    channel_waiter.clone(),
                ),
                runner.clone(),
            ))
        } else {
            None
        };

        let priorities = if self.config.enable_priorities {
            let mut requester = PriorityRequester::new(
                self.block_processor_queue.clone(),
                channel_waiter.clone(),
                self.clock.clone(),
                self.ledger.clone(),
                &self.config,
            );
            requester.block_processor_threshold = self.config.block_processor_theshold;
            self.stats_sources.lock().push(requester.stats());

            Some(self.spawn_query("Bootstrap", requester, runner.clone()))
        } else {
            None
        };

        let dependencies = if self.config.enable_dependency_walker {
            Some(self.spawn_query(
                "Bootstrap walker",
                DependencyRequester::new(self.stats.clone(), channel_waiter),
                runner.clone(),
            ))
        } else {
            None
        };

        let requesters = RequesterThreads {
            priorities,
            frontiers,
            dependencies,
        };

        *self.threads.lock() = Some(requesters);
    }

    pub fn stop(&self) {
        {
            let mut state = self.state.lock();
            state.stopped = true;
        }
        self.state_changed.notify_all();

        let threads = self.threads.lock().take();
        if let Some(mut threads) = threads {
            threads.join();
        }
    }

    fn spawn_query<T>(
        &self,
        name: &'static str,
        query_factory: T,
        runner: Arc<BootstrapPromiseRunner>,
    ) -> JoinHandle<()>
    where
        T: BootstrapPromise<AscPullQuerySpec> + Send + 'static,
    {
        tracing::info!("Spawning bootstrap requester: {}", name);
        let message_sender = self.message_sender.clone();
        let clock = self.clock.clone();
        let stats = self.stats.clone();
        kernel_common::async_rt::spawn_named(
            async move {
                let mut promise = SendQueriesPromise::new(
                    query_factory,
                    QuerySender::new(message_sender, clock, stats),
                );

                loop {
                    if runner.state.lock().stopped {
                        break;
                    }

                    let mut sent_any = false;
                    for _ in 0..16 {
                        let now = runner.clock.now();
                        let result = {
                            let mut state = runner.state.lock();
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
                        kernel_common::async_rt::sleep(core::time::Duration::from_millis(5)).await;
                    }
                }
            },
            Some(name),
        );
        JoinHandle::new()
    }
}

pub struct RequesterThreads {
    pub priorities: Option<JoinHandle<()>>,
    pub dependencies: Option<JoinHandle<()>>,
    pub frontiers: Option<JoinHandle<()>>,
}

impl RequesterThreads {
    pub fn join(&mut self) {
        if let Some(_handle) = self.priorities.take() {
            // handle.join().unwrap();
        }
        if let Some(_dependencies) = self.dependencies.take() {
            // dependencies.join().unwrap();
        }
        if let Some(_frontiers) = self.frontiers.take() {
            // frontiers.join().unwrap();
        }
    }
}

impl StatsSource for Requesters {
    fn collect_stats(&self, result: &mut StatsCollection) {
        for s in self.stats_sources.lock().iter() {
            s.collect_stats(result);
        }
    }
}
