use crate::TcpStream;
use core::net::SocketAddr;
use kernel_common::net::NET;
use smoltcp::socket::tcp::Socket as TcpSocket;
use core::future::poll_fn;
use core::task::Poll;

pub struct TcpListener {
    port: u16,
}

impl TcpListener {
    pub async fn bind(addr: SocketAddr) -> anyhow::Result<Self> {
        Ok(Self {
            port: addr.port(),
        })
    }

    pub async fn accept(&self) -> anyhow::Result<(TcpStream, SocketAddr)> {
        poll_fn(|cx| {
            if let Some(ref mut ctx) = *NET.lock() {
                let socket = ctx.sockets.get_mut::<TcpSocket>(ctx.tcp_handle);
                
                if !socket.is_active() && !socket.is_open() {
                    socket.listen(self.port).unwrap();
                }

                match socket.state() {
                    smoltcp::socket::tcp::State::Established => {
                        let remote_endpoint = socket.remote_endpoint().unwrap();
                        let addr = match remote_endpoint.addr {
                            smoltcp::wire::IpAddress::Ipv4(v4) => {
                                SocketAddr::new(core::net::IpAddr::V4(v4.into()), remote_endpoint.port)
                            }
                            smoltcp::wire::IpAddress::Ipv6(v6) => {
                                SocketAddr::new(core::net::IpAddr::V6(v6.into()), remote_endpoint.port)
                            }
                        };

                        // Use the accept-and-re-listen pattern:
                        // 1. Remove the established socket from the handle
                        let established_socket = ctx.sockets.remove(ctx.tcp_handle);
                        
                        // 2. Create a new listener socket and put it back into the handle
                        use smoltcp::socket::tcp::SocketBuffer as TcpSocketBuffer;
                        let tcp_rx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
                        let tcp_tx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
                        let mut new_listener = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
                        let _ = new_listener.listen(self.port);
                        ctx.tcp_handle = ctx.sockets.add(new_listener);

                        // 3. Add the established socket back under a new handle and wrap it
                        let new_handle = ctx.sockets.add(established_socket);
                        return Poll::Ready(Ok((TcpStream::new(kernel_common::net::async_tcp::AsyncTcpStream::new(new_handle)), addr)));
                    }
                    _ => {
                        socket.register_recv_waker(cx.waker());
                        Poll::Pending
                    }
                }
            } else {
                Poll::Ready(Err(anyhow::anyhow!("No network context")))
            }
        }).await
    }
}
