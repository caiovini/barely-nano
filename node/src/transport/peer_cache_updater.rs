use alloc::sync::Arc;
use spin::RwLock;
use core::time::Duration;
use alloc::vec::Vec;

use rsnano_core::utils::{CancellationToken, Tickable, SocketAddrV6};
use tracing::debug;

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, Network};
use rsnano_nullable_clock::SystemTimeFactory;
use rsnano_stats::{DetailType, StatType, Stats};

/// Writes a snapshot of the current peers to the database,
/// so that we can reconnect to them when the node is restarted
pub struct PeerCacheUpdater {
    network: Arc<RwLock<Network>>,
    ledger: Arc<Ledger>,
    time_factory: SystemTimeFactory,
    stats: Arc<Stats>,
    erase_cutoff: Duration,
}

impl PeerCacheUpdater {
    pub fn new(
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        time_factory: SystemTimeFactory,
        stats: Arc<Stats>,
        erase_cutoff: Duration,
    ) -> Self {
        Self {
            network,
            ledger,
            time_factory,
            stats,
            erase_cutoff,
        }
    }

    fn save_peers(&self) {
        let live_peers = self.network.read().sorted_channels();
        for peer in &live_peers {
            self.save_peer(peer);
        }
    }

    fn save_peer(&self, channel: &Channel) {
        let Some(endpoint) = channel.peering_addr() else {
            return;
        };
        let exists = self.ledger.store.peer.exists(endpoint);

        self.ledger
            .store
            .peer
            .put(endpoint, self.time_factory.now());

        if !exists {
            self.stats.inc(StatType::PeerHistory, DetailType::Inserted);
            debug!("Saved new peer: {}", endpoint);
        } else {
            self.stats.inc(StatType::PeerHistory, DetailType::Updated);
        }
    }

    fn delete_old_peers(&self) {
        for peer in self.get_old_peers() {
            self.ledger.store.peer.del(peer)
        }
    }

    fn get_old_peers(&self) -> Vec<SocketAddrV6> {
        let now = self.time_factory.now();
        let cutoff = now - self.erase_cutoff;
        self.ledger
            .store
            .peer
            .iter()
            .into_iter()
            .filter_map(|(peer, time)| {
                if time < cutoff || time > now {
                    Some(peer)
                } else {
                    None
                }
            })
            .collect()
    }
}

impl Tickable for PeerCacheUpdater {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.stats.inc(StatType::PeerHistory, DetailType::Loop);
        self.save_peers();
        self.delete_old_peers();
    }
}
