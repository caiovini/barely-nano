use embedded_nal_async::{TcpConnect, SocketAddr};
use kernel_common::net::async_tcp::{AsyncTcpStream, TcpError};
use smoltcp::wire::IpAddress;

pub struct AsyncTcpClient;

impl TcpConnect for AsyncTcpClient {
    type Error = TcpError;
    type Connection<'m> = AsyncTcpStream where Self: 'm;

    async fn connect<'m>(
        &'m self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'m>, Self::Error> {
        // Find an unused handle (this is simplified, ideally we manage a pool of tcp handles)
        // For now, let's just grab the predefined tcp_handle in NET context.
        let handle = {
            if let Some(ref ctx) = *kernel_common::net::NET.lock() {
                ctx.tcp_handle
            } else {
                return Err(TcpError::SmoltcpError);
            }
        };

        let endpoint = match remote {
            SocketAddr::V4(v4) => {
                smoltcp::wire::IpEndpoint::new(
                    IpAddress::Ipv4(smoltcp::wire::Ipv4Address::new(
                        v4.ip().octets()[0],
                        v4.ip().octets()[1],
                        v4.ip().octets()[2],
                        v4.ip().octets()[3],
                    )),
                    v4.port()
                )
            },
            SocketAddr::V6(_) => return Err(TcpError::SmoltcpError),
        };

        AsyncTcpStream::connect(handle, endpoint).await
    }
}
