#![no_std]
extern crate alloc;

mod tcp_socket;
mod tcp_stream;
mod tcp_stream_factory;
mod tcp_listener;

use core::sync::atomic::{AtomicU16, Ordering};
pub use tcp_socket::*;
pub use tcp_stream::TcpStream;
pub use tcp_stream_factory::TcpStreamFactory;
pub use tcp_listener::TcpListener;

static START_PORT: AtomicU16 = AtomicU16::new(1025);

pub fn get_available_port() -> u16 {
    START_PORT.fetch_add(1, Ordering::SeqCst)
}
