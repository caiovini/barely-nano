use crate::{
    utils::{into_core_addr}, Channel, ChannelDirection, ChannelId, DeadChannelCleanupStep,
    Network, NetworkError, ReceiveResult, TcpChannelAdapter,
};
use rsnano_core::utils::NULL_ENDPOINT;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_tcp::TcpStream;
use core::{
    net::{SocketAddr, SocketAddrV6},
    time::Duration,
    future::poll_fn,
    task::Poll,
};
use alloc::sync::Arc;
use alloc::string::ToString;
use hashbrown::HashMap;
use spin::{Mutex, RwLock};
use tracing::{warn, info};
use kernel_common::async_rt;

/// Connects the Network to TcpStreams
pub struct TcpNetworkAdapter {
    channel_adapters: Mutex<HashMap<ChannelId, Arc<TcpChannelAdapter>>>,
    network: Arc<RwLock<Network>>,
    clock: Arc<SteadyClock>,
}

impl TcpNetworkAdapter {
    pub fn new(
        network_info: Arc<RwLock<Network>>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            channel_adapters: Mutex::new(HashMap::new()),
            clock,
            network: network_info,
        }
    }

    pub async fn wait_for_available_inbound_slot(&self) {
        let mut last_log = self.clock.now();
        let log_interval = Duration::from_secs(15);
        while self.should_wait_for_inbound_slot() {
            if self.clock.now().millis() - last_log.millis() >= log_interval.as_millis() as i64 {
                warn!("Waiting for available slots to accept new connections");
                last_log = self.clock.now();
            }

            // Simple yielding wait
            async_rt::yield_now().await;
            for _ in 0..100 { core::hint::spin_loop(); }
        }
    }

    fn should_wait_for_inbound_slot(&self) -> bool {
        let network = self.network.read();
        !network.is_inbound_slot_available() && !network.is_stopped()
    }

    pub fn add(
        &self,
        stream: TcpStream,
        direction: ChannelDirection,
    ) -> anyhow::Result<Arc<Channel>> {
        let peer_addr = stream
            .peer_addr()
            .map(|addr| match addr {
                SocketAddr::V6(v6) => v6,
                SocketAddr::V4(v4) => SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0),
            })
            .unwrap_or(into_core_addr(NULL_ENDPOINT));

        let local_addr = stream
            .local_addr()
            .map(|addr| match addr {
                SocketAddr::V6(v6) => v6,
                SocketAddr::V4(v4) => SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0),
            })
            .unwrap_or(into_core_addr(NULL_ENDPOINT));

        let (channel, mut receiver) = self
            .network
            .write()
            .add(local_addr, peer_addr, direction, self.clock.now())
            .map_err(|e| anyhow!("Could not add channel: {:?}", e))?;

        receiver.ensure_handshake();

        let channel_id = channel.channel_id();
        let channel_adapter =
            TcpChannelAdapter::create(channel, stream, self.clock.clone());

        self.channel_adapters
            .lock()
            .insert(channel_id, channel_adapter.clone());

        let adapter_l = channel_adapter.clone();
        async_rt::spawn(async move {

            let channel_l = adapter_l.channel.clone();
            let mut buffer = [0u8; 1024];

            loop {
                if channel_l.is_closed() {
                    return;
                }

                // Wait for data
                match adapter_l.readable().await {
                    Ok(()) => {
                    }
                    Err(_e) => {
                        return;
                    }
                }

                let read_result = adapter_l.try_read(&mut buffer);
                match read_result {
                    Ok(read_count) => {
                        if read_count == 0 {
                            break;
                        }

                        let new_data = &buffer[..read_count];
                        match receiver.receive(new_data) {
                            ReceiveResult::Continue => {}
                            ReceiveResult::Abort => {
                                tracing::info!("Receiver task: receiver requested Abort, exiting");
                                break;
                            }
                            ReceiveResult::Pause => {
                                loop {
                                    async_rt::yield_now().await;
                                    for _ in 0..100 { core::hint::spin_loop(); }
                                    if channel_l.is_cancelled() { break; }
                                    match receiver.try_unpause() {
                                        ReceiveResult::Continue => break,
                                        ReceiveResult::Abort => {
                                            return;
                                        }
                                        ReceiveResult::Pause => {}
                                    }
                                }
                            }
                        }
                    },
                    Err(e) => {
                        let msg = alloc::format!("{:?}", e);
                        if msg.contains("Would block") {
                            continue;
                        }
                        return;
                    }
                }
            }
            channel_l.close();
        });

        Ok(channel_adapter.channel.clone())
    }

    pub fn add_outbound_attempt(&self, peer: SocketAddrV6) -> Result<(), NetworkError> {
        self.network
            .write()
            .add_outbound_attempt(peer, self.clock.now())
    }

    pub fn remove_attempt(&self, peer: &SocketAddrV6) {
        self.network.write().remove_attempt(peer);
    }

    pub fn set_listening_port(&self, port: u16) {
        self.network.write().set_listening_port(port);
    }

    pub fn connect_error(&self, peer: SocketAddrV6, e: anyhow::Error) {
        self.network.write().connect_error(peer, e);
    }

    pub fn attempt_timeout(&self, peer: SocketAddrV6) {
        self.network.write().attempt_timeout(peer);
    }

    pub fn accept_failure(&self) {
        self.network.read().accept_failure();
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(RwLock::new(Network::new_test_instance())),
            Arc::new(SteadyClock::new_null()),
        )
    }
}

pub struct NetworkCleanup(Arc<TcpNetworkAdapter>);

impl NetworkCleanup {
    pub fn new(network: Arc<TcpNetworkAdapter>) -> Self {
        Self(network)
    }
}

impl DeadChannelCleanupStep for NetworkCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut channels = self.0.channel_adapters.lock();
        for channel_id in dead_channel_ids {
            channels.remove(channel_id);
        }
    }
}
