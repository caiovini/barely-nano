use crate::{utils::CancellationToken, ChannelDirection, NetworkError, TcpNetworkAdapter};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::Poll;
use core::{net::SocketAddrV6, time::Duration};
use kernel_common::async_rt;
use rsnano_nullable_tcp::{TcpSocket, TcpStream};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};

/// Establishes a network connection to a given peer
pub struct PeerConnector {
    #[allow(dead_code)]
    connect_timeout: Duration,
    network_adapter: Arc<TcpNetworkAdapter>,
    cancel_token: CancellationToken,
    connect_listener: OutputListenerMt<SocketAddrV6>,
}

impl PeerConnector {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(connect_timeout: Duration, network_adapter: Arc<TcpNetworkAdapter>) -> Self {
        Self {
            connect_timeout,
            network_adapter,
            cancel_token: CancellationToken::new(),
            connect_listener: OutputListenerMt::new(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            connect_timeout: Self::DEFAULT_TIMEOUT,
            network_adapter: Arc::new(TcpNetworkAdapter::new_null()),
            cancel_token: CancellationToken::new(),
            connect_listener: OutputListenerMt::new(),
        }
    }

    pub fn track_connections(&self) -> Arc<OutputTrackerMt<SocketAddrV6>> {
        self.connect_listener.track()
    }

    /// Establish a network connection to the given peer
    pub fn connect_to(&self, peer: SocketAddrV6) -> Result<(), NetworkError> {
        kernel_common::uart::print(b"[peer] Reachout to ");
        let mut ip_str = alloc::string::String::new();
        let _ = core::fmt::write(&mut ip_str, format_args!("{}", peer));
        kernel_common::uart::print(ip_str.as_bytes());
        kernel_common::uart::print(b"\n");
        self.connect_listener.emit(peer);

        if self.cancel_token.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }

        match self.network_adapter.add_outbound_attempt(peer) {
            Ok(_) => {}
            Err(NetworkError::DuplicateConnection) => {
                // Silently skip if already attempting or connected
                return Ok(());
            }
            Err(e) => return Err(e),
        }

        let network_l = self.network_adapter.clone();
        let cancel_token = self.cancel_token.clone();

        async_rt::spawn_named(async move {
            // Add a timeout to the connection attempt to prevent task leaks
            let mut connect_fut = Box::pin(connect_impl(peer, &network_l));
            let mut cancel_fut = Box::pin(cancel_token.cancelled());

            let start = kernel_common::clint::mtime();
            let timeout_ticks = 5 * kernel_common::clint::FREQ;

            loop {
                let mtime = kernel_common::clint::mtime();
                if mtime - start > timeout_ticks {
                    break;
                }

                // Try polling the futures once
                let is_done = poll_fn(|cx| {
                    if let Poll::Ready(_) = Pin::new(&mut cancel_fut).poll(cx) {
                        return Poll::Ready(true);
                    }
                    if let Poll::Ready(_) = Pin::new(&mut connect_fut).poll(cx) {
                        return Poll::Ready(true);
                    }
                    Poll::Ready(false)
                }).await;

                if is_done {
                    break;
                }

                async_rt::yield_now().await;
            }

            network_l.remove_attempt(&peer);
        }, Some("Peer Connection"));

        Ok(())
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();
    }
}

async fn connect_impl(peer: SocketAddrV6, network_adapter: &TcpNetworkAdapter) {
    match connect_stream(peer).await {
        Ok(stream) => {
            let _ = network_adapter.add(stream, ChannelDirection::Outbound);
        }
        Err(e) => {
            kernel_common::uart::print(b"[peer] Connect failed to ");
            let mut ip_str = alloc::string::String::new();
            let _ = core::fmt::write(&mut ip_str, format_args!("{}", peer));
            kernel_common::uart::print(ip_str.as_bytes());
            kernel_common::uart::print(b"\n");
            network_adapter.connect_error(peer, e);
        }
    }
}

async fn connect_stream(peer: SocketAddrV6) -> anyhow::Result<TcpStream> {
    TcpSocket::new_v6()?.connect(peer.into()).await
}
