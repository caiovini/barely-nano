pub mod virtio_hal;
pub mod device;
pub mod async_tcp;

use core::ptr::NonNull;
use virtio_drivers::device::net::VirtIONet;
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::transport::{DeviceType, Transport};
use crate::net::virtio_hal::VirtioHal;
use crate::net::device::VirtioNetDevice;
use crate::uart;
use spin::Mutex;

pub static NET: Mutex<Option<NetworkContext<'static>>> = Mutex::new(None);

const VIRTIO0: usize = 0x10001000;
const VIRTIO_STEP: usize = 0x1000;
const VIRTIO_MAX: usize = 0x10008000;

extern crate alloc;
use alloc::vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};
use smoltcp::socket::icmp::{Socket as IcmpSocket, PacketBuffer as IcmpPacketBuffer};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};
use smoltcp::socket::dns::{Socket as DnsSocket};
use smoltcp::time::Instant;

pub struct NetworkContext<'a> {
    pub iface: Interface,
    pub device: VirtioNetDevice<'a>,
    pub sockets: SocketSet<'a>,
    pub tcp_handle: smoltcp::iface::SocketHandle,
    pub dns_handle: smoltcp::iface::SocketHandle,
    pub peering_handle: smoltcp::iface::SocketHandle,
    pub udp_handle: smoltcp::iface::SocketHandle,
    pub rpc_handle: smoltcp::iface::SocketHandle,
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
        let timestamp = Instant::from_micros((mtime / 10) as i64);
        let res = self.iface.poll(timestamp, &mut self.device, &mut self.sockets);
        res != smoltcp::iface::PollResult::None
    }

    pub fn create_tcp_socket(&mut self) -> smoltcp::iface::SocketHandle {
        let tcp_rx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
        let tcp_tx_buffer = TcpSocketBuffer::new(alloc::vec![0; 32768]);
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

    poll_fn(|cx| {
        if let Some(ref mut ctx) = *NET.lock() {
            let dns_socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
            match dns_socket.get_query_result(handle) {
                Err(smoltcp::socket::dns::GetQueryResultError::Pending) => {
                    dns_socket.register_query_waker(handle, cx.waker());
                    Poll::Pending
                }
                Ok(addrs) => {
                    Poll::Ready(Ok(addrs.to_vec()))
                }
                Err(_) => {
                    Poll::Ready(Err(()))
                }
            }
        } else {
            Poll::Ready(Err(()))
        }
    }).await
}


pub fn init_virtio_net<'a>() -> Option<NetworkContext<'a>> {
    for addr in (VIRTIO0..=VIRTIO_MAX).step_by(VIRTIO_STEP) {
        let header = NonNull::new(addr as *mut VirtIOHeader).unwrap();
        match unsafe { MmioTransport::new(header, VIRTIO_STEP) } {
            Ok(transport) => {
                if transport.device_type() == DeviceType::Network {
                    /*
                    uart::print(b"[net] Found VirtIO Network device at ");
                    uart::print_usize(addr);
                    uart::print(b"\n");
                    */

                    match VirtIONet::<VirtioHal, MmioTransport, 128>::new(transport, 2048) {
                        Ok(driver) => {
                            let mac = driver.mac_address();
                            /*
                            uart::print(b"[net] MAC address: ");
                            for (i, &b) in mac.iter().enumerate() {
                                uart::print_usize(b as usize);
                                if i < 5 { uart::print(b":"); }
                            }
                            uart::print(b"\n");
                            */

                            let mut device = VirtioNetDevice::new(driver);
                            let hardware_addr = HardwareAddress::Ethernet(EthernetAddress([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]));
                            let mut config = Config::new(hardware_addr);
                            config.random_seed = 0x12345678;

                            let mtime = crate::clint::mtime();
                            let start_ts = Instant::from_micros((mtime / 10) as i64);
                            let mut iface = Interface::new(config, &mut device, start_ts);
                            iface.update_ip_addrs(|ip_addrs| {
                                ip_addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 15)), 24)).unwrap();
                                ip_addrs.push(IpCidr::new(IpAddress::Ipv6(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 64)).unwrap();
                            });
                            iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2)).unwrap();

                            let mut sockets = SocketSet::new(vec![]);
                            let icmp_rx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
                            let icmp_tx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
                            let icmp_socket = IcmpSocket::new(icmp_rx_buffer, icmp_tx_buffer);
                            sockets.add(icmp_socket);

                            let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; 32768]);
                            let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; 32768]);
                            let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
                            let tcp_handle = sockets.add(tcp_socket);

                            let tcp_rx_buffer_2 = TcpSocketBuffer::new(vec![0; 32768]);
                            let tcp_tx_buffer_2 = TcpSocketBuffer::new(vec![0; 32768]);
                            let mut tcp_peering = TcpSocket::new(tcp_rx_buffer_2, tcp_tx_buffer_2);
                            let _ = tcp_peering.listen(7075);
                            let peering_handle = sockets.add(tcp_peering);

                            let dns_servers = &[smoltcp::wire::Ipv4Address::new(10, 0, 2, 3).into()];
                            let dns_socket = DnsSocket::new(dns_servers, vec![]);
                            let dns_handle = sockets.add(dns_socket);

                            let udp_rx_buffer = smoltcp::socket::udp::PacketBuffer::new(vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4], vec![0; 1024]);
                            let udp_tx_buffer = smoltcp::socket::udp::PacketBuffer::new(vec![smoltcp::socket::udp::PacketMetadata::EMPTY; 4], vec![0; 1024]);
                            let udp_socket = smoltcp::socket::udp::Socket::new(udp_rx_buffer, udp_tx_buffer);
                            let udp_handle = sockets.add(udp_socket);

                            let tcp_rx_buffer_3 = TcpSocketBuffer::new(vec![0; 4096]);
                            let tcp_tx_buffer_3 = TcpSocketBuffer::new(vec![0; 4096]);
                            let mut tcp_rpc = TcpSocket::new(tcp_rx_buffer_3, tcp_tx_buffer_3);
                            let _ = tcp_rpc.listen(7076);
                            let rpc_handle = sockets.add(tcp_rpc);

                            return Some(NetworkContext {
                                iface,
                                device,
                                sockets,
                                tcp_handle,
                                dns_handle,
                                peering_handle,
                                udp_handle,
                                rpc_handle,
                            });
                        }
                        Err(e) => {
                            uart::print(b"[net] Failed to initialize VirtIONet: ");
                            match e {
                                virtio_drivers::Error::NotReady => uart::print(b"NotReady\n"),
                                virtio_drivers::Error::InvalidParam => uart::print(b"InvalidParam\n"),
                                virtio_drivers::Error::DmaError => uart::print(b"DmaError\n"),
                                virtio_drivers::Error::IoError => uart::print(b"IoError\n"),
                                virtio_drivers::Error::Unsupported => uart::print(b"Unsupported\n"),
                                virtio_drivers::Error::ConfigSpaceTooSmall => uart::print(b"ConfigSpaceTooSmall\n"),
                                _ => uart::print(b"Other\n"),
                            }
                        }
                    }
                }
            }
            Err(_) => {} // not a virtio device or unsupported
        }
    }
    uart::print(b"[net] No VirtIO Network device found\n");
    None
}
