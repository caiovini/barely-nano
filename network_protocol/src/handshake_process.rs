use core::net::{Ipv6Addr, SocketAddrV4, SocketAddrV6};
use alloc::sync::Arc;

use rsnano_core::{BlockHash, PrivateKey};
use rsnano_messages::{Message, NodeIdHandshake};
use rsnano_network::Channel;
use rsnano_stats::Stats;
use crate::SynCookies;

pub enum HandshakeStatus {
    Abort,
    AbortOwnNodeId,
    Handshake,
    Realtime(rsnano_core::NodeId),
}

pub struct HandshakeProcess {
    pub channel: Arc<Channel>,
    pub node_id_key: PrivateKey,
    pub syn_cookies: Arc<SynCookies>,
    pub stats: Arc<Stats>,
    pub genesis_block_hash: BlockHash,
}

impl HandshakeProcess {
    pub fn new(
        channel: Arc<Channel>,
        node_id_key: PrivateKey,
        syn_cookies: Arc<SynCookies>,
        stats: Arc<Stats>,
        genesis_block_hash: BlockHash,
    ) -> Self {
        Self {
            channel,
            node_id_key,
            syn_cookies,
            stats,
            genesis_block_hash,
        }
    }

    pub fn initiate_handshake(&self, peer: &SocketAddrV6) -> anyhow::Result<()> {
        tracing::info!("[node] HandshakeProcess::initiate_handshake");
        if let Some(cookie) = self.syn_cookies.assign(peer) {
            let mut hex = alloc::string::String::new();
            for &b in &cookie {
                use core::fmt::Write;
                write!(hex, "{:02X}", b).unwrap();
            }
            tracing::info!("[node] Sending handshake with cookie: {} to {}", hex, peer);
            
            let message = Message::NodeIdHandshake(NodeIdHandshake {
                query: Some(rsnano_messages::NodeIdHandshakeQuery { cookie }),
                response: None,
                is_v2: true,
            });
            let mut serializer = rsnano_messages::MessageSerializer::new_with_buffer_size(
                rsnano_core::ProtocolInfo::default_for(rsnano_core::Networks::NanoLiveNetwork),
                512,
            );
            let buffer = serializer.serialize(&message);
            self.channel.send(buffer, rsnano_network::TrafficType::Generic);
        }
        Ok(())
    }

    pub fn process_handshake(&self, message: &NodeIdHandshake) -> (HandshakeStatus, Option<NodeIdHandshake>) {
        let mut response_message = NodeIdHandshake {
            query: None,
            response: None,
            is_v2: true,
        };
        let mut send_response = false;
        let mut status = HandshakeStatus::Handshake;

        // Handle incoming query
        if let Some(query) = &message.query {
            let signature = self.node_id_key.sign(&query.cookie);
            response_message.response = Some(rsnano_messages::NodeIdHandshakeResponse::new_v2(
                &query.cookie,
                &self.node_id_key,
                self.genesis_block_hash,
            ));
            send_response = true;
        }

        // Handle incoming response
        if let Some(response) = &message.response {
            let node_id = response.node_id;
            if let Some(cookie) = self.syn_cookies.cookie(&self.channel.peer_addr()) {
                if let Err(e) = response.validate(&cookie) {
                    tracing::warn!("Invalid handshake response from {}: {:?}", self.channel.peer_addr(), e);
                    return (HandshakeStatus::Abort, None);
                } else {
                    tracing::info!("Handshake SUCCESS! Peer: {}", self.channel.peer_addr());
                }
            } else {
                tracing::warn!("No cookie found for handshake response from {}", self.channel.peer_addr());
                return (HandshakeStatus::Abort, None);
            }
            status = HandshakeStatus::Realtime(node_id);
        }

        (status, if send_response { Some(response_message) } else { None })
    }
}

pub fn get_v6_address(addr: &core::net::SocketAddr) -> SocketAddrV6 {
    match addr {
        core::net::SocketAddr::V6(v6) => *v6,
        core::net::SocketAddr::V4(v4) => {
            let ip = v4.ip().octets();
            SocketAddrV6::new(
                Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 
                    ((ip[0] as u16) << 8) | (ip[1] as u16),
                    ((ip[2] as u16) << 8) | (ip[3] as u16)),
                v4.port(),
                0,
                0,
            )
        }
    }
}
