use smoltcp::phy::{Device, DeviceCapabilities, RxToken, TxToken};
use smoltcp::time::Instant;
use crate::uboot;

pub const RX_RING_SIZE: usize = 2048;
pub const TX_RING_SIZE: usize = 2048;
pub const PACKET_BUF_SIZE: usize = 1536;

pub struct UbootNetDevice {
    info: uboot::DeviceInfo,
    rx_ring: alloc::boxed::Box<[[u8; PACKET_BUF_SIZE]]>,
    rx_lens: alloc::boxed::Box<[u16]>,
    rx_head: usize,
    rx_tail: usize,
    rx_count: usize,
    rx_budget: usize,
    tx_ring: alloc::boxed::Box<[[u8; PACKET_BUF_SIZE]]>,
    tx_lens: alloc::boxed::Box<[u16]>,
    tx_head: usize,
    tx_tail: usize,
    tx_count: usize,
}

impl UbootNetDevice {
    pub fn new(info: uboot::DeviceInfo) -> Self {
        Self {
            info,
            rx_ring: alloc::vec![[0u8; PACKET_BUF_SIZE]; RX_RING_SIZE].into_boxed_slice(),
            rx_lens: alloc::vec![0u16; RX_RING_SIZE].into_boxed_slice(),
            rx_head: 0,
            rx_tail: 0,
            rx_count: 0,
            rx_budget: 256,
            tx_ring: alloc::vec![[0u8; PACKET_BUF_SIZE]; TX_RING_SIZE].into_boxed_slice(),
            tx_lens: alloc::vec![0u16; TX_RING_SIZE].into_boxed_slice(),
            tx_head: 0,
            tx_tail: 0,
            tx_count: 0,
        }
    }

    /// Refills the 2048-slot DRAM RX ring from U-Boot's hardware DMA descriptors on every poll
    /// cycle (so hardware DMA descriptors are recycled immediately even during multi-peer TCP
    /// bursts) while bounding per-poll `smoltcp` work to `budget`.
    pub fn begin_poll(&mut self, budget: usize) {
        self.rx_budget = budget;
        let free_slots = (RX_RING_SIZE - self.rx_count).min(256);
        if free_slots > 0 {
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
    }

    /// Flushes all queued TX frames from the 2048-slot DRAM TX ring to U-Boot in a single batch.
    pub fn flush_tx(&mut self) {
        if self.tx_count > 0 {
            let sent = uboot::eth_send_batch(
                &self.info,
                &self.tx_ring,
                &self.tx_lens,
                self.tx_head,
                self.tx_count,
            );
            self.tx_head = (self.tx_head + sent) % TX_RING_SIZE;
            self.tx_count -= sent;
        }
    }
}

impl Device for UbootNetDevice {
    type RxToken<'a> = UbootRxToken<'a> where Self: 'a;
    type TxToken<'a> = UbootTxToken<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_budget == 0 {
            return None;
        }

        if self.rx_count > 0 {
            self.rx_budget -= 1;
            let head = self.rx_head;
            let len = self.rx_lens[head] as usize;
            self.rx_head = (self.rx_head + 1) % RX_RING_SIZE;
            self.rx_count -= 1;

            let rx = UbootRxToken {
                buf: &self.rx_ring[head][..len],
            };
            let tx = UbootTxToken {
                info: &self.info,
                tx_ring: &mut self.tx_ring,
                tx_lens: &mut self.tx_lens,
                tx_tail: &mut self.tx_tail,
                tx_count: &mut self.tx_count,
            };
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_count >= TX_RING_SIZE {
            self.flush_tx();
        }
        Some(UbootTxToken {
            info: &self.info,
            tx_ring: &mut self.tx_ring,
            tx_lens: &mut self.tx_lens,
            tx_tail: &mut self.tx_tail,
            tx_count: &mut self.tx_count,
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
    tx_ring: &'a mut [[u8; PACKET_BUF_SIZE]],
    tx_lens: &'a mut [u16],
    tx_tail: &'a mut usize,
    tx_count: &'a mut usize,
}

impl<'a> TxToken for UbootTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        if *self.tx_count < TX_RING_SIZE && len <= PACKET_BUF_SIZE {
            let idx = *self.tx_tail;
            let slot = &mut self.tx_ring[idx];
            let res = f(&mut slot[..len]);
            let transmit_len = core::cmp::max(len, 60).min(PACKET_BUF_SIZE);
            if transmit_len > len {
                slot[len..transmit_len].fill(0);
            }
            self.tx_lens[idx] = transmit_len as u16;
            *self.tx_tail = (idx + 1) % TX_RING_SIZE;
            *self.tx_count += 1;
            res
        } else {
            let mut fallback = [0u8; 2048];
            let res = f(&mut fallback[..len]);
            let transmit_len = core::cmp::max(len, 60);
            let _ = uboot::eth_send(self.info, &fallback[..transmit_len]);
            res
        }
    }
}
