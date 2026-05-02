use core::{
    cmp::min,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll},
};
use alloc::vec::Vec;
use core::net::{SocketAddrV6, SocketAddr, Ipv6Addr, SocketAddrV4, Ipv4Addr};
use embedded_io_async::{Read, Write, ErrorType};
use kernel_common::net::async_tcp::{AsyncTcpStream, TcpError};
use kernel_common::net::NET;
use smoltcp::socket::tcp::Socket as SmolTcpSocket;
use core::future::poll_fn;

pub const TEST_ENDPOINT_1: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0, 0, 0, 1), 1111, 0, 0);

pub struct TcpStream {
    stream: StreamType,
}

impl TcpStream {
    pub fn new(stream: AsyncTcpStream) -> Self {
        Self {
            stream: StreamType::Real(stream),
        }
    }

    pub fn new_null() -> Self {
        Self {
            stream: StreamType::Stub(TcpStreamStub::new(TEST_ENDPOINT_1, Vec::new())),
        }
    }

    pub fn new_null_with_peer_addr(peer_addr: SocketAddrV6) -> Self {
        Self {
            stream: StreamType::Stub(TcpStreamStub::new(peer_addr, Vec::new())),
        }
    }

    pub fn new_null_with(incoming: Vec<u8>) -> Self {
        Self {
            stream: StreamType::Stub(TcpStreamStub::new(TEST_ENDPOINT_1, incoming)),
        }
    }

    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        match &mut self.stream {
            StreamType::Real(_stream) => {
                Ok(())
            }
            StreamType::Stub(stream) => stream.shutdown().await,
        }
    }

    pub async fn readable(&self) -> anyhow::Result<()> {
        match &self.stream {
            StreamType::Real(stream) => {
                poll_fn(|cx| {
                    if let Some(ref mut ctx) = *NET.lock() {
                        let socket = ctx.sockets.get_mut::<SmolTcpSocket>(stream.handle());
                        if socket.can_recv() {
                            Poll::Ready(Ok(()))
                        } else {
                            socket.register_recv_waker(cx.waker());
                            Poll::Pending
                        }
                    } else {
                        Poll::Ready(Err(anyhow::anyhow!("No network context")))
                    }
                }).await
            }
            StreamType::Stub(stream) => stream.readable().map_err(|e| anyhow::anyhow!(e)),
        }
    }

    pub fn try_read(&self, buf: &mut [u8]) -> anyhow::Result<usize> {
        match &self.stream {
            StreamType::Real(stream) => {
                if let Some(ref mut ctx) = *NET.lock() {
                    let socket = ctx.sockets.get_mut::<SmolTcpSocket>(stream.handle());
                    if socket.can_recv() {
                        socket.recv_slice(buf).map_err(|_| anyhow::anyhow!("Read error"))
                    } else {
                        Err(anyhow::anyhow!("Would block"))
                    }
                } else {
                    Err(anyhow::anyhow!("No network context"))
                }
            }
            StreamType::Stub(stream) => stream.try_read(buf).map_err(|e| anyhow::anyhow!(e)),
        }
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        match &self.stream {
            StreamType::Real(stream) => {
                if let Some(ref ctx) = *NET.lock() {
                    let socket = ctx.sockets.get::<SmolTcpSocket>(stream.handle());
                    let endpoint = socket.local_endpoint().ok_or_else(|| anyhow::anyhow!("No local endpoint"))?;
                    match endpoint.addr {
                        smoltcp::wire::IpAddress::Ipv4(ipv4) => {
                            let octets = ipv4.octets();
                            Ok(SocketAddr::V4(SocketAddrV4::new(
                                Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]),
                                endpoint.port
                            )))
                        }
                        smoltcp::wire::IpAddress::Ipv6(ipv6) => {
                            let octets = ipv6.octets();
                            Ok(SocketAddr::V6(SocketAddrV6::new(
                                Ipv6Addr::from(octets),
                                endpoint.port,
                                0,
                                0
                            )))
                        }
                    }
                } else {
                    Err(anyhow::anyhow!("No network context"))
                }
            }
            StreamType::Stub(stream) => Ok(stream.local_addr()),
        }
    }

    pub fn peer_addr(&self) -> anyhow::Result<SocketAddr> {
        match &self.stream {
            StreamType::Real(stream) => {
                if let Some(ref ctx) = *NET.lock() {
                    let socket = ctx.sockets.get::<SmolTcpSocket>(stream.handle());
                    let endpoint = socket.remote_endpoint().ok_or_else(|| anyhow::anyhow!("No remote endpoint"))?;
                    match endpoint.addr {
                        smoltcp::wire::IpAddress::Ipv4(ipv4) => {
                            let octets = ipv4.octets();
                            Ok(SocketAddr::V4(SocketAddrV4::new(
                                Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]),
                                endpoint.port
                            )))
                        }
                        smoltcp::wire::IpAddress::Ipv6(ipv6) => {
                            let octets = ipv6.octets();
                            Ok(SocketAddr::V6(SocketAddrV6::new(
                                Ipv6Addr::from(octets),
                                endpoint.port,
                                0,
                                0
                            )))
                        }
                    }
                } else {
                    Err(anyhow::anyhow!("No network context"))
                }
            }
            StreamType::Stub(stream) => Ok(stream.peer_addr()),
        }
    }

    pub async fn writable(&self) -> anyhow::Result<()> {
        match &self.stream {
            StreamType::Real(stream) => {
                poll_fn(|cx| {
                    if let Some(ref mut ctx) = *NET.lock() {
                        let socket = ctx.sockets.get_mut::<SmolTcpSocket>(stream.handle());
                        if socket.can_send() {
                            Poll::Ready(Ok(()))
                        } else {
                            socket.register_send_waker(cx.waker());
                            Poll::Pending
                        }
                    } else {
                        Poll::Ready(Err(anyhow::anyhow!("No network context")))
                    }
                }).await
            }
            StreamType::Stub(stream) => stream.writable().await.map_err(|e| anyhow::anyhow!(e)),
        }
    }

    pub fn try_write(&self, buf: &[u8]) -> anyhow::Result<usize> {
        match &self.stream {
            StreamType::Real(stream) => {
                if let Some(ref mut ctx) = *NET.lock() {
                    let socket = ctx.sockets.get_mut::<SmolTcpSocket>(stream.handle());
                    if socket.can_send() {
                        socket.send_slice(buf).map_err(|_| anyhow::anyhow!("Write error"))
                    } else {
                        Err(anyhow::anyhow!("Would block"))
                    }
                } else {
                    Err(anyhow::anyhow!("No network context"))
                }
            }
            StreamType::Stub(stream) => stream.try_write(buf).map_err(|e| anyhow::anyhow!(e)),
        }
    }
}

enum StreamType {
    Real(AsyncTcpStream),
    Stub(TcpStreamStub),
}

struct TcpStreamStub {
    incoming: Vec<u8>,
    position: AtomicUsize,
    peer_addr: SocketAddrV6,
}

impl TcpStreamStub {
    pub fn new(peer_addr: SocketAddrV6, incoming: Vec<u8>) -> Self {
        Self {
            incoming,
            position: AtomicUsize::new(0),
            peer_addr,
        }
    }

    fn no_data_error() -> &'static str {
        "nulled tcp stream has no data"
    }

    fn next_bytes(&self) -> &[u8] {
        let pos = self.position.load(Ordering::SeqCst);
        &self.incoming[pos..]
    }

    fn readable(&self) -> Result<(), &'static str> {
        if self.next_bytes().is_empty() {
            Err(Self::no_data_error())
        } else {
            Ok(())
        }
    }

    fn try_read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let next_bytes = self.next_bytes();
        if next_bytes.is_empty() {
            Err(Self::no_data_error())
        } else {
            let read_count = min(buf.len(), next_bytes.len());
            buf[..read_count].copy_from_slice(&next_bytes[..read_count]);
            self.position.fetch_add(read_count, Ordering::SeqCst);
            Ok(read_count)
        }
    }

    fn local_addr(&self) -> SocketAddr {
        SocketAddr::V6(TEST_ENDPOINT_1)
    }

    fn peer_addr(&self) -> SocketAddr {
        SocketAddr::V6(self.peer_addr)
    }

    async fn writable(&self) -> Result<(), &'static str> {
        Ok(())
    }

    fn try_write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        Ok(buf.len())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ErrorType for TcpStream {
    type Error = TcpError;
}

impl Read for TcpStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match &mut self.stream {
            StreamType::Real(s) => s.read(buf).await,
            StreamType::Stub(s) => s.try_read(buf).map_err(|_| TcpError::SmoltcpError),
        }
    }
}

impl Write for TcpStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match &mut self.stream {
            StreamType::Real(s) => s.write(buf).await,
            StreamType::Stub(s) => s.try_write(buf).map_err(|_| TcpError::SmoltcpError),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match &mut self.stream {
            StreamType::Real(s) => s.flush().await,
            StreamType::Stub(_) => Ok(()),
        }
    }
}
