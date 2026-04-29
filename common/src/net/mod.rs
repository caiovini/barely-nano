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
}

impl<'a> NetworkContext<'a> {
    pub fn poll(&mut self, mtime: u64) {
        let timestamp = Instant::from_millis((mtime / 10_000) as i64);
        self.iface.poll(timestamp, &mut self.device, &mut self.sockets);

        let socket = self.sockets.get_mut::<TcpSocket>(self.tcp_handle);
        if !socket.is_open() {
            socket.listen(80).unwrap();
        }

        if socket.can_recv() && socket.can_send() {
            if let Ok(_) = socket.recv(|buffer| {
                crate::uart::print(b"[tcp] Received ");
                crate::uart::print_usize(buffer.len());
                crate::uart::print(b" bytes\n");
                (buffer.len(), ())
            }) {
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\nHello RISC-V!\n";
                let _ = socket.send_slice(response);
                socket.close();
            }
        } else if !socket.may_recv() && socket.may_send() {
            // Client closed connection
            socket.close();
        }
    }
}

pub fn init_virtio_net<'a>() -> Option<NetworkContext<'a>> {
    for addr in (VIRTIO0..=VIRTIO_MAX).step_by(VIRTIO_STEP) {
        let header = NonNull::new(addr as *mut VirtIOHeader).unwrap();
        match unsafe { MmioTransport::new(header, VIRTIO_STEP) } {
            Ok(transport) => {
                if transport.device_type() == DeviceType::Network {
                    uart::print(b"[net] Found VirtIO Network device at ");
                    uart::print_usize(addr);
                    uart::print(b"\n");

                    match VirtIONet::<VirtioHal, MmioTransport, 32>::new(transport, 2048) {
                        Ok(driver) => {
                            let mac = driver.mac_address();
                            uart::print(b"[net] MAC address: ");
                            for (i, &b) in mac.iter().enumerate() {
                                uart::print_usize(b as usize);
                                if i < 5 { uart::print(b":"); }
                            }
                            uart::print(b"\n");

                            let mut device = VirtioNetDevice::new(driver);
                            let hardware_addr = HardwareAddress::Ethernet(EthernetAddress([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]));
                            let mut config = Config::new(hardware_addr);
                            config.random_seed = 0x12345678;

                            let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));
                            iface.update_ip_addrs(|ip_addrs| {
                                ip_addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 15)), 24)).unwrap();
                                ip_addrs.push(IpCidr::new(IpAddress::Ipv6(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 64)).unwrap();
                            });

                            let mut sockets = SocketSet::new(vec![]);
                            let icmp_rx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
                            let icmp_tx_buffer = IcmpPacketBuffer::new(vec![smoltcp::socket::icmp::PacketMetadata::EMPTY], vec![0; 256]);
                            let icmp_socket = IcmpSocket::new(icmp_rx_buffer, icmp_tx_buffer);
                            sockets.add(icmp_socket);

                            let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; 4096]);
                            let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; 4096]);
                            let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
                            let tcp_handle = sockets.add(tcp_socket);

                            let dns_servers = &[smoltcp::wire::Ipv4Address::new(8, 8, 8, 8).into()];
                            let dns_socket = DnsSocket::new(dns_servers, vec![]);
                            let dns_handle = sockets.add(dns_socket);

                            return Some(NetworkContext {
                                iface,
                                device,
                                sockets,
                                tcp_handle,
                                dns_handle,
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
