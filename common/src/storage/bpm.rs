use crate::storage::StorageContext;
extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub const BLOCK_SIZE: usize = 4096;

pub struct Frame {
    pub data: [u8; BLOCK_SIZE],
    pub page_id: Option<usize>,
    pub is_dirty: bool,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            data: [0; BLOCK_SIZE],
            page_id: None,
            is_dirty: false,
        }
    }
}

pub struct BufferPool<'a> {
    storage: StorageContext<'a>,
    frames: Vec<Frame>,
    page_table: BTreeMap<usize, usize>, // page_id -> frame_id
    lru_queue: Vec<usize>,              // frame_ids ordered from LRU (index 0) to MRU (index N)
}

impl<'a> BufferPool<'a> {
    pub fn new(storage: StorageContext<'a>, num_frames: usize) -> Self {
        let mut frames = Vec::with_capacity(num_frames);
        let mut lru_queue = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            frames.push(Frame::new());
            lru_queue.push(i);
        }
        
        Self {
            storage,
            frames,
            page_table: BTreeMap::new(),
            lru_queue,
        }
    }

    /// Mark a frame as recently used
    fn mark_mru(&mut self, frame_id: usize) {
        if let Some(pos) = self.lru_queue.iter().position(|&x| x == frame_id) {
            self.lru_queue.remove(pos);
            self.lru_queue.push(frame_id);
        }
    }

    pub fn fetch_page(&mut self, page_id: usize) -> Result<&mut Frame, virtio_drivers::Error> {
        // Cache Hit
        if let Some(&frame_id) = self.page_table.get(&page_id) {
            self.mark_mru(frame_id);
            return Ok(&mut self.frames[frame_id]);
        }

        // Cache Miss: Need to find a victim frame
        let victim_frame_id = self.lru_queue.remove(0);
        
        // If victim is dirty, flush it to disk
        let victim_page_id = self.frames[victim_frame_id].page_id;
        if self.frames[victim_frame_id].is_dirty {
            if let Some(pid) = victim_page_id {
                /*
                crate::uart::print(b"[bpm] Flushing dirty page ");
                crate::uart::print_usize(pid);
                crate::uart::print(b" to disk...\n");
                */
                self.storage.write_block(pid, &self.frames[victim_frame_id].data)?;
            }
        }

        // Remove old mapping
        if let Some(pid) = victim_page_id {
            self.page_table.remove(&pid);
        }

        // Load new page from disk
        self.storage.read_block(page_id, &mut self.frames[victim_frame_id].data)?;

        // Update frame metadata
        self.frames[victim_frame_id].page_id = Some(page_id);
        self.frames[victim_frame_id].is_dirty = false;
        
        // Update page table
        self.page_table.insert(page_id, victim_frame_id);
        
        // Mark as MRU
        self.lru_queue.push(victim_frame_id);

        Ok(&mut self.frames[victim_frame_id])
    }

    pub fn flush_all(&mut self) -> Result<(), virtio_drivers::Error> {
        for frame in &mut self.frames {
            if frame.is_dirty {
                if let Some(pid) = frame.page_id {
                    self.storage.write_block(pid, &frame.data)?;
                    frame.is_dirty = false;
                }
            }
        }
        Ok(())
    }

    pub fn fetch_page_read_only(&mut self, page_id: usize) -> Result<&Frame, virtio_drivers::Error> {
        self.fetch_page(page_id).map(|f| &*f)
    }
}
