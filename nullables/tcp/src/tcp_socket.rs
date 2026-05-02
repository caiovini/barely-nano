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
            strategy: Strategy::Real(handle),
        })
    }

    pub async fn connect(self, addr: SocketAddr) -> anyhow::Result<TcpStream> {
        match self.strategy {
            Strategy::Real(handle) => {
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
                        let ipv6 = smoltcp::wire::Ipv6Address::from_octets(v6.ip().octets());
                        if let Some(v4) = ipv6.to_ipv4() {
                            smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::Ipv4(v4), v6.port())
                        } else {
                            smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::Ipv6(ipv6), v6.port())
                        }
                    }
                };

                let stream = AsyncTcpStream::connect(handle, endpoint).await
                    .map_err(|_| anyhow::anyhow!("Connection failed"))?;
                Ok(TcpStream::new(stream))
            }
        }
    }
}

enum Strategy {
    Real(smoltcp::iface::SocketHandle),
}
