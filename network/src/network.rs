use crate::{
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
    utils::{into_core_addr, map_address_to_subnetwork}, Channel, ChannelDirection, ChannelId, ChannelMode,
    DataReceiver, DataReceiverFactory, NetworkConfig, TrafficType,
};
use rsnano_core::NodeId;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_random::NullableRng;
use core::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::boxed::Box;
use hashbrown::HashMap;
use rand::seq::SliceRandom;
use num_traits::Float;
use tracing::debug;
pub struct Network {
    channels: HashMap<ChannelId, Arc<Channel>>,
    next_channel_id: usize,
    stopped: core::sync::atomic::AtomicBool,
    clock: Arc<SteadyClock>,
    limiter: Arc<BandwidthLimiter>,
    stats: Arc<ChannelStats>,
    data_receiver_factory: Box<dyn DataReceiverFactory>,
    listening_port: u16,
    loopback: Arc<Channel>,
    pub protocol: rsnano_core::ProtocolInfo,
    on_new_realtime_channel_callback: Option<Arc<dyn Fn(Arc<Channel>) + Send + Sync>>,
}

impl Network {
    pub fn new(
        clock: Arc<SteadyClock>,
        limiter: Arc<BandwidthLimiter>,
        stats: Arc<ChannelStats>,
        data_receiver_factory: Box<dyn DataReceiverFactory>,
    ) -> Self {
        let loopback = Arc::new(Channel::new(
            ChannelId::LOOPBACK,
            into_core_addr(rsnano_core::utils::TEST_ENDPOINT_1),
            into_core_addr(rsnano_core::utils::TEST_ENDPOINT_1),
            ChannelDirection::Inbound,
            u8::MAX,
            clock.now(),
            limiter.clone(),
            stats.clone(),
        ));
        Self {
            channels: HashMap::new(),
            next_channel_id: 1,
            stopped: core::sync::atomic::AtomicBool::new(false),
            clock,
            limiter,
            stats,
            data_receiver_factory,
            listening_port: 0,
            loopback,
            protocol: rsnano_core::ProtocolInfo::default(),
            on_new_realtime_channel_callback: None,
        }
    }

    pub fn loopback(&self) -> &Arc<Channel> {
        &self.loopback
    }

    pub fn new_from_config(_config: NetworkConfig) -> Self {
        Self::new_test_instance()
    }

    pub fn new_test_instance() -> Self {
        use crate::NullDataReceiverFactory;
        Self::new(
            Arc::new(SteadyClock::new_null()),
            Arc::new(BandwidthLimiter::default()),
            Arc::new(ChannelStats::default()),
            Box::new(NullDataReceiverFactory::new()),
        )
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        for channel in self.channels.values() {
            channel.close();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn available_channels(&self) -> Vec<Arc<Channel>> {
        self.channels
            .values()
            .filter(|c| !c.is_closed() && c.mode() == ChannelMode::Realtime)
            .cloned()
            .collect()
    }

    pub fn add(
        &mut self,
        local_addr: SocketAddrV6,
        peer_addr: SocketAddrV6,
        direction: ChannelDirection,
        now: Timestamp,
    ) -> Result<(Arc<Channel>, Box<dyn DataReceiver + Send>), NetworkError> {
        if self.is_stopped() {
            return Err(NetworkError::Stopped);
        }

        let channel_id = ChannelId::from(self.next_channel_id);
        self.next_channel_id += 1;

        let channel = Arc::new(Channel::new(
            channel_id,
            local_addr,
            peer_addr,
            direction,
            0, // version
            now,
            self.limiter.clone(),
            self.stats.clone(),
        ));

        let receiver = self.data_receiver_factory.create_receiver_for(channel.clone());
        self.channels.insert(channel_id, channel.clone());

        Ok((channel, receiver))
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        if let Some(channel) = self.channels.remove(&channel_id) {
            channel.close();
        }
    }

    pub fn get(&self, channel_id: ChannelId) -> Option<Arc<Channel>> {
        self.channels.get(&channel_id).cloned()
    }

    pub fn count(&self) -> usize {
        self.channels.len()
    }

    pub fn is_inbound_slot_available(&self) -> bool {
        // Placeholder for real logic
        true
    }

    pub fn set_listening_port(&mut self, port: u16) {
        self.listening_port = port;
    }

    pub fn listening_port(&self) -> u16 {
        self.listening_port
    }

    pub fn add_outbound_attempt(&mut self, _peer: SocketAddrV6, _now: Timestamp) -> Result<(), NetworkError> {
        // Placeholder
        Ok(())
    }

    pub fn remove_attempt(&mut self, _peer: &SocketAddrV6) {
        // Placeholder
    }

    pub fn connect_error(&self, peer: SocketAddrV6, e: anyhow::Error) {
        debug!("Connection to {} failed: {}", peer, e);
    }

    pub fn attempt_timeout(&self, _peer: SocketAddrV6) {
        // Placeholder
    }

    pub fn accept_failure(&self) {
        // Placeholder
    }

    pub fn find_node_id(&self, node_id: &NodeId) -> Option<Arc<Channel>> {
        self.channels
            .values()
            .find(|c: &&Arc<Channel>| c.node_id() == Some(*node_id) && c.is_alive())
            .cloned()
    }

    pub fn find_channel(&self, endpoint: &SocketAddrV6) -> Option<Arc<Channel>> {
        self.channels.values().find(|c: &&Arc<Channel>| {
            c.mode() == ChannelMode::Realtime && c.is_alive() && c.peer_addr() == *endpoint
        }).cloned()
    }

    pub fn on_new_realtime_channel(&mut self, callback: Arc<dyn Fn(Arc<Channel>) + Send + Sync>) {
        self.on_new_realtime_channel_callback = Some(callback);
    }

    pub fn add_channel(&mut self, channel: Arc<Channel>) {
        self.channels.insert(channel.channel_id(), channel.clone());
        if channel.mode() == crate::ChannelMode::Realtime {
            if let Some(callback) = &self.on_new_realtime_channel_callback {
                callback(channel);
            }
        }
    }
    pub fn set_data_receiver_factory(&mut self, factory: Box<dyn DataReceiverFactory>) {
        self.data_receiver_factory = factory;
    }

    pub fn random_channels(&self, count: usize) -> Vec<Arc<Channel>> {
        let mut rng = NullableRng::rng();
        let mut channels: Vec<Arc<Channel>> = self.channels
            .values()
            .filter(|c: &&Arc<Channel>| c.is_alive() && c.mode() == ChannelMode::Realtime)
            .cloned()
            .collect();
        
        channels.shuffle(&mut rng);
        channels.truncate(count);
        channels
    }

    pub fn purge(&mut self, now: Timestamp, cutoff_period: Duration) -> Vec<Arc<Channel>> {
        let mut to_remove = Vec::new();
        for (id, channel) in &self.channels {
            if now.millis() - channel.last_activity().millis() >= cutoff_period.as_millis() as i64 {
                to_remove.push(*id);
            }
        }

        let mut removed = Vec::new();
        for id in to_remove {
            if let Some(channel) = self.channels.remove(&id) {
                channel.close();
                removed.push(channel);
            }
        }
        removed
    }

    pub fn fanout(&self, scale: f32) -> usize {
        let fanout = (self.channels.len() as f32).ln();
        (scale * fanout).ceil() as usize
    }

    pub fn check_capacity(&self, _traffic_type: TrafficType, _scale: f32) -> bool {
        true
    }

    pub fn idle_channels(&self, _min_idle_period: Duration, _now: Timestamp) -> Vec<Arc<Channel>> {
        Vec::new()
    }

    pub fn sorted_channels(&self) -> Vec<Arc<Channel>> {
        let mut result: Vec<_> = self.channels.values().cloned().collect();
        // Just return them as is for now
        result
    }

    pub fn set_peering_addr(&self, channel_id: ChannelId, peering_addr: SocketAddrV6) {
        if let Some(channel) = self.channels.get(&channel_id) {
            channel.set_peering_addr(peering_addr);
        }
    }

    pub fn upgrade_to_realtime_connection(
        &self,
        channel_id: ChannelId,
        node_id: NodeId,
    ) -> Option<(Arc<Channel>, Vec<Arc<dyn Fn(Arc<Channel>) + Send + Sync>>)> {
        if let Some(channel) = self.channels.get(&channel_id) {
            channel.set_mode(ChannelMode::Realtime);
            channel.set_node_id(node_id);
            let observers = if let Some(callback) = &self.on_new_realtime_channel_callback {
                alloc::vec![callback.clone()]
            } else {
                alloc::vec![]
            };
            return Some((channel.clone(), observers));
        }
        None
    }

    pub fn perma_ban(&mut self, _addr: SocketAddrV6) {
        // TODO: implement
    }

    pub fn count_by_mode(&self, mode: ChannelMode) -> usize {
        self.channels
            .values()
            .filter(|c| c.mode() == mode)
            .count()
    }

    pub fn shuffled_channels(&self, _type: TrafficType) -> Vec<Arc<Channel>> {
        let mut result: Vec<Arc<Channel>> = self.channels.values().cloned().collect();
        let mut rng = NullableRng::rng();
        result.shuffle(&mut rng);
        result
    }

    pub fn protocol_info(&self) -> rsnano_core::ProtocolInfo {
        self.protocol.clone()
    }

    pub fn random_fill_realtime(&self, peers: &mut [SocketAddrV6; 8]) {
        let mut i = 0;
        for channel in self.channels.values() {
            if channel.mode() == ChannelMode::Realtime {
                peers[i] = channel.peer_addr();
                i += 1;
                if i >= 8 {
                    break;
                }
            }
        }
    }

    pub fn find_realtime_channel_by_peering_addr(
        &self,
        peering_addr: &SocketAddrV6,
    ) -> Option<Arc<Channel>> {
        self.channels.values().find(|c| {
            c.mode() == ChannelMode::Realtime && c.peering_addr() == Some(*peering_addr)
        }).cloned()
    }

    pub fn bandwidth_limit(&self) -> usize {
        self.limiter.limit()
    }

    pub fn channels_info(&self) -> ChannelsInfo {
        let mut info = ChannelsInfo::default();
        for channel in self.channels.values() {
            info.total += 1;
            if channel.mode() == ChannelMode::Realtime {
                info.realtime += 1;
            }
            if channel.direction() == ChannelDirection::Inbound {
                info.inbound += 1;
            } else {
                info.outbound += 1;
            }
        }
        info
    }
}

#[derive(Default)]
pub struct ChannelsInfo {
    pub total: usize,
    pub realtime: usize,
    pub inbound: usize,
    pub outbound: usize,
}

#[derive(Debug)]
pub enum NetworkError {
    Stopped,
    Generic,
    MaxConnections,
    PeerExcluded,
    MaxConnectionsPerSubnetwork,
    MaxConnectionsPerIp,
    InvalidIp,
    DuplicateConnection,
    Cancelled,
}
