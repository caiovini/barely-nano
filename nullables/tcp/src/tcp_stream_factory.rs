use super::tcp_stream::TcpStream;
use core::net::SocketAddr;
use kernel_common::net::async_tcp::AsyncTcpStream;
use kernel_common::net::NET;

pub struct TcpStreamFactory {
    factory: FactoryType,
}

impl TcpStreamFactory {
    pub fn new() -> Self {
        Self {
            factory: FactoryType::Real,
        }
    }

    pub fn new_null() -> Self {
        Self {
            factory: FactoryType::Null,
        }
    }

    pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<TcpStream> {
        match &self.factory {
            FactoryType::Real => {
                let handle = {
                    if let Some(ref ctx) = *NET.lock() {
                        ctx.tcp_handle
                    } else {
                        return Err(anyhow::anyhow!("No network context"));
                    }
                };

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
                        smoltcp::wire::IpEndpoint::new(
                            smoltcp::wire::IpAddress::Ipv6(smoltcp::wire::Ipv6Address::from_octets(
                                v6.ip().octets()
                            )),
                            v6.port()
                        )
                    }
                };

                let stream = AsyncTcpStream::connect(handle, endpoint).await
                    .map_err(|_| anyhow::anyhow!("Connection failed"))?;
                Ok(TcpStream::new(stream))
            }
            FactoryType::Null => Err(anyhow::anyhow!("nulled TcpStreamFactory has no configured connections")),
        }
    }
}

impl Default for TcpStreamFactory {
    fn default() -> Self {
        Self::new()
    }
}

enum FactoryType {
    Real,
    Null,
}
