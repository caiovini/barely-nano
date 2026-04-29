extern crate alloc;

use alloc::vec::Vec;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use virtio_drivers::device::net::VirtIONet;
use virtio_drivers::transport::mmio::MmioTransport;
use crate::net::virtio_hal::VirtioHal;

pub struct VirtioNetDevice<'a> {
    pub driver: VirtIONet<VirtioHal, MmioTransport<'a>, 32>,
}

impl<'a> VirtioNetDevice<'a> {
    pub fn new(driver: VirtIONet<VirtioHal, MmioTransport<'a>, 32>) -> Self {
        Self { driver }
    }
}

impl<'a> Device for VirtioNetDevice<'a> {
    type RxToken<'b> = VirtioRxToken where Self: 'b;
    type TxToken<'b> = VirtioTxToken<'b, 'a> where Self: 'b;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.driver.receive() {
            Ok(buf) => {
                let rx_token = VirtioRxToken { buf: buf.packet().to_vec() };
                let _ = self.driver.recycle_rx_buffer(buf);
                let tx_token = VirtioTxToken { driver: &mut self.driver };
                Some((rx_token, tx_token))
            }
            Err(_) => None,
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken { driver: &mut self.driver })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct VirtioRxToken {
    buf: Vec<u8>,
}

impl RxToken for VirtioRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buf)
    }
}

pub struct VirtioTxToken<'b, 'a> {
    driver: &'b mut VirtIONet<VirtioHal, MmioTransport<'a>, 32>,
}

impl<'b, 'a> TxToken for VirtioTxToken<'b, 'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut tx_buf = self.driver.new_tx_buffer(len);
        let result = f(tx_buf.packet_mut());
        let _ = self.driver.send(tx_buf);
        result
    }
}
