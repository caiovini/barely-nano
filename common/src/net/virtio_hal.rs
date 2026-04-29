extern crate alloc;

use alloc::alloc::{alloc, dealloc, Layout};
use core::ptr::NonNull;
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

pub struct VirtioHal;

const PAGE_SIZE: usize = 4096;

unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            panic!("VirtioHal: failed to allocate {} pages of DMA memory", pages);
        }
        let paddr = ptr as usize as PhysAddr;
        (paddr, NonNull::new(ptr).unwrap())
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        unsafe { dealloc(vaddr.as_ptr(), layout) };
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(paddr as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        let ptr = buffer.cast::<u8>().as_ptr();
        ptr as usize as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        // Nothing to do for identity-mapped non-cached memory architecture (like basic QEMU bare metal)
    }
}
