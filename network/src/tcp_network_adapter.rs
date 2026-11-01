use crate::{
    utils::into_core_addr, Channel, ChannelDirection, ChannelId, DeadChannelCleanupStep, Network,
    NetworkError, ReceiveResult, TcpChannelAdapter,
};
use alloc::sync::Arc;
use core::{
    future::Future,
    net::{SocketAddr, SocketAddrV6},
    time::Duration,
};
use hashbrown::HashMap;
use kernel_common::async_rt;
use rsnano_core::utils::NULL_ENDPOINT;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_tcp::TcpStream;
use spin::{Mutex, RwLock};
use tracing::warn;

/// Connects the Network to TcpStreams
pub struct TcpNetworkAdapter {
    channel_adapters: Arc<Mutex<HashMap<ChannelId, Arc<TcpChannelAdapter>>>>,
    network: Arc<RwLock<Network>>,
    clock: Arc<SteadyClock>,
}

impl TcpNetworkAdapter {
    pub fn new(network_info: Arc<RwLock<Network>>, clock: Arc<SteadyClock>) -> Self {
        Self {
            channel_adapters: Arc::new(Mutex::new(HashMap::new())),
            clock,
            network: network_info,
        }
    }

    pub fn listening_port(&self) -> u16 {
        self.network.read().listening_port()
    }

    pub async fn wait_for_available_inbound_slot(&self) {
        let mut last_log = self.clock.now();
        let log_interval = Duration::from_secs(15);
        while self.should_wait_for_inbound_slot() {
            if self.clock.now().millis() - last_log.millis() >= log_interval.as_millis() as i64 {
                warn!("Waiting for available slots to accept new connections");
                last_log = self.clock.now();
            }

            // Sleep to yield executor without pegging CPU
            async_rt::sleep(Duration::from_millis(100)).await;
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

        tracing::debug!("Adding new channel: {} ({:?})", peer_addr, direction);
        receiver.ensure_handshake();

        let channel_id = channel.channel_id();
        let channel_adapter = TcpChannelAdapter::create(channel, stream, self.clock.clone());

        self.channel_adapters
            .lock()
            .insert(channel_id, channel_adapter.clone());

        let adapters_l = self.channel_adapters.clone();
        let adapter_l = channel_adapter.clone();
        let clock_l = self.clock.clone();
        async_rt::spawn_named(async move {
            let channel_l = adapter_l.channel.clone();
            let mut buffer = [0u8; 4096];
            let mut consecutive_reads = 0usize;

            loop {
                if channel_l.is_closed() {
                    break;
                }
                if consecutive_reads >= 16 {
                    consecutive_reads = 0;
                    async_rt::yield_now().await;
                }

                // Wait for data or cancellation in a single NET.lock() acquisition
                let read_result = {
                    let mut read_fut = core::pin::pin!(adapter_l.read_chunk(&mut buffer));
                    let mut cancel_fut = core::pin::pin!(channel_l.cancelled());
                    
                    let mut polled_once = false;
                    core::future::poll_fn(|cx| {
                        if let core::task::Poll::Ready(r) = read_fut.as_mut().poll(cx) {
                            if !polled_once {
                                consecutive_reads += 1;
                            } else {
                                consecutive_reads = 0;
                            }
                            return core::task::Poll::Ready(Some(r));
                        }
                        if let core::task::Poll::Ready(_) = cancel_fut.as_mut().poll(cx) {
                            return core::task::Poll::Ready(None);
                        }
                        polled_once = true;
                        consecutive_reads = 0;
                        core::task::Poll::Pending
                    }).await
                };

                let Some(read_res) = read_result else { 
                    break; 
                };
                
                match read_res {
                    Ok(read_count) => {
                        if read_count == 0 {
                            break;
                        }

                        adapter_l.channel.set_last_activity(clock_l.now());
                        let new_data = &buffer[..read_count];
                        match receiver.receive(new_data) {
                            ReceiveResult::Continue => {}
                            ReceiveResult::Abort => {
                                break;
                            }
                            ReceiveResult::Pause => loop {
                                async_rt::sleep(core::time::Duration::from_millis(2)).await;
                                if channel_l.is_cancelled() {
                                    break;
                                }
                                match receiver.try_unpause() {
                                            ReceiveResult::Continue => {
                                                match receiver.receive(&[]) {
                                                    ReceiveResult::Continue => break,
                                                    ReceiveResult::Abort => {
                                                        break;
                                                    }
                                                    ReceiveResult::Pause => {} 
                                                }
                                            }
                                            ReceiveResult::Abort => {
                                                break;
                                            }
                                            ReceiveResult::Pause => {}
                                        }
                                    },
                                }
                            }
                    Err(_) => {
                        break;
                    }
                }
            }
            channel_l.close();
            adapters_l.lock().remove(&channel_id);
        }, Some("TcpChannel Read"));

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
