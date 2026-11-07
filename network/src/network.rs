use crate::{
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
    peer_exclusion::PeerExclusion, utils::into_core_addr, Channel, ChannelDirection, ChannelId,
    ChannelMode, DataReceiver, DataReceiverFactory, NetworkConfig, TrafficType,
};
use rsnano_core::NodeId;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_random::NullableRng;
use core::{
    net::SocketAddrV6,
    sync::atomic::Ordering,
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
    attempts: HashMap<SocketAddrV6, Timestamp>,
    excluded_peers: PeerExclusion,
    next_channel_id: usize,
    stopped: core::sync::atomic::AtomicBool,
    #[allow(dead_code)]
    clock: Arc<SteadyClock>,
    limiter: Arc<BandwidthLimiter>,
    stats: Arc<ChannelStats>,
    config: NetworkConfig,
    listening_port: u16,
    loopback: Arc<Channel>,
    pub protocol: rsnano_core::ProtocolInfo,
    on_new_realtime_channel_observers: Vec<Arc<dyn Fn(Arc<Channel>) + Send + Sync>>,
    data_receiver_factory: Option<Box<dyn DataReceiverFactory>>,
}

impl Network {
    const MAX_PEERS_PER_SUBNET: usize = 4;

    pub fn new(
        clock: Arc<SteadyClock>,
        limiter: Arc<BandwidthLimiter>,
        stats: Arc<ChannelStats>,
        config: NetworkConfig,
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
            attempts: HashMap::new(),
            excluded_peers: PeerExclusion::new(),
            next_channel_id: 1,
            stopped: core::sync::atomic::AtomicBool::new(false),
            clock,
            limiter,
            stats,
            data_receiver_factory: None,
            config,
            listening_port: 0,
            loopback,
            protocol: rsnano_core::ProtocolInfo::default(),
            on_new_realtime_channel_observers: Vec::new(),
        }
    }

    pub fn loopback(&self) -> &Arc<Channel> {
        &self.loopback
    }

    pub fn new_from_config(config: NetworkConfig) -> Self {
        Self::new(
            Arc::new(SteadyClock::new_null()),
            Arc::new(BandwidthLimiter::default()),
            Arc::new(ChannelStats::default()),
            config,
        )
    }

    pub fn new_test_instance() -> Self {
        Self::new(
            Arc::new(SteadyClock::new_null()),
            Arc::new(BandwidthLimiter::default()),
            Arc::new(ChannelStats::default()),
            NetworkConfig::default(),
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

    pub fn iter_channels(&self) -> impl Iterator<Item = &Arc<Channel>> {
        self.channels.values()
    }

    pub fn available_channels(&self) -> Vec<Arc<Channel>> {
        self.channels
            .values()
            .filter(|c| {
                #[cfg(not(test))]
                let ready = c.seconds_since_realtime() >= 1.0;
                #[cfg(test)]
                let ready = true;
                !c.is_closed()
                    && c.mode() == ChannelMode::Realtime
                    && ready
                    && !c.should_drop(TrafficType::BootstrapRequests)
            })
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

        if self.excluded_peers.is_excluded(peer_addr.ip()) {
            kernel_common::uart::putchar(b'-');
            return Err(NetworkError::PeerExcluded);
        }

        if let Some(existing) = self.find_channel_by_ip(&peer_addr.ip()) {
            if !existing.is_closed() {
                kernel_common::uart::putchar(b'-');
                return Err(NetworkError::DuplicateConnection);
            }
        }

        if self.count_by_subnet(peer_addr.ip()) >= Self::MAX_PEERS_PER_SUBNET {
            kernel_common::uart::putchar(b'-');
            return Err(NetworkError::MaxConnectionsPerSubnetwork);
        }

        // Also reject inbound connections if we already have a pending outbound
        // attempt to the same IP, to avoid simultaneous bidirectional connections
        // that cause the remote C++ node to reset one side.
        if direction == ChannelDirection::Inbound {
            let ip = peer_addr.ip();
            let has_attempt = self.attempts.keys().any(|a| *a.ip() == *ip);
            if has_attempt {
                kernel_common::uart::putchar(b'-');
                return Err(NetworkError::DuplicateConnection);
            }
        }

        if direction == ChannelDirection::Inbound && !self.is_inbound_slot_available() {
            debug!("Rejecting inbound connection: max connections reached ({})", self.config.max_inbound_connections);
            return Err(NetworkError::MaxConnections);
        }

        if direction == ChannelDirection::Outbound && !self.is_outbound_slot_available() {
            debug!("Rejecting outbound connection: max connections reached ({})", self.config.max_outbound_connections);
            return Err(NetworkError::MaxConnections);
        }

        let channel_id = ChannelId::from(self.next_channel_id);
        self.next_channel_id += 1;

        let channel = Arc::new(Channel::new(
            channel_id,
            local_addr,
            peer_addr,
            direction,
            self.protocol.version_min, // version
            now,
            self.limiter.clone(),
            self.stats.clone(),
        ));

        let receiver = self.data_receiver_factory.as_ref().expect("DataReceiverFactory not set").create_receiver_for(channel.clone());
        self.channels.insert(channel_id, channel.clone());
        kernel_common::uart::putchar(b'+');
        Ok((channel, receiver))
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        kernel_common::uart::putchar(b'-');
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
        self.count_by_direction(ChannelDirection::Inbound) < self.config.max_inbound_connections
    }

    pub fn is_outbound_slot_available(&self) -> bool {
        self.count_by_direction(ChannelDirection::Outbound) < self.config.max_outbound_connections
    }

    pub fn is_attempt_slot_available(&self) -> bool {
        self.attempts.len() < self.config.max_attempts
    }

    fn count_by_direction(&self, direction: ChannelDirection) -> usize {
        self.channels
            .values()
            .filter(|c| c.direction() == direction && !c.is_closed())
            .count()
    }

    pub fn set_listening_port(&mut self, port: u16) {
        self.listening_port = port;
    }

    pub fn listening_port(&self) -> u16 {
        self.listening_port
    }

    pub fn add_outbound_attempt(&mut self, peer: SocketAddrV6, now: Timestamp) -> Result<(), NetworkError> {
        if self.excluded_peers.is_excluded(peer.ip()) {
            return Err(NetworkError::PeerExcluded);
        }
        if !self.is_outbound_slot_available() || !self.is_attempt_slot_available() {
            debug!("Rejecting outbound attempt to {}: max connections/attempts reached ({})", peer, self.config.max_outbound_connections);
            return Err(NetworkError::MaxConnections);
        }
        if self.attempts.contains_key(&peer) {
            debug!("Rejecting outbound attempt to {}: already attempting", peer);
            return Err(NetworkError::DuplicateConnection);
        }
        if self.find_channel_by_ip(&peer.ip()).is_some() {
            debug!("Rejecting outbound attempt to {}: already connected", peer);
            return Err(NetworkError::DuplicateConnection);
        }
        if self.count_by_subnet(peer.ip()) >= Self::MAX_PEERS_PER_SUBNET {
            return Err(NetworkError::MaxConnectionsPerSubnetwork);
        }
        self.attempts.insert(peer, now);
        Ok(())
    }

    pub fn remove_attempt(&mut self, peer: &SocketAddrV6) {
        self.attempts.remove(peer);
    }

    pub fn connect_error(&self, peer: SocketAddrV6, e: anyhow::Error) {
        debug!("Connection to {} failed: {}", peer, e);
    }

    pub fn attempt_timeout(&mut self, peer: SocketAddrV6) {
        self.attempts.remove(&peer);
    }

    pub fn accept_failure(&self) {
    }

    pub fn find_node_id(&self, node_id: &NodeId) -> Option<Arc<Channel>> {
        self.channels
            .values()
            .find(|c: &&Arc<Channel>| c.node_id() == Some(*node_id) && c.is_alive())
            .cloned()
    }

    pub fn find_channel(&self, endpoint: &SocketAddrV6) -> Option<Arc<Channel>> {
        self.channels.values().find(|c: &&Arc<Channel>| {
            (c.mode() == ChannelMode::Realtime || c.mode() == ChannelMode::Undefined) && c.is_alive() && c.peer_addr() == *endpoint
        }).cloned()
    }

    pub fn find_channel_by_ip(&self, ip: &core::net::Ipv6Addr) -> Option<Arc<Channel>> {
        if ip.is_loopback() {
            return None;
        }
        let octets = ip.octets();
        if octets[0..10] == [0; 10] && octets[10] == 0xff && octets[11] == 0xff {
            let ip4_octets = [octets[12], octets[13], octets[14], octets[15]];
            if ip4_octets[0] == 127 {
                return None;
            }
        }
        self.channels.values().find(|c: &&Arc<Channel>| {
            (c.mode() == ChannelMode::Realtime || c.mode() == ChannelMode::Undefined) 
                && c.is_alive() 
                && c.peer_addr().ip() == ip
        }).cloned()
    }

    fn is_exempt_subnet(ip: &core::net::Ipv6Addr) -> bool {
        if ip.is_loopback() || ip.is_unspecified() {
            return true;
        }
        let o = ip.octets();
        // IPv4-mapped IPv6 address (::ffff:a.b.c.d)
        if o[0..10] == [0; 10] && o[10] == 0xff && o[11] == 0xff {
            let a = o[12];
            let b = o[13];
            return a == 127
                || a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254);
        }
        // IPv6 Unique Local (fc00::/7) or Link-Local (fe80::/10)
        (o[0] & 0xfe) == 0xfc || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
    }

    fn same_subnet(a: &core::net::Ipv6Addr, b: &core::net::Ipv6Addr) -> bool {
        let oa = a.octets();
        let ob = b.octets();
        let a_v4 = oa[0..10] == [0; 10] && oa[10] == 0xff && oa[11] == 0xff;
        let b_v4 = ob[0..10] == [0; 10] && ob[10] == 0xff && ob[11] == 0xff;
        if a_v4 && b_v4 {
            // /24 subnet for IPv4
            oa[12..15] == ob[12..15]
        } else if !a_v4 && !b_v4 {
            // /48 subnet for IPv6
            oa[0..6] == ob[0..6]
        } else {
            false
        }
    }

    fn count_by_subnet(&self, ip: &core::net::Ipv6Addr) -> usize {
        if Self::is_exempt_subnet(ip) {
            return 0;
        }
        let active = self
            .channels
            .values()
            .filter(|c| {
                (c.mode() == ChannelMode::Realtime || c.mode() == ChannelMode::Undefined)
                    && c.is_alive()
                    && Self::same_subnet(c.peer_addr().ip(), ip)
            })
            .count();
        let pending = self
            .attempts
            .keys()
            .filter(|a| Self::same_subnet(a.ip(), ip))
            .count();
        active + pending
    }

    pub fn on_new_realtime_channel(&mut self, callback: Arc<dyn Fn(Arc<Channel>) + Send + Sync>) {
        self.on_new_realtime_channel_observers.push(callback);
    }

    pub fn add_channel(&mut self, channel: Arc<Channel>) {
        if channel.direction() == ChannelDirection::Inbound && !self.is_inbound_slot_available() {
            debug!("add_channel rejected inbound: limit reached");
            return;
        }
        if channel.direction() == ChannelDirection::Outbound && !self.is_outbound_slot_available() {
            debug!("add_channel rejected outbound: limit reached");
            return;
        }

        self.channels.insert(channel.channel_id(), channel.clone());
        if channel.mode() == crate::ChannelMode::Realtime {
            for observer in &self.on_new_realtime_channel_observers {
                observer(channel.clone());
            }
        }
    }

    pub fn add_test_channel(&mut self) -> Arc<Channel> {
        let channel = Arc::new(Channel::new_test_instance());
        channel.set_mode(ChannelMode::Realtime);
        self.add_channel(channel.clone());
        channel
    }

    pub fn set_data_receiver_factory(&mut self, factory: Box<dyn DataReceiverFactory>) {
        self.data_receiver_factory = Some(factory);
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
            if channel.is_closed()
                || now.millis() - channel.last_activity().millis() >= cutoff_period.as_millis() as i64
                || channel.protocol_version() < self.protocol.version_min
            {
                to_remove.push(*id);
                kernel_common::uart::putchar(b'~');
            }
        }

        let mut removed = Vec::new();
        for id in to_remove {
            if let Some(channel) = self.channels.remove(&id) {
                kernel_common::uart::putchar(b'~');
                channel.close();
                removed.push(channel);
            }
        }
        self.excluded_peers.purge(now, Duration::from_secs(3600));
        removed
    }

    pub fn fanout(&self, scale: f32) -> usize {
        if self.channels.is_empty() {
            return 0;
        }
        let fanout = (self.channels.len() as f32).ln();
        (scale * fanout).ceil() as usize
    }

    pub fn check_capacity(&self, traffic_type: TrafficType, scale: f32) -> bool {
        let available = self
            .channels
            .values()
            .filter(|c| c.mode() == ChannelMode::Realtime && !c.should_drop(traffic_type))
            .count();
        available >= self.fanout(scale)
    }

    pub fn idle_channels(&self, min_idle_period: Duration, now: Timestamp) -> Vec<Arc<Channel>> {
        let mut result = Vec::new();
        for channel in self.channels.values() {
            if channel.mode() == ChannelMode::Realtime
                && !channel.is_closed()
                && now.millis() - channel.last_activity().millis() >= min_idle_period.as_millis() as i64
            {
                result.push(channel.clone());
            }
        }
        result
    }

    pub fn sorted_channels(&self) -> Vec<Arc<Channel>> {
        let result: Vec<_> = self.channels.values().cloned().collect();
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
            kernel_common::uart::putchar(b'U');
            let observers = self.on_new_realtime_channel_observers.clone();
            Some((channel.clone(), observers))
        } else {
            kernel_common::uart::putchar(b'u');
            None
        }
    }

    pub fn perma_ban(&mut self, addr: SocketAddrV6) {
        self.excluded_peers.exclude(*addr.ip(), self.clock.now());
    }

    pub fn count_by_mode(&self, mode: ChannelMode) -> usize {
        self.channels
            .values()
            .filter(|c| c.mode() == mode)
            .count()
    }

    pub fn shuffled_channels(&self, traffic_type: TrafficType) -> Vec<Arc<Channel>> {
        let mut result: Vec<Arc<Channel>> = self.channels
            .values()
            .filter(|c| {
                #[cfg(not(test))]
                let ready = c.seconds_since_realtime() >= 1.0;
                #[cfg(test)]
                let ready = true;
                c.mode() == ChannelMode::Realtime
                    && !c.is_closed()
                    && ready
                    && !c.should_drop(traffic_type)
            })
            .cloned()
            .collect();
        let mut rng = NullableRng::rng();
        result.shuffle(&mut rng);
        result
    }

    pub fn protocol_info(&self) -> rsnano_core::ProtocolInfo {
        self.protocol.clone()
    }

    pub fn random_fill_realtime(&self, peers: &mut [SocketAddrV6; 8]) {
        let mut candidates: Vec<&Arc<Channel>> = self
            .channels
            .values()
            .filter(|c| c.mode() == ChannelMode::Realtime && c.is_alive())
            .collect();
        let mut rng = NullableRng::rng();
        candidates.shuffle(&mut rng);

        for (i, channel) in candidates.into_iter().take(8).enumerate() {
            let mut peer = if let Some(peering) = channel.peering_addr() {
                peering
            } else {
                channel.peer_addr()
            };

            // For the Nano network, we should always advertise the standard listening port
            // unless we explicitly know otherwise. Ephemeral ports must never be sent.
            if peer.port() != 7075 {
                peer.set_port(7075);
            }

            peers[i] = peer;
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
            info.queued_messages += channel.queue_len();
        }
        info
    }
}

#[repr(C, align(16))]
#[derive(Default)]
pub struct ChannelsInfo {
    pub total: usize,
    pub realtime: usize,
    pub inbound: usize,
    pub outbound: usize,
    pub queued_messages: usize,
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
