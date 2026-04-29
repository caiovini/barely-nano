use crate::{TcpNetworkAdapter, utils::CancellationToken};
use core::future::pending;
use alloc::sync::Arc;
use kernel_common::async_rt;
use tracing::debug;

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

        async_rt::spawn(async move {
            let run_loop = async {
                loop {
                    if cancel_token.is_cancelled() {
                        break;
                    }

                    network_adapter.wait_for_available_inbound_slot().await;

                    if cancel_token.is_cancelled() {
                        break;
                    }

                    // For now, we don't have a real AsyncTcpListener implementation in kernel-common
                    // that works with the smoltcp stack in a way we can use here yet.
                    // This will be implemented when the networking stack is further along.
                    debug!("TCP Listener: waiting for incoming connections (stub)");
                    pending::<()>().await;
                }
            };

            let _ = core::future::poll_fn(|cx| {
                if cancel_token.is_cancelled() {
                    core::task::Poll::Ready(())
                } else {
                    // This is a stub, so we just stay pending
                    core::task::Poll::Pending
                }
            });
            
            run_loop.await;
        });
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();
    }
}
