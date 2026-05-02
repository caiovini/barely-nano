use crate::{ChannelDirection, NetworkError, TcpNetworkAdapter, utils::CancellationToken};
use rsnano_nullable_tcp::{TcpSocket, TcpStream};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use core::{net::SocketAddrV6, time::Duration};
use alloc::sync::Arc;
use alloc::boxed::Box;
use alloc::string::ToString;
use tracing::debug;
use kernel_common::async_rt;
use core::future::{poll_fn, Future};
use core::task::Poll;
use core::pin::Pin;

/// Establishes a network connection to a given peer
pub struct PeerConnector {
    connect_timeout: Duration,
    network_adapter: Arc<TcpNetworkAdapter>,
    cancel_token: CancellationToken,
    connect_listener: OutputListenerMt<SocketAddrV6>,
}

impl PeerConnector {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(
        connect_timeout: Duration,
        network_adapter: Arc<TcpNetworkAdapter>,
    ) -> Self {
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
        self.connect_listener.emit(peer);

        if self.cancel_token.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }

        tracing::info!("[node] Attempting to connect to peer: {}", peer);
        self.network_adapter.add_outbound_attempt(peer)?;

        let network_l = self.network_adapter.clone();
        let cancel_token = self.cancel_token.clone();

        async_rt::spawn(async move {
            let mut connect_fut = Box::pin(connect_impl(peer, &network_l));
            let mut cancel_fut = Box::pin(cancel_token.cancelled());
            
            poll_fn(|cx| {
                if let Poll::Ready(_) = Pin::new(&mut cancel_fut).poll(cx) {
                    debug!("Connection attempt cancelled: {}", peer);
                    return Poll::Ready(());
                }
                if let Poll::Ready(_) = Pin::new(&mut connect_fut).poll(cx) {
                    return Poll::Ready(());
                }
                Poll::Pending
            }).await;

            network_l.remove_attempt(&peer);
        });

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
            network_adapter.connect_error(peer, e);
        }
    }
}

async fn connect_stream(peer: SocketAddrV6) -> anyhow::Result<TcpStream> {
    TcpSocket::new_v6()?.connect(peer.into()).await
}
