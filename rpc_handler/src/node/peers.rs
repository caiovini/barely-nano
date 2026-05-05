use alloc::string::ToString;
use alloc::sync::Arc;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{DetailedPeers, PeerInfo, PeersArgs, PeersDto, SimplePeers};
use hashbrown::HashMap;
use core::net::SocketAddrV6;

impl RpcCommandHandler {
    pub(crate) fn peers(&self, args: PeersArgs) -> PeersDto {
        let peer_details = args.peer_details.unwrap_or_default().inner();
        let mut peers: HashMap<SocketAddrV6, PeerInfo> = HashMap::new();

        self.node
            .network
            .read()
            .iter_channels()
            .for_each(|channel: &Arc<rsnano_network::Channel>| {
                peers.insert(
                    channel.peer_addr(),
                    PeerInfo {
                        protocol_version: channel.protocol_version().into(),
                        node_id: channel.node_id().map(|i: rsnano_core::NodeId| i.to_string()).unwrap_or_default(),
                        connection_type: "tcp".to_string(),
                        peering: channel.peering_addr_or_peer_addr(),
                    },
                );
            });

        if peer_details {
            PeersDto::Detailed(DetailedPeers { peers })
        } else {
            PeersDto::Simple(SimplePeers {
                peers: peers
                    .drain()
                    .map(|(k, v)| (k, v.protocol_version))
                    .collect(),
            })
        }
    }
}
