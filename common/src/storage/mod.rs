pub mod bpm;
pub mod btree;

use core::ptr::NonNull;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::transport::{DeviceType, Transport};
use crate::net::virtio_hal::VirtioHal;
use crate::uart;

use spin::Mutex;
pub static DB: Mutex<Option<btree::BTree<'static>>> = Mutex::new(None);

const VIRTIO0: usize = 0x10001000;
const VIRTIO_STEP: usize = 0x1000;
const VIRTIO_MAX: usize = 0x10008000;

pub struct StorageContext<'a> {
    pub driver: VirtIOBlk<VirtioHal, MmioTransport<'a>>,
}

impl<'a> StorageContext<'a> {
    pub fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> Result<(), virtio_drivers::Error> {
        self.driver.read_blocks(block_id, buf)
    }

    pub fn write_block(&mut self, block_id: usize, buf: &[u8]) -> Result<(), virtio_drivers::Error> {
        self.driver.write_blocks(block_id, buf)
    }

    pub fn capacity(&self) -> u64 {
        self.driver.capacity()
    }
}

pub fn init_virtio_blk<'a>() -> Option<StorageContext<'a>> {
    for addr in (VIRTIO0..=VIRTIO_MAX).step_by(VIRTIO_STEP) {
        let header = NonNull::new(addr as *mut VirtIOHeader).unwrap();
        match unsafe { MmioTransport::new(header, VIRTIO_STEP) } {
            Ok(transport) => {
                if transport.device_type() == DeviceType::Block {
                    uart::print(b"[storage] Found VirtIO Block device at ");
                    uart::print_usize(addr);
                    uart::print(b"\n");

                    match VirtIOBlk::<VirtioHal, MmioTransport>::new(transport) {
                        Ok(driver) => {
                            let capacity = driver.capacity();
                            uart::print(b"[storage] Capacity: ");
                            uart::print_usize((capacity * 512 / 1024 / 1024) as usize);
                            uart::print(b" MB\n");

                            return Some(StorageContext {
                                driver,
                            });
                        }
                        Err(_e) => {
                            uart::print(b"[storage] Failed to initialize VirtIOBlk\n");
                        }
                    }
                }
            }
            Err(_) => {} // not a virtio device or unsupported
        }
    }
    uart::print(b"[storage] No VirtIO Block device found\n");
    None
}
