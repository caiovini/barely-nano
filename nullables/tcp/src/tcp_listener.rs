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
                                // If smoltcp 0.13 uses core::net types directly
                                SocketAddr::new(core::net::IpAddr::V4(v4.into()), remote_endpoint.port)
                            }
                            smoltcp::wire::IpAddress::Ipv6(v6) => {
                                SocketAddr::new(core::net::IpAddr::V6(v6.into()), remote_endpoint.port)
                            }
                        };
                        return Poll::Ready(Ok((TcpStream::new(kernel_common::net::async_tcp::AsyncTcpStream::new(ctx.tcp_handle)), addr)));
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
