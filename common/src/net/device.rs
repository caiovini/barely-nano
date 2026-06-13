use smoltcp::phy::{Device, DeviceCapabilities, RxToken, TxToken};
use smoltcp::time::Instant;
use crate::uboot;
use crate::uart;
use core::marker::PhantomData;

pub struct UbootNetDevice {
    info: uboot::DeviceInfo,
    rx_buffer: [u8; 2048],
    tx_buffer: TxBuffer,
}

struct TxBuffer {
    data: [u8; 2048],
}

impl UbootNetDevice {
    pub fn new(info: uboot::DeviceInfo) -> Self {
        Self {
            info,
            rx_buffer: [0u8; 2048],
            tx_buffer: TxBuffer { data: [0u8; 2048] },
        }
    }
}

impl Device for UbootNetDevice {
    type RxToken<'a> = UbootRxToken<'a> where Self: 'a;
    type TxToken<'a> = UbootTxToken<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let buffer = &mut self.rx_buffer[..];
        let bytes_read = uboot::eth_receive(&self.info, buffer);
        
        if bytes_read > 0 {
            let buffer = &buffer[..bytes_read as usize];
            
            let rx = UbootRxToken { buf: buffer };
            let tx = UbootTxToken { 
                info: &self.info,
                tx_buffer: &mut self.tx_buffer.data,
            };
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(UbootTxToken { 
            info: &self.info,
            tx_buffer: &mut self.tx_buffer.data,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps.medium = smoltcp::phy::Medium::Ethernet;
        caps.checksum.ipv4 = smoltcp::phy::Checksum::Tx;
        caps.checksum.tcp = smoltcp::phy::Checksum::Tx;
        caps.checksum.udp = smoltcp::phy::Checksum::Tx;
        caps.checksum.icmpv4 = smoltcp::phy::Checksum::Tx;
        caps
    }
}

pub struct UbootRxToken<'a> {
    buf: &'a [u8],
}

impl<'a> RxToken for UbootRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.buf)
    }
}

pub struct UbootTxToken<'a> {
    info: &'a uboot::DeviceInfo,
    tx_buffer: &'a mut [u8],
}

impl<'a> TxToken for UbootTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let res = f(&mut self.tx_buffer[..len]);
        
        let transmit_len = core::cmp::max(len, 60);
        if transmit_len > len {
            for i in len..transmit_len {
                self.tx_buffer[i] = 0;
            }
        }
        let _ = uboot::eth_send(&self.info, &self.tx_buffer[..transmit_len]);
        res
    }
}

pub struct VirtioNetDevice<'a> {
    pub device: virtio_drivers::device::net::VirtIONet<crate::net::virtio_hal::VirtioHal, virtio_drivers::transport::mmio::MmioTransport<'static>, 128>,
    _phantom: PhantomData<&'a ()>,
}

impl<'a> VirtioNetDevice<'a> {
    pub fn new(device: virtio_drivers::device::net::VirtIONet<crate::net::virtio_hal::VirtioHal, virtio_drivers::transport::mmio::MmioTransport<'static>, 128>) -> Self {
        Self {
            device,
            _phantom: PhantomData,
        }
    }

    pub fn receive<'b>(&'b mut self, _timestamp: Instant) -> Option<(VirtioRxToken, VirtioTxToken<'b, 'a, virtio_drivers::transport::mmio::MmioTransport<'static>>)> {
        if let Ok(rx_buf) = self.device.receive() {
            uart::putchar(b'.');
            Some((VirtioRxToken { buf: rx_buf }, VirtioTxToken { device: self, _phantom: PhantomData }))
        } else {
            None
        }
    }

    pub fn transmit<'b>(&'b mut self, _timestamp: Instant) -> Option<VirtioTxToken<'b, 'a, virtio_drivers::transport::mmio::MmioTransport<'static>>> {
        Some(VirtioTxToken { device: self, _phantom: PhantomData })
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps.medium = smoltcp::phy::Medium::Ethernet;
        caps
    }
}

pub struct VirtioRxToken {
    buf: virtio_drivers::device::net::RxBuffer,
}

impl RxToken for VirtioRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.buf.packet())
    }
}

pub struct VirtioTxToken<'a, 'b, T: virtio_drivers::transport::Transport> {
    device: &'a mut VirtioNetDevice<'b>,
    _phantom: PhantomData<T>,
}

impl<'a, 'b, T: virtio_drivers::transport::Transport> TxToken for VirtioTxToken<'a, 'b, T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut tx_buf = self.device.device.new_tx_buffer(len);
        let res = f(tx_buf.packet_mut());
        self.device.device.send(tx_buf).expect("Failed to send VirtIO packet");
        res
    }
}
