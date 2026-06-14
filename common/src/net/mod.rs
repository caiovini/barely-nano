pub mod virtio_hal;
pub mod device;
pub mod async_tcp;
pub use async_tcp::AsyncTcpStream;

use crate::net::device::UbootNetDevice;
use crate::uart;
use crate::uboot;
use spin::Mutex;

pub static NET: Mutex<Option<NetworkContext<'static>>> = Mutex::new(None);

const VIRTIO0: usize = 0x10001000;
const VIRTIO_STEP: usize = 0x1000;
const VIRTIO_MAX: usize = 0x10008000;

extern crate alloc;
use alloc::vec;
use smoltcp::iface::{Config, Interface, SocketSet, PollResult};
use smoltcp::phy::Device;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use smoltcp::socket::AnySocket;
use smoltcp::socket::icmp::{Socket as IcmpSocket, PacketBuffer as IcmpPacketBuffer};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};
use smoltcp::socket::dns::{Socket as DnsSocket};
use smoltcp::time::Instant;

pub enum NetDevice<'a> {
    Virtio(device::VirtioNetDevice<'a>),
    Uboot(device::UbootNetDevice),
}

impl<'a> smoltcp::phy::Device for NetDevice<'a> {
    type RxToken<'b> = NetRxToken<'b> where Self: 'b;
    type TxToken<'b> = NetTxToken<'b> where Self: 'b;

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self {
            NetDevice::Virtio(d) => {
                let (rx, tx) = d.receive(timestamp)?;
                let tx_static = unsafe { core::mem::transmute(tx) };
                Some((NetRxToken::Virtio(rx), NetTxToken::Virtio(tx_static)))
            }
            NetDevice::Uboot(d) => {
                let (rx, tx) = d.receive(timestamp)?;
                Some((NetRxToken::Uboot(rx), NetTxToken::Uboot(tx)))
            }
        }
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        match self {
            NetDevice::Virtio(d) => {
                let tx = d.transmit(timestamp)?;
                let tx_static = unsafe { core::mem::transmute(tx) };
                Some(NetTxToken::Virtio(tx_static))
            },
            NetDevice::Uboot(d) => Some(NetTxToken::Uboot(d.transmit(timestamp)?)),
        }
    }

    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        match self {
            NetDevice::Virtio(d) => d.capabilities(),
            NetDevice::Uboot(d) => d.capabilities(),
        }
    }
}

pub enum NetRxToken<'a> {
    Virtio(device::VirtioRxToken),
    Uboot(device::UbootRxToken<'a>),
}

impl<'a> smoltcp::phy::RxToken for NetRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        match self {
            NetRxToken::Virtio(t) => t.consume(f),
            NetRxToken::Uboot(t) => t.consume(f),
        }
    }
}

pub enum NetTxToken<'b> {
    Virtio(device::VirtioTxToken<'b, 'static, virtio_drivers::transport::mmio::MmioTransport<'static>>),
    Uboot(device::UbootTxToken<'b>),
}

impl<'b> smoltcp::phy::TxToken for NetTxToken<'b> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            NetTxToken::Virtio(t) => t.consume(len, f),
            NetTxToken::Uboot(t) => t.consume(len, f),
        }
    }
}

pub struct NetworkContext<'a> {
    pub iface: Interface,
    pub device: NetDevice<'a>,
    pub sockets: SocketSet<'a>,
    pub tcp_handle: smoltcp::iface::SocketHandle,
    pub icmp_handle: smoltcp::iface::SocketHandle,
    pub dns_handle: smoltcp::iface::SocketHandle,
    pub peering_handle: smoltcp::iface::SocketHandle,
    pub udp_handle: smoltcp::iface::SocketHandle,
    pub rpc_handle: smoltcp::iface::SocketHandle,
    pub sockets_to_drop: alloc::vec::Vec<smoltcp::iface::SocketHandle>,
}

impl<'a> NetworkContext<'a> {
    pub fn accept_peering(&mut self) -> Option<smoltcp::iface::SocketHandle> {
        use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer, State as TcpState};
        let is_established = {
            let socket = self.sockets.get::<TcpSocket>(self.peering_handle);
            socket.is_active() && socket.state() == TcpState::Established
        };

        if is_established {
            let established_socket = self.sockets.remove(self.peering_handle);
            
            let tcp_rx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
            let tcp_tx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
            let mut new_listener = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
            let _ = new_listener.listen(7075);
            self.peering_handle = self.sockets.add(new_listener);
            
            Some(self.sockets.add(established_socket))
        } else {
            None
        }
    }

    pub fn poll(&mut self, mtime: u64) -> bool {
        let timestamp = Instant::from_micros((mtime as u128 * 1_000_000 / crate::clint::FREQ as u128) as i64);
        
        let mut progress = false;
        loop {
            match self.iface.poll(timestamp, &mut self.device, &mut self.sockets) {
                PollResult::None => break,
                _ => {
                    progress = true;
                }
            }
        }

        let mut i = 0;
        while i < self.sockets_to_drop.len() {
            let handle = self.sockets_to_drop[i];
            let is_closed = {
                let socket = self.sockets.get::<smoltcp::socket::tcp::Socket>(handle);
                socket.state() == smoltcp::socket::tcp::State::Closed
            };
            if is_closed {
                self.sockets.remove(handle);
                self.sockets_to_drop.swap_remove(i);
            } else {
                i += 1;
            }
        }
        
        progress
    }

    pub fn ping(&mut self, target: Ipv4Address) {
        use smoltcp::socket::icmp::Socket as IcmpSocket;
        use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr};
        
        let socket = self.sockets.get_mut::<IcmpSocket>(self.icmp_handle);
        if socket.can_send() {
            let mut buffer = [0u8; 8];
            let icmp_repr = Icmpv4Repr::EchoRequest {
                ident: 0x1234,
                seq_no: 1,
                data: &[],
            };
            let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut buffer);
            icmp_repr.emit(&mut icmp_packet, &self.device.capabilities().checksum);
            
            let _ = socket.send_slice(&buffer, target.into());
        }
    }

    pub fn create_tcp_socket(&mut self) -> smoltcp::iface::SocketHandle {
        let tcp_rx_buffer = TcpSocketBuffer::new(alloc::vec![0; 65535]);
        let tcp_tx_buffer = TcpSocketBuffer::new(alloc::vec![0; 65535]);
        let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        self.sockets.add(tcp_socket)
    }

    pub fn remove_tcp_socket(&mut self, handle: smoltcp::iface::SocketHandle) {
        self.sockets.remove(handle);
    }

    pub fn create_udp_socket(&mut self) -> smoltcp::iface::SocketHandle {
        use smoltcp::socket::udp::{Socket as UdpSocket, PacketBuffer as UdpPacketBuffer, PacketMetadata as UdpPacketMetadata};
        let udp_rx_buffer = UdpPacketBuffer::new(vec![UdpPacketMetadata::EMPTY; 4], vec![0; 1024]);
        let udp_tx_buffer = UdpPacketBuffer::new(vec![UdpPacketMetadata::EMPTY; 4], vec![0; 1024]);
        let udp_socket = UdpSocket::new(udp_rx_buffer, udp_tx_buffer);
        self.sockets.add(udp_socket)
    }
}

pub async fn resolve(name: &str) -> Result<alloc::vec::Vec<IpAddress>, ()> {
    use core::future::poll_fn;
    use core::task::Poll;

    let handle = {
        if let Some(ref mut ctx) = *NET.lock() {
            let dns_socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
            dns_socket.start_query(ctx.iface.context(), name, smoltcp::wire::DnsQueryType::A)
                .map_err(|_| ())?
        } else {
            return Err(());
        }
    };

    struct QueryGuard {
        handle: smoltcp::socket::dns::QueryHandle,
        done: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
    }

    impl Drop for QueryGuard {
        fn drop(&mut self) {
            if !self.done.load(core::sync::atomic::Ordering::SeqCst) {
                if let Some(ref mut ctx) = *NET.lock() {
                    let dns_socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
                    dns_socket.cancel_query(self.handle);
                }
            }
        }
    }

    let done = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
    let _guard = QueryGuard { handle, done: done.clone() };
    let start = crate::clint::mtime();
    let timeout_ticks = 5 * crate::clint::FREQ;

    poll_fn(move |cx| {
        let mtime = crate::clint::mtime();
        if mtime - start > timeout_ticks {
            return Poll::Ready(Err(()));
        }

        if let Some(ref mut ctx) = *NET.lock() {
            let dns_socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
            match dns_socket.get_query_result(handle) {
                Err(smoltcp::socket::dns::GetQueryResultError::Pending) => {
                    dns_socket.register_query_waker(handle, cx.waker());
                    // Wake ourselves up frequently to check the timeout
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Ok(addrs) => {
                    done.store(true, core::sync::atomic::Ordering::SeqCst);
                    Poll::Ready(Ok(addrs.to_vec()))
                }
                Err(_) => {
                    done.store(true, core::sync::atomic::Ordering::SeqCst);
                    Poll::Ready(Err(()))
                }
            }
        } else {
            done.store(true, core::sync::atomic::Ordering::SeqCst);
            Poll::Ready(Err(()))
        }
    }).await
}

pub fn initialize_net() {
    if let Some(ctx) = init_uboot_net() {
        *NET.lock() = Some(ctx);
    }
}

pub fn init_uboot_net<'a>() -> Option<NetworkContext<'a>> {
    uart::print(b"[net] Scanning U-Boot devices for Network Interface...\n");
    
    let mut net_info = None;
    let mut last_info = None;
    let mut max_devices = 16;
    while max_devices > 0 {
        if let Some(info) = uboot::dev_enum(last_info.as_ref()) {
            if info.type_ == uboot::DEV_TYP_NET {
                net_info = Some(info);
                break;
            }
            last_info = Some(info);
        } else {
            break;
        }
        max_devices -= 1;
    }

    if let Some(mut info) = net_info {
        uart::print(b"[net] Found U-Boot Network Device!\n");
        let _ = uboot::dev_close(&mut info);
        let res = uboot::dev_open(&mut info);
        if res != 0 {
            return None;
        }

        let mut device = NetDevice::Uboot(UbootNetDevice::new(info));
        
        // Hardcode the real MAC found in the log: c0:74:2b:f9:15:ec
        let mut mac_bytes = [0xc0, 0x74, 0x2b, 0xf9, 0x15, 0xec];
        let mac_found = true;
        
        crate::uart::print(b"[net] Using HARDCODED real MAC: c0:74:2b:f9:15:ec\n");

        if !mac_found {
            crate::uart::print(b"[net] Using fallback randomized MAC\n");
            mac_bytes = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00];
            let mut seed = crate::clint::mtime();
            for i in 1..6 {
                seed = seed.wrapping_mul(0x6364136229384751).wrapping_add(1);
                mac_bytes[i] = (seed % 256) as u8;
            }
        }

        let mtime = crate::clint::mtime();
        crate::uart::print(b"[net] MAC: ");
        for i in 0..6 {
            crate::uart::print_u8_hex(mac_bytes[i]);
            if i < 5 { crate::uart::print(b":"); }
        }
        crate::uart::print(b"\n");
        let hardware_addr = HardwareAddress::Ethernet(EthernetAddress(mac_bytes));
        let mut config = Config::new(hardware_addr);
        
        // Better randomness for ISNs
        let mut seed = mtime;
        for _ in 0..16 {
            seed = seed.wrapping_mul(0x6364136229384751).wrapping_add(1);
        }
        config.random_seed = seed;

        // Initialize ephemeral port counter with randomness
        let start_port = (seed % 16384) as u16 + 49152;
        crate::net::async_tcp::set_starting_port(start_port);

        let start_ts = Instant::from_micros((mtime / (crate::clint::FREQ / 1_000_000)) as i64);
        let mut iface = Interface::new(config, &mut device, start_ts);
        
        let static_ip = Ipv4Address::new(192, 168, 0, 200);
        iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(IpCidr::new(IpAddress::Ipv4(static_ip), 24)).unwrap();
        });
        iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(192, 168, 0, 1)).unwrap();
        
        crate::uart::print(b"[net] IP: 192.168.0.200\n");

        let mut sockets = SocketSet::new(vec![]);
        let icmp_rx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
        let icmp_tx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
        let icmp_socket = IcmpSocket::new(icmp_rx_buffer, icmp_tx_buffer);
        let icmp_handle = sockets.add(icmp_socket);

        let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
        let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
        let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        let tcp_handle = sockets.add(tcp_socket);

        let tcp_rx_buffer_2 = TcpSocketBuffer::new(vec![0; 65535]);
        let tcp_tx_buffer_2 = TcpSocketBuffer::new(vec![0; 65535]);
        let mut tcp_peering = TcpSocket::new(tcp_rx_buffer_2, tcp_tx_buffer_2);
        let _ = tcp_peering.listen(7075);
        let peering_handle = sockets.add(tcp_peering);

        let dns_servers = &[smoltcp::wire::Ipv4Address::new(1, 1, 1, 1).into()];
        let dns_socket = DnsSocket::new(dns_servers, vec![]);
        let dns_handle = sockets.add(dns_socket);

        let udp_rx_buffer = smoltcp::socket::udp::PacketBuffer::new(vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4], vec![0; 1024]);
        let udp_tx_buffer = smoltcp::socket::udp::PacketBuffer::new(vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4], vec![0; 1024]);
        let udp_socket = smoltcp::socket::udp::Socket::new(udp_rx_buffer, udp_tx_buffer);
        let udp_handle = sockets.add(udp_socket);

        let tcp_rx_buffer_3 = TcpSocketBuffer::new(vec![0; 65535]);
        let tcp_tx_buffer_3 = TcpSocketBuffer::new(vec![0; 65535]);
        let mut tcp_rpc = TcpSocket::new(tcp_rx_buffer_3, tcp_tx_buffer_3);
        let _ = tcp_rpc.listen(7076);
        let rpc_handle = sockets.add(tcp_rpc);

        return Some(NetworkContext {
            iface,
            device,
            sockets,
            tcp_handle,
            icmp_handle,
            dns_handle,
            peering_handle,
            udp_handle,
            rpc_handle,
            sockets_to_drop: alloc::vec::Vec::new(),
        });
    }

    None
}
