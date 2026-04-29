use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::Socket as TcpSocket;
use smoltcp::wire::IpEndpoint;
use core::future::poll_fn;
use core::task::Poll;
use embedded_io_async::{ErrorType, Error, Read, Write};
use crate::net::NET;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpError {
    SmoltcpError,
}

impl Error for TcpError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

pub struct AsyncTcpStream {
    handle: SocketHandle,
}

impl AsyncTcpStream {
    pub fn new(handle: SocketHandle) -> Self {
        Self { handle }
    }

    pub fn handle(&self) -> SocketHandle {
        self.handle
    }

    pub async fn connect(handle: SocketHandle, endpoint: impl Into<IpEndpoint>) -> Result<Self, TcpError> {
        let endpoint = endpoint.into();
        let port = endpoint.port;
        
        let mut success = false;
        if let Some(ref mut ctx) = *NET.lock() {
            let socket = ctx.sockets.get_mut::<TcpSocket>(handle);
            static NEXT_PORT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(49152);
            let local_port = NEXT_PORT.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
            if socket.connect(ctx.iface.context(), endpoint, local_port).is_ok() {
                success = true;
            }
        }

        if !success {
            return Err(TcpError::SmoltcpError);
        }

        poll_fn(|cx| {
            if let Some(ref mut ctx) = *NET.lock() {
                let socket = ctx.sockets.get_mut::<TcpSocket>(handle);
                match socket.state() {
                    smoltcp::socket::tcp::State::Established => Poll::Ready(Ok(())),
                    smoltcp::socket::tcp::State::Closed |
                    smoltcp::socket::tcp::State::Closing |
                    smoltcp::socket::tcp::State::CloseWait |
                    smoltcp::socket::tcp::State::TimeWait => Poll::Ready(Err(TcpError::SmoltcpError)),
                    _ => {
                        socket.register_send_waker(cx.waker());
                        Poll::Pending
                    }
                }
            } else {
                Poll::Ready(Err(TcpError::SmoltcpError))
            }
        }).await?;

        Ok(Self { handle })
    }
}

impl ErrorType for AsyncTcpStream {
    type Error = TcpError;
}

impl Read for AsyncTcpStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        poll_fn(|cx| {
            if let Some(ref mut ctx) = *NET.lock() {
                let socket = ctx.sockets.get_mut::<TcpSocket>(self.handle);
                if !socket.is_open() {
                    return Poll::Ready(Ok(0)); // EOF
                }
                if socket.can_recv() {
                    match socket.recv_slice(buf) {
                        Ok(len) => Poll::Ready(Ok(len)),
                        Err(_) => Poll::Ready(Err(TcpError::SmoltcpError)),
                    }
                } else {
                    socket.register_recv_waker(cx.waker());
                    Poll::Pending
                }
            } else {
                Poll::Ready(Err(TcpError::SmoltcpError))
            }
        }).await
    }
}

impl Write for AsyncTcpStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        poll_fn(|cx| {
            if let Some(ref mut ctx) = *NET.lock() {
                let socket = ctx.sockets.get_mut::<TcpSocket>(self.handle);
                if !socket.is_open() {
                    return Poll::Ready(Err(TcpError::SmoltcpError));
                }
                if socket.can_send() {
                    match socket.send_slice(buf) {
                        Ok(len) => Poll::Ready(Ok(len)),
                        Err(_) => Poll::Ready(Err(TcpError::SmoltcpError)),
                    }
                } else {
                    socket.register_send_waker(cx.waker());
                    Poll::Pending
                }
            } else {
                Poll::Ready(Err(TcpError::SmoltcpError))
            }
        }).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
