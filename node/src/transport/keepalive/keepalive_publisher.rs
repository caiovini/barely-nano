use alloc::sync::Arc;
use super::KeepaliveMessageFactory;
use crate::transport::MessageSender;
use rsnano_core::utils::Peer;
use rsnano_network::{
    utils::into_ipv6_socket_address, Channel, Network, PeerConnector, TrafficType,
};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use alloc::string::String;
use spin::{ Mutex, RwLock };
use core::net::SocketAddr;

/// Connects to a peer if we don't have a connection
/// or it sends a keepalive message if we are already connected
pub struct KeepalivePublisher {
    keepalive_listener: OutputListenerMt<Peer>,
    network: Arc<RwLock<Network>>,
    peer_connector: Arc<PeerConnector>,
    message_sender: Mutex<MessageSender>,
    message_factory: Arc<KeepaliveMessageFactory>,
    resolver: Arc<dyn rsnano_network::AsyncDnsResolver>,
}

impl KeepalivePublisher {
    pub fn new(
        network: Arc<RwLock<Network>>,
        peer_connector: Arc<PeerConnector>,
        message_sender: MessageSender,
        message_factory: Arc<KeepaliveMessageFactory>,
        resolver: Arc<dyn rsnano_network::AsyncDnsResolver>,
    ) -> Self {
        Self {
            keepalive_listener: OutputListenerMt::new(),
            network,
            peer_connector,
            message_sender: Mutex::new(message_sender),
            message_factory,
            resolver,
        }
    }

    pub fn track_keepalives(&self) -> Arc<OutputTrackerMt<Peer>> {
        self.keepalive_listener.track()
    }

    pub async fn keepalive_or_connect(&self, address: String, port: u16) {
        self.keepalive_listener
            .emit(Peer::new(address.clone(), port));
        
        match self.resolver.resolve(&address).await {
            Ok(addresses) => {
                for addr in addresses {
                    self.keepalive_or_connect_socket(SocketAddr::new(addr, port));
                }
            }
            Err(e) => {
                tracing::error!(
                    "Error resolving address for keepalive: {}:{} ({})",
                    address, port, e
                )
            }
        }
    }

    fn keepalive_or_connect_socket(&self, peer: SocketAddr) {
        let peer_v6 = into_ipv6_socket_address(peer);

        let channel = self
            .network
            .read()
            .find_realtime_channel_by_peering_addr(&peer_v6);

        match channel {
            Some(channel) => {
                self.try_send_keepalive(&channel);
            }
            None => {
                let _ = self.peer_connector.connect_to(peer_v6);
            }
        }
    }

    fn try_send_keepalive(&self, channel: &Channel) {
        let keepalive = self.message_factory.create_keepalive();

        self.message_sender
            .lock()
            .try_send(channel, &keepalive, TrafficType::Keepalive);
    }
}
