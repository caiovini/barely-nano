use crate::{TcpNetworkAdapter, utils::CancellationToken};
use alloc::sync::Arc;
use kernel_common::async_rt;

pub struct TcpListener {
    network_adapter: Arc<TcpNetworkAdapter>,
    cancel_token: CancellationToken,
}

impl TcpListener {
    pub fn new(
        network_adapter: Arc<TcpNetworkAdapter>,
    ) -> Self {
        Self {
            network_adapter,
            cancel_token: CancellationToken::new(),
        }
    }

    pub fn run(&self) {
        let network_adapter = self.network_adapter.clone();
        let cancel_token = self.cancel_token.clone();

        async_rt::spawn_named(async move {
            loop {
                if cancel_token.is_cancelled() {
                    break;
                }

                network_adapter.wait_for_available_inbound_slot().await;

                if cancel_token.is_cancelled() {
                    break;
                }

                let handle = if let Some(ref mut ctx) = *::kernel_common::net::NET.lock() {
                    ctx.accept_peering()
                } else {
                    None
                };

                if let Some(handle) = handle {
                    let async_stream = ::kernel_common::net::async_tcp::AsyncTcpStream::new(handle);
                    let stream = ::rsnano_nullable_tcp::TcpStream::new(async_stream);
                    let _ = network_adapter.add(stream, crate::ChannelDirection::Inbound);
                } else {
                    ::kernel_common::async_rt::sleep(core::time::Duration::from_millis(10)).await;
                }
            }
        }, Some("TcpListener"));
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();
    }
}
