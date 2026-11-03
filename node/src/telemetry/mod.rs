use alloc::{boxed::Box, collections::VecDeque, sync::Arc};
mod telemetry_factory;

pub use telemetry_factory::TelemetryFactory;

use core::{mem::size_of, net::SocketAddrV6, time::Duration};
use hashbrown::HashMap;
use spin::{Mutex, RwLock};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    BlockHash,
};
use rsnano_messages::{Message, TelemetryAck, TelemetryData};
use rsnano_network::{Channel, ChannelId, DeadChannelCleanupStep, Network, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp as Instant};
use rsnano_stats::{DetailType, Direction, StatType, Stats};

use crate::{config::NetworkParams, transport::MessageSender};

pub struct Telemetry {
    telemetry_factory: TelemetryFactory,
    config: TelementryConfig,
    stats: Arc<Stats>,
    mutex: Mutex<TelemetryImpl>,
    network_params: NetworkParams,
    network: Arc<RwLock<Network>>,
    message_sender: Mutex<MessageSender>,
    pub startup_time: Instant,
    telemetry_processed_callbacks:
        Mutex<alloc::vec::Vec<Box<dyn Fn(&TelemetryData, &SocketAddrV6) + Send + Sync>>>,
    clock: Arc<SteadyClock>,
    genesis_hash: BlockHash,
}

impl Telemetry {
    const MAX_SIZE: usize = 1024;

    pub(crate) fn new(
        telemetry_factory: TelemetryFactory,
        config: TelementryConfig,
        stats: Arc<Stats>,
        genesis_hash: BlockHash,
        network_params: NetworkParams,
        network: Arc<RwLock<Network>>,
        message_sender: MessageSender,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            telemetry_factory,
            config,
            stats,
            network_params,
            network,
            message_sender: Mutex::new(message_sender),
            mutex: Mutex::new(TelemetryImpl {
                stopped: false,
                telemetries: Default::default(),
                last_broadcast: None,
            }),
            telemetry_processed_callbacks: Mutex::new(alloc::vec::Vec::new()),
            startup_time: clock.now(),
            clock,
            genesis_hash,
        }
    }

    pub fn stop(&self) {
        self.mutex.lock().stopped = true;
    }

    pub fn on_telemetry_processed(
        &self,
        f: Box<dyn Fn(&TelemetryData, &SocketAddrV6) + Send + Sync>,
    ) {
        self.telemetry_processed_callbacks.lock().push(f);
    }

    pub fn process(&self, telemetry: &TelemetryAck, channel: &Channel) {
        let Some(data) = &telemetry.0 else {
            self.stats
                .inc_dir(StatType::Telemetry, DetailType::EmptyPayload, Direction::In);
            return;
        };

        if data.genesis_block != self.genesis_hash {
            self.stats.inc_dir(
                StatType::Telemetry,
                DetailType::GenesisMismatch,
                Direction::In,
            );
            return;
        }

        if let Some(node_id) = channel.node_id() {
            if data.node_id != node_id {
                self.stats.inc_dir(
                    StatType::Telemetry,
                    DetailType::NodeIdMismatch,
                    Direction::In,
                );
                return;
            }
        }

        if !data.validate_signature() {
            self.stats.inc_dir(
                StatType::Telemetry,
                DetailType::InvalidSignature,
                Direction::In,
            );
            return;
        }

        let peer_addr = channel.peering_addr().unwrap_or_else(|| channel.peer_addr());
        let now = self.clock.now();
        {
            let mut guard = self.mutex.lock();
            if guard.stopped {
                return;
            }
            guard.telemetries.insert(
                Entry {
                    channel_id: channel.channel_id(),
                    endpoint: peer_addr,
                    data: data.clone(),
                    last_updated: now,
                },
                Self::MAX_SIZE,
            );
        }

        self.stats
            .inc_dir(StatType::Telemetry, DetailType::Process, Direction::In);

        let callbacks = self.telemetry_processed_callbacks.lock();
        for callback in callbacks.iter() {
            (callback)(data, &peer_addr);
        }
    }

    pub fn trigger_if_due(&self) {
        let now = self.clock.now();
        let interval = if self.network_params.network.is_dev_network() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(60)
        };

        let should_broadcast = {
            let mut guard = self.mutex.lock();
            if guard.stopped {
                return;
            }
            guard.telemetries.cleanup_outdated(now, interval * 3, &self.stats);
            if !self.config.enable_ongoing_broadcasts {
                return;
            }
            match guard.last_broadcast {
                Some(last) if last.elapsed(now) < interval => false,
                _ => {
                    guard.last_broadcast = Some(now);
                    true
                }
            }
        };

        if should_broadcast {
            let channels = self.network.read().sorted_channels();
            let mut sender = self.message_sender.lock();
            for channel in channels {
                if channel.mode() == rsnano_network::ChannelMode::Realtime {
                    sender.try_send(&channel, &Message::TelemetryReq, TrafficType::Telemetry);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.mutex.lock().telemetries.len()
    }

    pub fn local_telemetry(&self) -> TelemetryData {
        self.telemetry_factory.get_telemetry()
    }

    pub fn get_telemetry(&self, endpoint: &SocketAddrV6) -> Option<TelemetryData> {
        self.mutex.lock().telemetries.get_by_endpoint(endpoint)
    }

    pub fn get_all_telemetries(&self) -> HashMap<SocketAddrV6, TelemetryData> {
        self.mutex.lock().telemetries.get_all()
    }
}

pub fn rsnano_version_string() -> alloc::string::String {
    alloc::format!("RsNano V3.0.0-no_std")
}

pub fn rsnano_build_info() -> alloc::string::String {
    alloc::format!("RsNano no_std build")
}

pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: alloc::string::String,
}

pub fn rsnano_version() -> Version {
    Version {
        major: 2,
        minor: 0,
        patch: 0,
        pre: alloc::string::String::new(),
    }
}

pub fn get_pre_release_version(_v: &Version) -> u8 {
    0
}

impl ContainerInfoProvider for Telemetry {
    fn container_info(&self) -> ContainerInfo {
        let len = self.len();
        [("telemetries", len, OrderedTelemetries::ELEMENT_SIZE)].into()
    }
}

#[derive(Clone, Default)]
pub struct TelementryConfig {
    pub enable_ongoing_broadcasts: bool,
}

pub trait TelementryExt {
    fn start(&self);
}

struct TelemetryImpl {
    stopped: bool,
    telemetries: OrderedTelemetries,
    last_broadcast: Option<Instant>,
}

impl TelementryExt for Arc<Telemetry> {
    fn start(&self) {
        self.mutex.lock().stopped = false;
    }
}

#[derive(Default)]
struct OrderedTelemetries {
    by_channel_id: HashMap<ChannelId, Entry>,
    by_endpoint: HashMap<SocketAddrV6, ChannelId>,
    sequenced: VecDeque<ChannelId>,
}

impl OrderedTelemetries {
    pub const ELEMENT_SIZE: usize = size_of::<Entry>() + size_of::<SocketAddrV6>() * 2;

    fn len(&self) -> usize {
        self.sequenced.len()
    }

    fn insert(&mut self, entry: Entry, max_size: usize) {
        let channel_id = entry.channel_id;
        let endpoint = entry.endpoint;
        if let Some(old) = self.by_channel_id.insert(channel_id, entry) {
            if old.endpoint != endpoint {
                self.by_endpoint.remove(&old.endpoint);
            }
            self.sequenced.retain(|i| *i != channel_id);
        }
        self.by_endpoint.insert(endpoint, channel_id);
        self.sequenced.push_back(channel_id);

        while self.sequenced.len() > max_size {
            if let Some(oldest_id) = self.sequenced.pop_front() {
                if let Some(removed) = self.by_channel_id.remove(&oldest_id) {
                    self.by_endpoint.remove(&removed.endpoint);
                }
            }
        }
    }

    fn get_by_endpoint(&self, endpoint: &SocketAddrV6) -> Option<TelemetryData> {
        let channel_id = self.by_endpoint.get(endpoint)?;
        self.by_channel_id.get(channel_id).map(|e| e.data.clone())
    }

    fn get_all(&self) -> HashMap<SocketAddrV6, TelemetryData> {
        self.by_channel_id
            .values()
            .map(|e| (e.endpoint, e.data.clone()))
            .collect()
    }

    fn cleanup_outdated(&mut self, now: Instant, cutoff: Duration, stats: &Stats) {
        let stale: alloc::vec::Vec<ChannelId> = self
            .by_channel_id
            .values()
            .filter(|e| e.last_updated.elapsed(now) >= cutoff)
            .map(|e| e.channel_id)
            .collect();
        for id in stale {
            self.remove(id);
            stats.inc(StatType::Telemetry, DetailType::CleanupOutdated);
        }
    }

    fn remove(&mut self, channel_id: ChannelId) {
        if let Some(entry) = self.by_channel_id.remove(&channel_id) {
            self.by_endpoint.remove(&entry.endpoint);
            self.sequenced.retain(|i| *i != channel_id);
        }
    }
}

struct Entry {
    channel_id: ChannelId,
    endpoint: SocketAddrV6,
    data: TelemetryData,
    last_updated: Instant,
}

impl DeadChannelCleanupStep for Telemetry {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut guard = self.mutex.lock();
        for channel_id in dead_channel_ids {
            guard.telemetries.remove(*channel_id);
        }
    }
}

pub struct TelemetryDeadChannelCleanup(pub Arc<Telemetry>);

impl DeadChannelCleanupStep for TelemetryDeadChannelCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        self.0.clean_up_dead_channels(dead_channel_ids);
    }
}
