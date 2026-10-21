use smoltcp::phy::{Device, DeviceCapabilities, RxToken, TxToken};
use smoltcp::time::Instant;
use crate::uboot;

pub const RX_RING_SIZE: usize = 512;
pub const PACKET_BUF_SIZE: usize = 1536;

pub struct UbootNetDevice {
    info: uboot::DeviceInfo,
    rx_ring: alloc::boxed::Box<[[u8; PACKET_BUF_SIZE]]>,
    rx_lens: [u16; RX_RING_SIZE],
    rx_head: usize,
    rx_tail: usize,
    rx_count: usize,
    tx_buffer: TxBuffer,
}

struct TxBuffer {
    data: [u8; 2048],
}

impl UbootNetDevice {
    pub fn new(info: uboot::DeviceInfo) -> Self {
        Self {
            info,
            rx_ring: alloc::vec![[0u8; PACKET_BUF_SIZE]; RX_RING_SIZE].into_boxed_slice(),
            rx_lens: [0u16; RX_RING_SIZE],
            rx_head: 0,
            rx_tail: 0,
            rx_count: 0,
            tx_buffer: TxBuffer { data: [0u8; 2048] },
        }
    }
}

impl Device for UbootNetDevice {
    type RxToken<'a> = UbootRxToken<'a> where Self: 'a;
    type TxToken<'a> = UbootTxToken<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // When ring buffer is empty, refill by batch-draining from U-Boot's hardware DMA ring
        if self.rx_count == 0 {
            let free_slots = RX_RING_SIZE - self.rx_count;
            let received = uboot::eth_receive_batch(
                &self.info,
                &mut self.rx_ring,
                &mut self.rx_lens,
                self.rx_tail,
                free_slots,
            );
            if received > 0 {
                self.rx_tail = (self.rx_tail + received) % RX_RING_SIZE;
                self.rx_count += received;
            }
        }

        if self.rx_count > 0 {
            let head = self.rx_head;
            let len = self.rx_lens[head] as usize;
            self.rx_head = (self.rx_head + 1) % RX_RING_SIZE;
            self.rx_count -= 1;

            let rx = UbootRxToken {
                buf: &self.rx_ring[head][..len],
            };
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
        caps.max_burst_size = Some(RX_RING_SIZE);
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
