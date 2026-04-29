use alloc::sync::{Arc, Weak};
use spin::{Mutex, RwLock};

use rsnano_core::{BlockHash, NodeId, PrivateKey, ProtocolInfo};
use rsnano_messages::{Message, MessageDeserializer, NetworkFilter};
use rsnano_network::{Channel, DataReceiver, DataReceiverFactory, Network};
use rsnano_stats::Stats;

use crate::{
    HandshakeProcess, HandshakeStats, LatestKeepalives, MessageCallback, NanoDataReceiver,
    SynCookies,
};

pub struct NanoDataReceiverFactory {
    network: Weak<RwLock<Network>>,
    try_enqueue: Arc<dyn Fn(Message, Arc<Channel>) -> bool + Send + Sync>,
    network_filter: Arc<NetworkFilter>,
    stats: Arc<Stats>,
    handshake_stats: Arc<HandshakeStats>,
    syn_cookies: Arc<SynCookies>,
    node_id_key: PrivateKey,
    latest_keepalives: Arc<Mutex<LatestKeepalives>>,
    genesis_block_hash: BlockHash,
    protocol: ProtocolInfo,
}

impl NanoDataReceiverFactory {
    pub fn new(
        network: &Arc<RwLock<Network>>,
        try_enqueue: impl Fn(Message, Arc<Channel>) -> bool + Send + Sync + 'static,
        network_filter: Arc<NetworkFilter>,
        stats: Arc<Stats>,
        handshake_stats: Arc<HandshakeStats>,
        syn_cookies: Arc<SynCookies>,
        node_id_key: PrivateKey,
        latest_keepalives: Arc<Mutex<LatestKeepalives>>,
        genesis_block_hash: BlockHash,
        protocol: ProtocolInfo,
    ) -> Self {
        Self {
            network: Arc::downgrade(network),
            try_enqueue: Arc::new(try_enqueue),
            network_filter,
            stats,
            handshake_stats,
            syn_cookies,
            node_id_key,
            latest_keepalives,
            genesis_block_hash,
            protocol,
        }
    }
}

impl DataReceiverFactory for NanoDataReceiverFactory {
    fn create_receiver_for(&self, channel: Arc<Channel>) -> alloc::boxed::Box<dyn DataReceiver + Send> {
        let handshake_process = HandshakeProcess::new(
            channel.clone(),
            self.node_id_key.clone(),
            self.syn_cookies.clone(),
            self.stats.clone(),
            self.genesis_block_hash,
        );

        let message_deserializer = MessageDeserializer::with_filter(
            self.protocol.clone(),
            self.network_filter.clone(),
        );

        alloc::boxed::Box::new(NanoDataReceiver::new(
            channel,
            handshake_process,
            message_deserializer,
            self.try_enqueue.clone(),
            self.latest_keepalives.clone(),
            self.stats.clone(),
            self.network.clone(),
            self.handshake_stats.clone(),
            self.protocol.clone(),
        ))
    }
}
