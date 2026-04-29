use crate::{
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
    utils::map_address_to_subnetwork, Channel, ChannelDirection, ChannelId, ChannelMode,
    DataReceiver, DataReceiverFactory, TrafficType,
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
    stopped: AtomicBool,
    clock: Arc<SteadyClock>,
    limiter: Arc<BandwidthLimiter>,
    stats: Arc<ChannelStats>,
    data_receiver_factory: Box<dyn DataReceiverFactory>,
    listening_port: u16,
}

impl Network {
    pub fn new(
        clock: Arc<SteadyClock>,
        limiter: Arc<BandwidthLimiter>,
        stats: Arc<ChannelStats>,
        data_receiver_factory: Box<dyn DataReceiverFactory>,
    ) -> Self {
        Self {
            channels: HashMap::new(),
            next_channel_id: 1,
            stopped: AtomicBool::new(false),
            clock,
            limiter,
            stats,
            data_receiver_factory,
            listening_port: 0,
        }
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

    pub fn connect_error(&self, _peer: SocketAddrV6, _e: anyhow::Error) {
        // Placeholder
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

    pub fn fanout(&self, size: usize) -> usize {
        let fanout = (size as f32).ln();
        let scale = 2.0_f32;
        (scale * fanout).ceil() as usize
    }

    pub fn upgrade_to_realtime_connection(
        &self,
        _channel_id: ChannelId,
        _node_id: NodeId,
    ) -> Option<(Arc<Channel>, Vec<Arc<dyn Fn(Arc<Channel>) + Send + Sync>>)> {
        // TODO: implement
        None
    }

    pub fn perma_ban(&mut self, _addr: SocketAddrV6) {
        // TODO: implement
    }
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
