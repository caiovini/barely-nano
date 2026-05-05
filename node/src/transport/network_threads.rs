use alloc::sync::Arc;
use spin::{ Mutex, RwLock };
use crate::utils::{Condvar, JoinHandle};
use core::time::Duration;

use rsnano_messages::{Keepalive, Message, NetworkFilter};
use rsnano_network::{DeadChannelCleanup, Network, NetworkConfig, PeerConnector, TrafficType};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{DetailType, StatType, Stats};

use super::{keepalive::KeepaliveMessageFactory, MessageFlooder};
use crate::config::{NetworkParams, NodeFlags};
use rsnano_network_protocol::{LatestKeepalives, SynCookies};
use kernel_common::async_rt;

pub(crate) struct NetworkThreads {
    cleanup_thread: Option<JoinHandle<()>>,
    keepalive_thread: Option<JoinHandle<()>>,
    reachout_thread: Option<JoinHandle<()>>,
    stopped: Arc<(Condvar, Mutex<bool>)>,
    network: Arc<RwLock<Network>>,
    peer_connector: Arc<PeerConnector>,
    flags: NodeFlags,
    network_params: NetworkParams,
    network_config: NetworkConfig,
    stats: Arc<Stats>,
    syn_cookies: Arc<SynCookies>,
    network_filter: Arc<NetworkFilter>,
    keepalive_factory: Arc<KeepaliveMessageFactory>,
    latest_keepalives: Arc<Mutex<LatestKeepalives>>,
    dead_channel_cleanup: Arc<Mutex<DeadChannelCleanup>>,
    message_flooder: MessageFlooder,
    clock: Arc<SteadyClock>,
}

impl NetworkThreads {
    pub fn new(
        network: Arc<RwLock<Network>>,
        peer_connector: Arc<PeerConnector>,
        flags: NodeFlags,
        network_params: NetworkParams,
        network_config: NetworkConfig,
        stats: Arc<Stats>,
        syn_cookies: Arc<SynCookies>,
        network_filter: Arc<NetworkFilter>,
        keepalive_factory: Arc<KeepaliveMessageFactory>,
        latest_keepalives: Arc<Mutex<LatestKeepalives>>,
        dead_channel_cleanup: Arc<Mutex<DeadChannelCleanup>>,
        message_flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            cleanup_thread: None,
            keepalive_thread: None,
            reachout_thread: None,
            stopped: Arc::new((Condvar::new(), Mutex::new(false))),
            network,
            peer_connector,
            flags,
            network_params,
            network_config,
            stats,
            syn_cookies,
            network_filter,
            keepalive_factory,
            latest_keepalives,
            dead_channel_cleanup,
            message_flooder,
            clock,
        }
    }

    pub fn start(&mut self) {
        let cleanup = CleanupLoop {
            stopped: self.stopped.clone(),
            network_params: self.network_params.clone(),
            flags: self.flags.clone(),
            syn_cookies: self.syn_cookies.clone(),
            network_filter: self.network_filter.clone(),
            dead_channel_cleanup: self.dead_channel_cleanup.clone(),
        };

        async_rt::spawn(async move {
            cleanup.run_async().await;
        });

        let mut keepalive = KeepaliveLoop {
            stopped: self.stopped.clone(),
            network: self.network.clone(),
            keepalive_period: self.network_params.network.keepalive_period,
            stats: Arc::clone(&self.stats),
            keepalive_factory: self.keepalive_factory.clone(),
            message_flooder: self.message_flooder.clone(),
            clock: self.clock.clone(),
        };

        async_rt::spawn(async move {
            keepalive.run_async().await;
        });

        if !self.network_config.peer_reachout.is_zero() {
            let reachout = ReachoutLoop {
                stopped: self.stopped.clone(),
                reachout_interval: self.network_config.peer_reachout,
                stats: self.stats.clone(),
                peer_connector: self.peer_connector.clone(),
                latest_keepalives: self.latest_keepalives.clone(),
            };

            async_rt::spawn(async move {
                reachout.run_async().await;
            });
        }
    }

    pub fn stop(&mut self) {
        *self.stopped.1.lock() = true;
        self.stopped.0.notify_all();
        self.network.write().stop();
        let _t = self.keepalive_thread.take();
        // if let Some(t) = t {
        //     t.join().unwrap(); */ */ */ */
        // }
        let _t = self.cleanup_thread.take();
        // if let Some(t) = t {
        //     t.join().unwrap();
        // }
        let _t = self.reachout_thread.take();
        // if let Some(t) = t {
        //     t.join().unwrap();
        // }
    }
}

impl Drop for NetworkThreads {
    fn drop(&mut self) {
        // All threads must be stopped before this destructor
        // debug_assert!(self.cleanup_thread.is_none());
        // debug_assert!(self.keepalive_thread.is_none());
    }
}

struct CleanupLoop {
    stopped: Arc<(Condvar, Mutex<bool>)>,
    network_params: NetworkParams,
    flags: NodeFlags,
    syn_cookies: Arc<SynCookies>,
    network_filter: Arc<NetworkFilter>,
    dead_channel_cleanup: Arc<Mutex<DeadChannelCleanup>>,
}

impl CleanupLoop {
    #[allow(dead_code)]
    async fn run_async(&self) {
        while !*self.stopped.1.lock() {
            let timeout = if self.network_params.network.is_dev_network() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(5)
            };
            
            async_rt::sleep(timeout).await;

            if *self.stopped.1.lock() {
                return;
            }

            if !self.flags.disable_connection_cleanup {
                self.dead_channel_cleanup.lock().clean_up();
            }

            self.syn_cookies
                .purge(self.network_params.network.sync_cookie_cutoff);

            self.network_filter.update(timeout.as_secs());
        }
    }
}

struct KeepaliveLoop {
    stopped: Arc<(Condvar, Mutex<bool>)>,
    stats: Arc<Stats>,
    network: Arc<RwLock<Network>>,
    keepalive_factory: Arc<KeepaliveMessageFactory>,
    message_flooder: MessageFlooder,
    clock: Arc<SteadyClock>,
    keepalive_period: Duration,
}

impl KeepaliveLoop {
    #[allow(dead_code)]
    async fn run_async(&mut self) {
        while !*self.stopped.1.lock() {
            async_rt::sleep(self.keepalive_period).await;

            if *self.stopped.1.lock() {
                return;
            }

            self.stats.inc(StatType::Network, DetailType::LoopKeepalive);
            self.flood_keepalive(0.75);
            self.flood_keepalive_self(0.25);

            self.keepalive();
        }
    }

    fn keepalive(&mut self) {
        let (message, keepalive_list) = {
            let message = self.keepalive_factory.create_keepalive();

            let network = self.network.read();
            let channels = network.idle_channels(self.keepalive_period, self.clock.now());
            (message, channels)
        };

        for channel in keepalive_list {
            self.message_flooder
                .try_send(&channel, &message, TrafficType::Keepalive);
        }
    }

    fn flood_keepalive(&mut self, scale: f32) {
        let mut keepalive = Keepalive::default();
        self.network
            .read()
            .random_fill_realtime(&mut keepalive.peers);
        self.message_flooder.flood(
            &Message::Keepalive(keepalive),
            TrafficType::Keepalive,
            scale,
        );
    }

    fn flood_keepalive_self(&mut self, scale: f32) {
        let keepalive = self.keepalive_factory.create_keepalive_self();
        self.message_flooder
            .flood(&keepalive, TrafficType::Keepalive, scale);
    }
}

struct ReachoutLoop {
    stopped: Arc<(Condvar, Mutex<bool>)>,
    reachout_interval: Duration,
    stats: Arc<Stats>,
    peer_connector: Arc<PeerConnector>,
    latest_keepalives: Arc<Mutex<LatestKeepalives>>,
}

impl ReachoutLoop {
    #[allow(dead_code)]
    async fn run_async(&self) {
        while !*self.stopped.1.lock() {
            async_rt::sleep(self.reachout_interval).await;

            if *self.stopped.1.lock() {
                return;
            }

            let keepalive = self.latest_keepalives.lock().pop_random();
            if let Some(keepalive) = keepalive {
                for peer in keepalive.peers {
                    if peer.ip().is_unspecified() {
                        continue;
                    }
                    self.stats.inc(StatType::Network, DetailType::ReachoutLive);
                    let _ = self.peer_connector.connect_to(peer);
                }
            }
        }
    }
}
