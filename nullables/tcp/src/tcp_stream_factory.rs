use super::tcp_stream::TcpStream;
use crate::TcpSocket;
use core::net::SocketAddr;

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
            FactoryType::Real => TcpSocket::new_v6()?.connect(addr).await,
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
