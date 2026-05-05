use super::KeepalivePublisher;
use alloc::sync::Arc;
use alloc::vec::Vec;
use rsnano_core::utils::Peer;

use tracing::debug;

/// Connect to preconfigured peers or send keepalive messages
/// if we are already connected
pub(crate) struct PreconfiguredPeersKeepalive {
    peers: Vec<Peer>,
    keepalive: Arc<KeepalivePublisher>,
}

impl PreconfiguredPeersKeepalive {
    pub(crate) fn new(peers: Vec<Peer>, keepalive: Arc<KeepalivePublisher>) -> Self {
        Self { peers, keepalive }
    }

    pub async fn keepalive(&self) {
        debug!(
            "Running keepalive for {} preconfigured peers",
            self.peers.len()
        );
        for peer in &self.peers {
            self.keepalive
                .keepalive_or_connect(peer.address.clone(), peer.port)
                .await;
        }
    }
}
