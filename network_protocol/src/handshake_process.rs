use alloc::sync::Arc;
use core::net::{Ipv6Addr, SocketAddrV6};

use crate::SynCookies;
use rsnano_core::{BlockHash, PrivateKey};
use rsnano_messages::{Message, NodeIdHandshake};
use rsnano_network::Channel;
use rsnano_stats::Stats;

#[derive(Debug, Clone, PartialEq, Eq)]
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
        let cookie = self.syn_cookies.assign(peer);
        if let Some(cookie) = cookie {
            let mut hex = alloc::string::String::new();
            for b in cookie {
                hex.push_str(&alloc::format!("{:02x}", b));
            }

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
            self.channel.send_handshake(buffer);
        }
        Ok(())
    }

    pub fn process_handshake(
        &self,
        message: &NodeIdHandshake,
    ) -> (HandshakeStatus, Option<NodeIdHandshake>) {
        let mut response_message = NodeIdHandshake {
            query: None,
            response: None,
            is_v2: true,
        };
        let mut send_response = false;
        let mut status = HandshakeStatus::Handshake;

        // Handle incoming query
        if let Some(query) = &message.query {
            kernel_common::uart::putchar(b'&');
            response_message.response = Some(rsnano_messages::NodeIdHandshakeResponse::new_v2(
                &query.cookie,
                &self.node_id_key,
                self.genesis_block_hash,
            ));

            // Also send our own query so that the other side can respond and we can complete the handshake
            if let Some(cookie) = self.syn_cookies.assign(&self.channel.peer_addr()) {
                response_message.query = Some(rsnano_messages::NodeIdHandshakeQuery { cookie });
            }

            send_response = true;
        }

        // Handle incoming response
        if let Some(response) = &message.response {
            let node_id = response.node_id;

            if let Some(cookie) = self.syn_cookies.cookie(&self.channel.peer_addr()) {
                // Debug: print cookie being used for validation
                kernel_common::uart::print(b"[hs] cookie:");
                for b in &cookie {
                    kernel_common::uart::print_u8_hex(*b);
                }
                kernel_common::uart::putchar(b'\n');

                // Debug: print node_id
                kernel_common::uart::print(b"[hs] nodeid:");
                for b in node_id.as_key().as_bytes() {
                    kernel_common::uart::print_u8_hex(*b);
                }
                kernel_common::uart::putchar(b'\n');

                // Debug: print sig first 16 bytes (R component)
                kernel_common::uart::print(b"[hs] sig_r:");
                for b in &response.signature.as_bytes()[..16] {
                    kernel_common::uart::print_u8_hex(*b);
                }
                kernel_common::uart::putchar(b'\n');

                // Debug: print genesis
                if let Some(ref v2) = response.v2 {
                    kernel_common::uart::print(b"[hs] genesis:");
                    for b in v2.genesis.as_bytes() {
                        kernel_common::uart::print_u8_hex(*b);
                    }
                    kernel_common::uart::putchar(b'\n');

                    // Print local genesis for comparison
                    kernel_common::uart::print(b"[hs] local_genesis:");
                    for b in self.genesis_block_hash.as_bytes() {
                        kernel_common::uart::print_u8_hex(*b);
                    }
                    kernel_common::uart::putchar(b'\n');
                }

                if let Err(_) = response.validate(&cookie) {
                    kernel_common::uart::putchar(b'X'); // X for validation fail
                    status = HandshakeStatus::Abort;
                } else {
                    kernel_common::uart::putchar(b'$'); // $ for Success
                    status = HandshakeStatus::Realtime(node_id);
                }
            } else {
                kernel_common::uart::putchar(b'@'); // @ for no cookie
                status = HandshakeStatus::Abort;
            }
        }

        (
            status,
            if send_response {
                Some(response_message)
            } else {
                None
            },
        )
    }
}

pub fn get_v6_address(addr: &core::net::SocketAddr) -> SocketAddrV6 {
    match addr {
        core::net::SocketAddr::V6(v6) => *v6,
        core::net::SocketAddr::V4(v4) => {
            let ip = v4.ip().octets();
            SocketAddrV6::new(
                Ipv6Addr::new(
                    0,
                    0,
                    0,
                    0,
                    0,
                    0xffff,
                    ((ip[0] as u16) << 8) | (ip[1] as u16),
                    ((ip[2] as u16) << 8) | (ip[3] as u16),
                ),
                v4.port(),
                0,
                0,
            )
        }
    }
}
