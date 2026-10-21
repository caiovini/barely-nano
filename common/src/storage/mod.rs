pub mod bpm;
pub mod btree;

#[repr(align(64))]
#[derive(Clone, Copy)]
pub struct AlignedBlock {
    pub bytes: [u8; 4096],
}

impl AlignedBlock {
    pub const fn new() -> Self {
        Self { bytes: [0; 4096] }
    }
}

impl Default for AlignedBlock {
    fn default() -> Self {
        Self::new()
    }
}

use crate::uart;
use crate::uboot;

use spin::Mutex;
use alloc::sync::Arc;

pub const SHARD_COUNT: usize = 16;

pub static SHARDS: [Mutex<Option<btree::BTree>>; SHARD_COUNT] = [
    Mutex::new(None), Mutex::new(None), Mutex::new(None), Mutex::new(None),
    Mutex::new(None), Mutex::new(None), Mutex::new(None), Mutex::new(None),
    Mutex::new(None), Mutex::new(None), Mutex::new(None), Mutex::new(None),
    Mutex::new(None), Mutex::new(None), Mutex::new(None), Mutex::new(None),
];

pub static SHARD_SIZE_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Helper for legacy code or single-shard access
pub fn db_shard(index: usize) -> &'static Mutex<Option<btree::BTree>> {
    &SHARDS[index % SHARD_COUNT]
}

pub fn get_shard_index(key: &[u8]) -> usize {
    if key.len() < 2 { return 0; }
    // We use the high 4 bits of the first byte of the actual key (after Table ID) to shard.
    // This provides Prefix Sharding, which allows range scans to work by visiting shards in order.
    (key[1] >> 4) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    IoError,
}

pub static GLOBAL_STORAGE: Mutex<Option<Arc<Mutex<StorageContext>>>> = Mutex::new(None);

pub enum StorageBackend {
    UBoot(uboot::DeviceInfo),
    RamDisk(alloc::vec::Vec<u8>),
}

pub struct StorageContext {
    pub backend: StorageBackend,
}

pub const STORAGE_BASE_LBA: u64 = 1_048_576; // 512 MB offset to prevent overwriting U-Boot boot partition/SPL

impl StorageContext {
    pub fn new_uboot(info: uboot::DeviceInfo) -> Self {
        Self { backend: StorageBackend::UBoot(info) }
    }

    pub fn new_ramdisk(size_mb: usize) -> Self {
        let size = size_mb * 1024 * 1024;
        Self { backend: StorageBackend::RamDisk(alloc::vec![0; size]) }
    }

    pub fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> Result<(), StorageError> {
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = (block_id * 8) as u64 + STORAGE_BASE_LBA;
                let count = 8;
                let ret = uboot::dev_read(info, lba, count, buf.as_mut_ptr());
                if ret > 0 {
                    Ok(())
                } else {
                    Err(StorageError::IoError)
                }
            }
            StorageBackend::RamDisk(vec) => {
                let offset = block_id * 4096;
                if offset + 4096 <= vec.len() {
                    buf.copy_from_slice(&vec[offset..offset + 4096]);
                    Ok(())
                } else {
                    Err(StorageError::IoError)
                }
            }
        }
    }

    pub fn write_block(&mut self, block_id: usize, buf: &[u8]) -> Result<(), StorageError> {
        match &mut self.backend {
            StorageBackend::UBoot(info) => {
                let lba = (block_id * 8) as u64 + STORAGE_BASE_LBA;
                let count = 8;
                let ret = uboot::dev_write(info, lba, count, buf.as_ptr());
                if ret > 0 {
                    Ok(())
                } else {
                    tracing::error!("Storage: FAILED to write block {}", block_id);
                    Err(StorageError::IoError)
                }
            }
            StorageBackend::RamDisk(vec) => {
                let offset = block_id * 4096;
                if offset + 4096 <= vec.len() {
                    vec[offset..offset + 4096].copy_from_slice(buf);
                    Ok(())
                } else {
                    tracing::error!("Storage: FAILED to write RAM block {}", block_id);
                    Err(StorageError::IoError)
                }
            }
        }
    }

    pub fn capacity(&self) -> u64 {
        match &self.backend {
            StorageBackend::UBoot(info) => {
                if info.block_count > STORAGE_BASE_LBA {
                    info.block_count - STORAGE_BASE_LBA
                } else {
                    0
                }
            }
            StorageBackend::RamDisk(vec) => (vec.len() / 512) as u64,
        }
    }
}



pub fn flush_all_shards() {
    for i in 0..SHARD_COUNT {
        if let Some(mut guard) = SHARDS[i].try_lock() {
            if let Some(db) = guard.as_mut() {
                let db: &mut btree::BTree = db;
                let _ = db.flush();
            }
        }
    }
}

pub fn init_uboot_blk() {
    uart::print(b"[storage] Scanning U-Boot devices for Storage...\n");
    
    let mut ssd_info = None;
    let mut fallback_info = None;
    let mut last_info = None;
    let mut max_devices = 32;
    while max_devices > 0 {
        if let Some(info) = uboot::dev_enum(last_info.as_ref()) {
            uart::print(b"[storage] Discovered Device - Type: ");
            uart::print_usize(info.type_ as usize);
            uart::print(b", Cookie: ");
            uart::print_usize(info.cookie as usize);
            uart::print(b", Blocks: ");
            uart::print_usize(info.block_count as usize);
            uart::print(b"\n");

            if info.type_ == uboot::DEV_TYP_STOR {
                let if_type = if !info.cookie.is_null() {
                    unsafe { *(info.cookie as *const i32) }
                } else {
                    -1
                };
                
                uart::print(b"[storage] -> Block Storage Device found! if_type: ");
                if if_type >= 0 {
                    uart::print_usize(if_type as usize);
                } else {
                    uart::print(b"unknown/none");
                }
                uart::print(b"\n");

                // Target NVMe SSD / NVMe/SCSI device, ignore MMC/SD card (if_type 6 and 7)
                if if_type != 6 && if_type != 7 && if_type != -1 {
                    uart::print(b"[storage] -> Selected as NVMe SSD Device!\n");
                    ssd_info = Some(info);
                    break;
                }
                
                if fallback_info.is_none() {
                    uart::print(b"[storage] -> Registered as Fallback Storage (MMC/SD Card).\n");
                    fallback_info = Some(info);
                }
            }
            last_info = Some(info);
        } else {
            break;
        }
        max_devices -= 1;
    }

    let target_info = if let Some(info) = ssd_info {
        uart::print(b"[storage] Selecting NVMe SSD as primary storage.\n");
        Some(info)
    } else if let Some(info) = fallback_info {
        uart::print(b"[storage] WARNING: NVMe SSD not found, falling back to SD card.\n");
        Some(info)
    } else {
        None
    };

    if let Some(mut info) = target_info {
        uart::print(b"[storage] Opening U-Boot Storage Device...\n");
        let res = uboot::dev_open(&mut info);
        if res != 0 {
            uart::print(b"[storage] WARNING: Failed to open U-Boot Storage Device: ");
            uart::print_usize(res as usize);
            uart::print(b"\n");
        }
        let storage = StorageContext::new_uboot(info);
        init_shards(storage);
    } else {
        uart::print(b"[storage] No U-Boot Storage Device found. Allocating 128MB RAM Disk!\n");
        let storage = StorageContext::new_ramdisk(128);
        init_shards(storage);
    }
}

const STORAGE_MAGIC: u64 = 0x5348415244534846; // "SHARDSHD" + 2
const HEADER_PAGE: usize = 0;

fn init_shards(mut storage: StorageContext) {
    let capacity_sectors = storage.capacity();
    let total_pages = (capacity_sectors / 8) as usize;
    uart::print(b"[storage] Drive Capacity: ");
    uart::print_usize(total_pages * 4 / 1024);
    uart::print(b" MB (");
    uart::print_usize(total_pages);
    uart::print(b" pages)\n");
    
    // Read header from Page 0
    let mut header_data = AlignedBlock::new();
    let _ = storage.read_block(HEADER_PAGE, &mut header_data.bytes);
    
    let magic = u64::from_le_bytes(header_data.bytes[0..8].try_into().unwrap());
    let mut shard_size = if magic == STORAGE_MAGIC {
        u64::from_le_bytes(header_data.bytes[8..16].try_into().unwrap()) as usize
    } else {
        0
    };

    if shard_size == 0 || shard_size > total_pages {
        // First time initialization or invalid header
        shard_size = (total_pages - 1) / SHARD_COUNT;
        uart::print(b"[storage] Initializing new storage header...\n");
        
        let mut new_header = AlignedBlock::new();
        new_header.bytes[0..8].copy_from_slice(&STORAGE_MAGIC.to_le_bytes());
        new_header.bytes[8..16].copy_from_slice(&(shard_size as u64).to_le_bytes());
        new_header.bytes[16..20].copy_from_slice(&(SHARD_COUNT as u32).to_le_bytes());
        
        if let Err(_e) = storage.write_block(HEADER_PAGE, &new_header.bytes) {
            uart::print(b"[storage] FAILED to write header: ");
        }
    }
    
    SHARD_SIZE_PAGES.store(shard_size, core::sync::atomic::Ordering::SeqCst);


    let shared_storage = Arc::new(Mutex::new(storage));
    *GLOBAL_STORAGE.lock() = Some(shared_storage.clone());
    
    for i in 0..SHARD_COUNT {
        // Shards start at Page 1 to avoid colliding with the header
        let base_page = 1 + (i * (shard_size + 1));
        let bpm = bpm::BufferPool::new(shared_storage.clone(), 2048, base_page);
        let btree = btree::BTree::new(bpm);
        *SHARDS[i].lock() = Some(btree);
    }
}

pub fn process_delegated_requests() {}
