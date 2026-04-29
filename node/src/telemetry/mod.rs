mod telemetry_factory;

pub use telemetry_factory::TelemetryFactory;

use alloc::sync::{ Arc };
use alloc::collections::{ VecDeque };
use spin::{ Mutex, RwLock };
use hashbrown::{ HashMap };
use core::time::Duration;
use core::mem::size_of;
use core::cmp::min;
use core::net::SocketAddrV6;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    BlockHash, PublicKey,
};
use rsnano_messages::{Message, TelemetryAck, TelemetryData};
use rsnano_network::{Channel, ChannelId, DeadChannelCleanupStep, Network, TrafficType};
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_clock::Timestamp as Instant;
use rsnano_stats::{DetailType, StatType, Stats, StatsCollection, StatsSource};

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
        _f: Box<dyn Fn(&TelemetryData, &SocketAddrV6) + Send + Sync>,
    ) {
    }

    pub fn process(&self, _telemetry: &TelemetryAck, _channel: &Channel) {}
    pub fn len(&self) -> usize { 0 }
    pub fn local_telemetry(&self) -> TelemetryData { self.telemetry_factory.get_telemetry() }
    pub fn get_telemetry(&self, _endpoint: &SocketAddrV6) -> Option<TelemetryData> { None }
    pub fn get_all_telemetries(&self) -> HashMap<SocketAddrV6, TelemetryData> { HashMap::new() }
}

pub fn rsnano_version_string() -> alloc::string::String {
    alloc::format!("RsNano V2.0.0-no_std")
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
    Version { major: 2, minor: 0, patch: 0, pre: alloc::string::String::new() }
}

pub fn get_pre_release_version(_v: &Version) -> u8 { 0 }

impl ContainerInfoProvider for Telemetry {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::new()
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
    fn start(&self) {}
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
