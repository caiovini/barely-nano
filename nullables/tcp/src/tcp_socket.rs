use crate::TcpStream;
use core::net::SocketAddr;
use kernel_common::net::async_tcp::AsyncTcpStream;
use kernel_common::net::NET;

pub struct TcpSocket {
    strategy: Strategy,
}

impl TcpSocket {
    pub fn new_v6() -> anyhow::Result<Self> {
        let handle = {
            if let Some(ref mut ctx) = *NET.lock() {
                ctx.create_tcp_socket()
            } else {
                return Err(anyhow::anyhow!("No network context"));
            }
        };
        Ok(Self {
            strategy: Strategy::Real(Some(handle)),
        })
    }

    pub async fn connect(mut self, addr: SocketAddr) -> anyhow::Result<TcpStream> {
        match &mut self.strategy {
            Strategy::Real(handle_opt) => {
                let handle = handle_opt.take().ok_or_else(|| anyhow::anyhow!("Socket already used or dropped"))?;
                let endpoint = match addr {
                    SocketAddr::V4(v4) => {
                        smoltcp::wire::IpEndpoint::new(
                            smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address::new(
                                v4.ip().octets()[0],
                                v4.ip().octets()[1],
                                v4.ip().octets()[2],
                                v4.ip().octets()[3],
                            )),
                            v4.port()
                        )
                    }
                    SocketAddr::V6(v6) => {
                        if let Some(v4) = v6.ip().to_ipv4() {
                            let octets = v4.octets();
                            smoltcp::wire::IpEndpoint::new(
                                smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address::new(
                                    octets[0], octets[1], octets[2], octets[3]
                                )),
                                v6.port()
                            )
                        } else {
                            let ipv6 = smoltcp::wire::Ipv6Address::from_octets(v6.ip().octets());
                            smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::Ipv6(ipv6), v6.port())
                        }
                    }
                };

                struct SocketCleanupGuard(Option<smoltcp::iface::SocketHandle>);
                impl Drop for SocketCleanupGuard {
                    fn drop(&mut self) {
                        if let Some(h) = self.0.take() {
                            if let Some(ref mut ctx) = *NET.lock() {
                                ctx.remove_tcp_socket(h);
                            }
                        }
                    }
                }
                
                let mut guard = SocketCleanupGuard(Some(handle));

                let stream = AsyncTcpStream::connect(handle, endpoint).await
                    .map_err(|_| {
                        anyhow::anyhow!("Connection failed")
                    })?;
                    
                guard.0 = None;
                Ok(TcpStream::new(stream))
            }
        }
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        match &mut self.strategy {
            Strategy::Real(Some(handle)) => {
                if let Some(ref mut ctx) = *NET.lock() {
                    ctx.remove_tcp_socket(*handle);
                }
            }
            Strategy::Real(None) => {}
        }
    }
}

enum Strategy {
    Real(Option<smoltcp::iface::SocketHandle>),
}
