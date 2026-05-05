use crate::{
    bandwidth_limiter::BandwidthLimiter,
    channel_stats::ChannelStats,
    utils::{ipv4_address_or_ipv6_subnet, map_address_to_subnetwork, CancellationToken, into_core_addr},
    write_queue::{Entry, WriteQueue},
    ChannelDirection, ChannelId, ChannelMode, TrafficType,
};
use num_traits::FromPrimitive;
use rsnano_core::{
    utils::{TEST_ENDPOINT_1, TEST_ENDPOINT_2},
    NodeId,
};
use rsnano_nullable_clock::Timestamp;
use core::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering},
    time::Duration,
};
use alloc::sync::Arc;
use spin::Mutex;
use tracing::debug;

/// Default timeout in seconds
const DEFAULT_TIMEOUT: u64 = 120;

pub struct Channel {
    channel_id: ChannelId,
    local_addr: SocketAddrV6,
    peer_addr: SocketAddrV6,
    data: Mutex<PeerInfo>,
    protocol_version: AtomicU8,
    direction: ChannelDirection,

    /// the timestamp (in seconds since epoch) of the last time there was successful activity on the socket
    last_activity: AtomicI64,
    last_bootstrap_attempt: AtomicI64,

    /// Duration in seconds of inactivity that causes a socket timeout
    /// activity is any successful connect, send or receive event
    timeout_seconds: AtomicU64,

    /// Flag that is set when cleanup decides to close the socket due to timeout.
    timed_out: AtomicBool,

    /// Set by close() - completion handlers must check this. This is more reliable than checking
    /// error codes as the OS may have already completed the async operation.
    closed: AtomicBool,

    socket_type: AtomicU8,
    write_queue: WriteQueue,
    cancel_token: CancellationToken,
    limiter: Arc<BandwidthLimiter>,
    stats: Arc<ChannelStats>,
}

impl Channel {
    pub const MAX_QUEUE_SIZE: usize = 64;

    pub(crate) fn new(
        channel_id: ChannelId,
        local_addr: SocketAddrV6,
        peer_addr: SocketAddrV6,
        direction: ChannelDirection,
        protocol_version: u8,
        now: Timestamp,
        limiter: Arc<BandwidthLimiter>,
        stats: Arc<ChannelStats>,
    ) -> Self {
        Self {
            channel_id,
            local_addr,
            peer_addr,
            protocol_version: AtomicU8::new(protocol_version),
            direction,
            last_activity: AtomicI64::new(now.millis()),
            last_bootstrap_attempt: AtomicI64::new(0),
            timeout_seconds: AtomicU64::new(DEFAULT_TIMEOUT),
            timed_out: AtomicBool::new(false),
            socket_type: AtomicU8::new(ChannelMode::Undefined as u8),
            closed: AtomicBool::new(false),
            data: Mutex::new(PeerInfo {
                node_id: None,
                peering_addr: if direction == ChannelDirection::Outbound {
                    Some(peer_addr)
                } else {
                    None
                },
            }),
            write_queue: WriteQueue::new(Self::MAX_QUEUE_SIZE, stats.clone()),
            cancel_token: CancellationToken::new(),
            limiter,
            stats,
        }
    }

    pub fn new_test_instance() -> Self {
        Self::new_test_instance_with_id(42)
    }

    pub fn new_test_instance_with_id(id: impl Into<ChannelId>) -> Self {
        Self::new(
            id.into(),
            into_core_addr(TEST_ENDPOINT_1),
            into_core_addr(TEST_ENDPOINT_2),
            ChannelDirection::Outbound,
            u8::MAX,
            Timestamp::new_test_instance(),
            Arc::new(BandwidthLimiter::default()),
            Arc::new(ChannelStats::default()),
        )
    }

    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    pub fn node_id(&self) -> Option<NodeId> {
        self.data.lock().node_id
    }

    pub fn direction(&self) -> ChannelDirection {
        self.direction
    }

    pub fn local_addr(&self) -> SocketAddrV6 {
        self.local_addr
    }

    pub fn peer_addr(&self) -> SocketAddrV6 {
        self.peer_addr
    }

    pub fn peering_addr(&self) -> Option<SocketAddrV6> {
        self.data.lock().peering_addr
    }

    pub fn peering_addr_or_peer_addr(&self) -> SocketAddrV6 {
        self.data
            .lock()
            .peering_addr
            .unwrap_or(self.peer_addr)
    }

    pub fn ipv4_address_or_ipv6_subnet(&self) -> Ipv6Addr {
        ipv4_address_or_ipv6_subnet(self.peer_addr().ip())
    }

    pub fn subnetwork(&self) -> Ipv6Addr {
        map_address_to_subnetwork(self.peer_addr().ip())
    }

    pub fn protocol_version(&self) -> u8 {
        self.protocol_version.load(Ordering::Relaxed)
    }

    pub fn set_protocol_version(&self, version: u8) {
        self.protocol_version.store(version, Ordering::Relaxed);
    }

    pub fn last_activity(&self) -> Timestamp {
        Timestamp::new(self.last_activity.load(Ordering::Relaxed) as i128 * 1_000_000)
    }

    pub fn set_last_activity(&self, now: Timestamp) {
        self.last_activity.store(now.millis(), Ordering::Relaxed);
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds.load(Ordering::Relaxed))
    }

    pub fn set_timeout(&self, value: Duration) {
        self.timeout_seconds
            .store(value.as_secs(), Ordering::Relaxed)
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Relaxed)
    }

    pub fn set_timed_out(&self, value: bool) {
        self.timed_out.store(value, Ordering::Relaxed)
    }

    pub fn is_alive(&self) -> bool {
        !self.is_closed()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub fn write_error(&self) {
        self.stats.write_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn close(&self) {
        let already_closed = self.closed.swap(true, Ordering::Relaxed);
        if already_closed {
            return;
        }
        self.set_timeout(Duration::ZERO);
        self.write_queue.close();
        self.cancel_token.cancel();
    }

    pub fn set_node_id(&self, node_id: NodeId) {
        self.data.lock().node_id = Some(node_id);
    }

    pub fn set_peering_addr(&self, peering_addr: SocketAddrV6) {
        self.data.lock().peering_addr = Some(peering_addr);
    }

    pub fn mode(&self) -> ChannelMode {
        FromPrimitive::from_u8(self.socket_type.load(Ordering::SeqCst)).unwrap()
    }

    pub fn set_mode(&self, mode: ChannelMode) {
        self.socket_type.store(mode as u8, Ordering::SeqCst);
    }

    pub fn last_bootstrap_attempt(&self) -> Timestamp {
        Timestamp::new(self.last_bootstrap_attempt.load(Ordering::Relaxed) as i128 * 1_000_000)
    }

    pub fn set_last_bootstrap_attempt(&self, now: Timestamp) {
        self.last_bootstrap_attempt
            .store(now.millis(), Ordering::Relaxed);
    }

    pub fn should_drop(&self, traffic_type: TrafficType) -> bool {
        self.write_queue.free_capacity(traffic_type) == 0
    }

    pub fn send(&self, buffer: &[u8], traffic_type: TrafficType) -> bool {
        if self.is_closed() {
            return false;
        }

        if self.should_drop(traffic_type) {
            return false;
        }

        let should_pass = self.limiter.should_pass(buffer.len(), traffic_type);
        if !should_pass {
            return false;
        }

        let inserted = self
            .write_queue
            .try_insert(Arc::new(buffer.to_vec()), traffic_type);
        inserted
    }

    pub fn free_capacity(&self, traffic_type: TrafficType) -> usize {
        self.write_queue.free_capacity(traffic_type)
    }

    pub fn queue_len(&self) -> usize {
        self.write_queue.len()
    }

    pub async fn pop(&self) -> Option<Entry> {
        self.write_queue.pop().await
    }

    pub fn cancelled(&self) -> crate::utils::WaitForCancellationFuture<'_> {
        self.cancel_token.cancelled()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    pub fn check_timeout(&self, now: Timestamp) -> bool {
        if !self.is_alive() {
            return true;
        }

        let has_timed_out = (now.millis() - self.last_activity().millis()) > self.timeout().as_millis() as i64;
        if has_timed_out {
            self.stats.timed_out.fetch_add(1, Ordering::Relaxed);

            debug!(
            channel_id = %self.channel_id(),
            remote_addr = ?self.peer_addr(),
            mode = ?self.mode(),
            direction = ?self.direction(),
            "Closing channel due to timeout");

            self.set_timed_out(true);
            self.close();
        }
        has_timed_out
    }

    pub fn read_succeeded(&self, count: usize, now: Timestamp) {
        self.stats
            .read_succeeded
            .fetch_add(count, Ordering::Relaxed);
        self.set_last_activity(now);
    }

    pub fn read_failed(&self) {
        self.stats.read_failed.fetch_add(1, Ordering::Relaxed);
        self.close();
    }
}

impl PartialEq for Channel {
    fn eq(&self, other: &Self) -> bool {
        self.channel_id() == other.channel_id()
    }
}

impl Eq for Channel {}

impl core::fmt::Debug for Channel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Channel")
            .field("id", &self.channel_id.as_usize())
            .field("peer", &self.peer_addr())
            .finish()
    }
}

struct PeerInfo {
    node_id: Option<NodeId>,
    peering_addr: Option<SocketAddrV6>,
}
