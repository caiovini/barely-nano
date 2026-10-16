//! Buffer Pool Manager (BPM)
//!
//! In a bare-metal environment without an Operating System (no Linux, no Windows),
//! we don't have access to high-level abstractions like memory-mapped files (mmap)
//! or sophisticated filesystem caches.
//!
//! The BPM provides a middle layer between our storage hardware (e.g., NVMe SSD via U-Boot API)
//! and our high-level data structures (like the B-Tree). It maintains a fixed-size cache
//! of "Frames" in memory to minimize slow hardware I/O.

use crate::storage::StorageContext;
extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::sync::Arc;
use spin::Mutex;

/// The fundamental unit of storage and caching. Matches typical SSD block sizes.
pub const BLOCK_SIZE: usize = 4096;

/// A container for a single cached block of data.
pub struct Frame {
    pub data: crate::storage::AlignedBlock,
    pub page_id: Option<usize>,
    pub is_dirty: bool,
    pub pin_count: u32,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            data: crate::storage::AlignedBlock::new(),
            page_id: None,
            is_dirty: false,
            pin_count: 0,
        }
    }
}

/// Manages a set of memory frames and handles the movement of data between 
/// storage and memory using an LRU (Least Recently Used) eviction policy.
pub struct BufferPool {
    storage: Arc<Mutex<StorageContext>>,
    /// The physical page on disk where this buffer pool's virtual "Page 0" starts.
    pub base_page: usize,
    frames: Vec<Frame>,
    /// Mapping from Page ID to its current index in the `frames` vector.
    page_table: BTreeMap<usize, usize>, // page_id -> frame_id
    /// Tracks frame usage. The frame at index 0 is the least recently used.
    lru_queue: Vec<usize>,              // frame_ids ordered from LRU (index 0) to MRU (index N)
}

pub struct PageGuard {
    bpm: *mut BufferPool,
    frame_id: usize,
    is_dirty: bool,
}

impl PageGuard {
    pub fn data(&self) -> &[u8; BLOCK_SIZE] {
        unsafe { &(&(*self.bpm).frames)[self.frame_id].data.bytes }
    }

    pub fn data_mut(&mut self) -> &mut [u8; BLOCK_SIZE] {
        self.is_dirty = true;
        unsafe { &mut (&mut (*self.bpm).frames)[self.frame_id].data.bytes }
    }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        unsafe {
            (*self.bpm).unpin(self.frame_id, self.is_dirty);
        }
    }
}

impl BufferPool {
    pub fn new(storage: Arc<Mutex<StorageContext>>, num_frames: usize, base_page: usize) -> Self {
        let mut frames = Vec::with_capacity(num_frames);
        let mut lru_queue = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            frames.push(Frame::new());
            lru_queue.push(i);
        }
        
        Self {
            storage,
            base_page,
            frames,
            page_table: BTreeMap::new(),
            lru_queue,
        }
    }

    /// Mark a frame as recently used
    fn mark_mru(&mut self, frame_id: usize) {
        // Optimization: If it's already the most recently used, do nothing.
        if self.lru_queue.last() == Some(&frame_id) {
            return;
        }

        if let Some(pos) = self.lru_queue.iter().position(|&x| x == frame_id) {
            self.lru_queue.remove(pos);
            self.lru_queue.push(frame_id);
        }
    }

    /// Retrieves a page from memory or loads it from storage if not present.
    ///
    /// If all frames are full, it will select a "victim" frame using LRU, 
    /// flush it to disk if it's dirty, and reuse the frame for the new page.
    pub fn fetch_page(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        let frame_id = self.fetch_frame_id(page_id)?;
        Ok(PageGuard {
            bpm: self as *mut _,
            frame_id,
            is_dirty: false,
        })
    }

    fn fetch_frame_id(&mut self, page_id: usize) -> Result<usize, crate::storage::StorageError> {
        // Cache Hit
        if let Some(&frame_id) = self.page_table.get(&page_id) {
            self.mark_mru(frame_id);
            self.frames[frame_id].pin_count += 1;
            return Ok(frame_id);
        }

        // Find a victim frame to evict. 
        let mut victim_idx: Option<usize> = None;
        for (idx, &fid) in self.lru_queue.iter().enumerate() {
            if self.frames[fid].pin_count == 0 && !self.frames[fid].is_dirty {
                victim_idx = Some(idx);
                break;
            }
        }

        if victim_idx.is_none() {
            for (idx, &fid) in self.lru_queue.iter().enumerate() {
                if self.frames[fid].pin_count == 0 {
                    victim_idx = Some(idx);
                    break;
                }
            }
        }

        let victim_frame_id = if let Some(v_idx) = victim_idx {
            self.lru_queue.remove(v_idx)
        } else {
            return Err(crate::storage::StorageError::IoError); 
        };

        if let Some(pid) = self.frames[victim_frame_id].page_id {
            if self.frames[victim_frame_id].is_dirty {
                self.storage.lock().write_block(pid + self.base_page, &self.frames[victim_frame_id].data.bytes)?;
            }
            self.page_table.remove(&pid);
        }

        let frame = &mut self.frames[victim_frame_id];
        frame.page_id = Some(page_id);
        frame.is_dirty = false;
        frame.pin_count = 1;
        
        self.storage.lock().read_block(page_id + self.base_page, &mut frame.data.bytes)?;

        self.page_table.insert(page_id, victim_frame_id);
        self.lru_queue.push(victim_frame_id);
        Ok(victim_frame_id)
    }

    fn unpin(&mut self, frame_id: usize, is_dirty: bool) {
        if self.frames[frame_id].pin_count > 0 {
            self.frames[frame_id].pin_count -= 1;
        }
        if is_dirty {
            self.frames[frame_id].is_dirty = true;
        }
    }

    pub fn get_frame_mut(&mut self, frame_id: usize) -> &mut Frame {
        &mut self.frames[frame_id]
    }

    /// Persists all dirty frames to the storage device.
    ///
    /// This should be called before shutdown or during critical synchronization points
    /// to ensure data durability.
    pub fn flush_all(&mut self) -> Result<(), crate::storage::StorageError> {
        let mut storage_guard = self.storage.lock();

        for frame in &mut self.frames {
            if frame.is_dirty {
                if let Some(pid) = frame.page_id {
                    storage_guard.write_block(pid + self.base_page, &frame.data.bytes)?;
                    frame.is_dirty = false;
                }
            }
        }
        Ok(())
    }

    pub fn fetch_page_read_only(&mut self, page_id: usize) -> Result<PageGuard, crate::storage::StorageError> {
        self.fetch_page(page_id)
    }
}
