#![no_std]

extern crate alloc;
extern crate kernel_common;

pub mod attempt_container;
pub mod bandwidth_limiter;
mod channel;
mod channel_stats;
mod dead_channel_cleanup;
mod network;
mod peer_connector;
pub mod peer_exclusion;
mod tcp_channel_adapter;
mod tcp_listener;
mod tcp_network_adapter;
pub mod token_bucket;
pub mod utils;
pub mod write_queue;

pub use channel::*;
pub use dead_channel_cleanup::*;
pub use network::*;
pub use peer_connector::*;
pub use tcp_channel_adapter::*;
pub use tcp_listener::*;
pub use tcp_network_adapter::*;

use core::fmt::{Debug, Display};
use alloc::sync::Arc;
use alloc::boxed::Box;
use num_derive::FromPrimitive;
use strum::{EnumCount, EnumIter, IntoStaticStr};

use rsnano_stats::DetailType;

#[macro_use]
extern crate anyhow;

#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub struct ChannelId(usize);

impl ChannelId {
    pub const LOOPBACK: Self = Self(0);
    pub const MIN: Self = Self(usize::MIN);
    pub const MAX: Self = Self(usize::MAX);

    pub fn as_usize(&self) -> usize {
        self.0
    }
}

impl Display for ChannelId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

impl Debug for ChannelId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

impl From<usize> for ChannelId {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, FromPrimitive, Debug)]
pub enum ChannelDirection {
    /// Socket was created by accepting an incoming connection
    Inbound,
    /// Socket was created by initiating an outgoing connection
    Outbound,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, EnumCount, EnumIter, IntoStaticStr, Default,
)]
#[strum(serialize_all = "snake_case")]
pub enum TrafficType {
    #[default]
    Generic,
    /// Ascending bootstrap (asc_pull_ack, asc_pull_req) traffic
    BootstrapServer,
    BootstrapRequests,
    BlockBroadcast,
    BlockBroadcastInitial,
    BlockBroadcastRpc,
    ConfirmationRequests,
    Keepalive,
    Vote,
    VoteRebroadcast,
    VoteReply,
    RepCrawler,
    Telemetry,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, FromPrimitive)]
pub enum ChannelMode {
    /// No messages have been exchanged yet, so the mode is undefined
    Undefined,
    /// serve realtime traffic (votes, new blocks,...)
    Realtime,
}

impl ChannelMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelMode::Undefined => "undefined",
            ChannelMode::Realtime => "realtime",
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait AsyncBufferReader {
    async fn read(&self, buffer: &mut [u8], count: usize) -> anyhow::Result<()>;
}

pub trait AsyncDnsResolver: Send + Sync {
    fn resolve(
        &self,
        name: &str,
    ) -> core::pin::Pin<
        alloc::boxed::Box<
            dyn core::future::Future<
                    Output = anyhow::Result<alloc::vec::Vec<core::net::IpAddr>>,
                > + Send,
        >,
    >;
}

pub trait DataReceiverFactory: Send + Sync {
    fn create_receiver_for(&self, channel: Arc<Channel>) -> Box<dyn DataReceiver + Send>;
}

pub enum ReceiveResult {
    Continue,
    Abort,
    Pause,
}

pub trait DataReceiver: Send {
    fn receive(&mut self, data: &[u8]) -> ReceiveResult;
    /// after receive returns Pause this has to be called until it returns true
    fn try_unpause(&self) -> ReceiveResult;
    fn ensure_handshake(&mut self) {}
}

pub struct NullDataReceiverFactory;

impl NullDataReceiverFactory {
    pub fn new() -> Self {
        Self
    }
}

impl DataReceiverFactory for NullDataReceiverFactory {
    fn create_receiver_for(&self, _channel: Arc<Channel>) -> Box<dyn DataReceiver + Send> {
        Box::new(NullDataReceiver::new())
    }
}

pub struct NullDataReceiver;

impl NullDataReceiver {
    pub fn new() -> Self {
        Self
    }
}

impl DataReceiver for NullDataReceiver {
    fn receive(&mut self, _: &[u8]) -> ReceiveResult {
        ReceiveResult::Continue
    }

    fn try_unpause(&self) -> ReceiveResult {
        ReceiveResult::Continue
    }
}

impl From<ChannelDirection> for rsnano_stats::Direction {
    fn from(value: ChannelDirection) -> Self {
        match value {
            ChannelDirection::Inbound => rsnano_stats::Direction::In,
            ChannelDirection::Outbound => rsnano_stats::Direction::Out,
        }
    }
}

impl From<TrafficType> for DetailType {
    fn from(value: TrafficType) -> Self {
        match value {
            TrafficType::Generic => DetailType::Generic,
            TrafficType::BootstrapServer => DetailType::BootstrapServer,
            TrafficType::BootstrapRequests => DetailType::BootstrapRequests,
            TrafficType::BlockBroadcast => DetailType::BlockBroadcast,
            TrafficType::BlockBroadcastInitial => DetailType::BlockBroadcastInitial,
            TrafficType::BlockBroadcastRpc => DetailType::BlockBroadcastRpc,
            TrafficType::ConfirmationRequests => DetailType::ConfirmationRequests,
            TrafficType::Keepalive => DetailType::Keepalive,
            TrafficType::Vote => DetailType::Vote,
            TrafficType::VoteRebroadcast => DetailType::VoteRebroadcast,
            TrafficType::RepCrawler => DetailType::RepCrawler,
            TrafficType::VoteReply => DetailType::VoteReply,
            TrafficType::Telemetry => DetailType::Telemetry,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkConfig {
    pub listening_port: u16,
    pub max_inbound_connections: usize,
    pub max_outbound_connections: usize,
    pub peer_reachout: core::time::Duration,
    pub cached_peer_reachout: core::time::Duration,
}

impl NetworkConfig {
    pub fn default_for(_network: rsnano_core::Networks) -> Self {
        Self {
            listening_port: 0,
            max_inbound_connections: 128,
            max_outbound_connections: 128,
            peer_reachout: core::time::Duration::from_secs(1),
            cached_peer_reachout: core::time::Duration::from_secs(1),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self::default_for(rsnano_core::Networks::NanoLiveNetwork)
    }
}

pub struct NullDnsResolver;

impl AsyncDnsResolver for NullDnsResolver {
    fn resolve(
        &self,
        _name: &str,
    ) -> core::pin::Pin<
        alloc::boxed::Box<
            dyn core::future::Future<
                    Output = anyhow::Result<alloc::vec::Vec<core::net::IpAddr>>,
                > + Send,
        >,
    > {
        alloc::boxed::Box::pin(async { Ok(alloc::vec::Vec::new()) })
    }
}
