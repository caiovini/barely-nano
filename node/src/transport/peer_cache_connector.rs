use core::net::SocketAddrV6;
use core::time::Duration;
use alloc::sync::Arc;
use alloc::vec::Vec;


use tracing::info;

use rsnano_core::utils::{CancellationToken, Tickable};
use rsnano_ledger::Ledger;
use rsnano_network::PeerConnector;
use rsnano_stats::{DetailType, StatType, Stats};

use core::sync::atomic::{AtomicBool, Ordering};

// Tries to connect to peers that are stored in the peer cache
#[derive(Clone)]
pub struct PeerCacheConnector {
    ledger: Arc<Ledger>,
    peer_connector: Arc<PeerConnector>,
    stats: Arc<Stats>,
    first_run: Arc<AtomicBool>,
    ///Delay between each connection attempt. This throttles new connections.
    #[allow(dead_code)]
    reach_out_delay: Duration,
}

impl PeerCacheConnector {
    pub fn new(
        ledger: Arc<Ledger>,
        peer_connector: Arc<PeerConnector>,
        stats: Arc<Stats>,
        reach_out_delay: Duration,
    ) -> Self {
        Self {
            ledger,
            peer_connector,
            stats,
            first_run: Arc::new(AtomicBool::new(true)),
            reach_out_delay,
        }
    }

    fn load_peers_from_cache(&self) -> Vec<SocketAddrV6> {
        self.ledger
            .store
            .peer
            .iter()
            .into_iter()
            .map(|(peer, _)| peer)
            .collect()
    }

    pub async fn run_once_async(&self) {
        self.stats
            .inc(StatType::Network, DetailType::LoopReachoutCached);
        let cached_peers = self.load_peers_from_cache();

        if self.first_run.swap(false, Ordering::SeqCst) {
            info!("Adding cached initial peers: {}", cached_peers.len());
        }

        for peer in cached_peers {
            self.stats
                .inc(StatType::Network, DetailType::ReachoutCached);
            let _ = self.peer_connector.connect_to(peer);
            if !self.reach_out_delay.is_zero() {
                kernel_common::async_rt::sleep(self.reach_out_delay).await;
            }
        }
    }
}

pub trait PeerCacheConnectorExt {
    fn start(&self, interval: Duration);
}

impl PeerCacheConnectorExt for Arc<PeerCacheConnector> {
    fn start(&self, interval: Duration) {
        let this = self.clone();
        kernel_common::async_rt::spawn(async move {
            loop {
                this.run_once_async().await;
                kernel_common::async_rt::sleep(interval).await;
            }
        });
    }
}

impl PeerCacheConnectorExt for Arc<spin::Mutex<PeerCacheConnector>> {
    fn start(&self, interval: Duration) {
        let this = self.clone();
        kernel_common::async_rt::spawn(async move {
            loop {
                let connector = this.try_lock().map(|g| (*g).clone());
                if let Some(connector) = connector {
                    connector.run_once_async().await;
                }
                kernel_common::async_rt::sleep(interval).await;
            }
        });
    }
}


impl Tickable for PeerCacheConnector {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.stats
            .inc(StatType::Network, DetailType::LoopReachoutCached);
        let cached_peers = self.load_peers_from_cache();

        if self.first_run.swap(false, Ordering::SeqCst) {
            info!("Adding cached initial peers: {}", cached_peers.len());
        }

        for peer in cached_peers {
            self.stats
                .inc(StatType::Network, DetailType::ReachoutCached);
            let _ = self.peer_connector.connect_to(peer);
        }
    }
}
