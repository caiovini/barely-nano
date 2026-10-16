//! B-Tree Storage Engine
//!
//! Since we are running in a bare-metal environment, we cannot use off-the-shelf 
//! databases like LMDB or RocksDB, which rely heavily on POSIX APIs (mmap, ftruncate, 
//! pwrite, etc.).
//!
//! This B-Tree implementation is built directly on top of our Buffer Pool Manager (BPM).
//! It provides a persistent, ordered key-value store where every node corresponds
//! to exactly one 4KB block (page) on the hardware storage.

use crate::storage::bpm::{BufferPool, BLOCK_SIZE};
extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// Maximum number of keys a single B-Tree node can hold.
/// Chosen to ensure a node (keys + values + children) fits in a 4096-byte block.
pub const MAX_KEYS: usize = 12;
/// Minimum number of keys a node must have (except the root).
pub const MIN_KEYS: usize = 4;
/// Size of the key in bytes.
pub const KEY_SIZE: usize = 65; // 1 byte table ID + 64 bytes key
/// Size of the value in bytes.
pub const VALUE_SIZE: usize = 256;

/// A single node in the B-Tree, stored as a 4KB block.
#[derive(Clone, Debug)]
pub struct BTreeNode {
    /// Whether this node is a leaf (has no children).
    pub is_leaf: bool,
    /// Number of keys currently stored in this node.
    pub num_keys: usize,
    /// Sorted array of keys.
    pub keys: Box<[[u8; KEY_SIZE]; MAX_KEYS]>,
    /// Array of values associated with the keys.
    pub values: Box<[[u8; VALUE_SIZE]; MAX_KEYS]>,
    /// Exact length of each value.
    pub value_lengths: [u16; MAX_KEYS],
    /// Page IDs of the children nodes (only used in internal nodes).
    pub children: [u32; MAX_KEYS + 1],
}

impl BTreeNode {
    pub fn new(is_leaf: bool) -> Self {
        Self {
            is_leaf,
            num_keys: 0,
            keys: Box::new([[0; KEY_SIZE]; MAX_KEYS]),
            values: Box::new([[0; VALUE_SIZE]; MAX_KEYS]),
            value_lengths: [0; MAX_KEYS],
            children: [0; MAX_KEYS + 1],
        }
    }

    /// Deserializes a B-Tree node from a raw 4KB block.
    pub fn from_bytes(data: &[u8; BLOCK_SIZE]) -> Self {
        let is_leaf = data[0] != 0;
        let mut num_keys = u16::from_le_bytes([data[1], data[2]]) as usize;
        if num_keys > MAX_KEYS {
            num_keys = MAX_KEYS;
        }
        
        let mut keys = Box::new([[0u8; KEY_SIZE]; MAX_KEYS]);
        let mut values = Box::new([[0u8; VALUE_SIZE]; MAX_KEYS]);
        let mut value_lengths = [0u16; MAX_KEYS];
        let mut children = [0u32; MAX_KEYS + 1];

        // Read all keys
        let mut offset = 4;
        for i in 0..MAX_KEYS {
            keys[i].copy_from_slice(&data[offset..offset + KEY_SIZE]);
            offset += KEY_SIZE;
        }

        // Read all values
        offset = 4 + MAX_KEYS * KEY_SIZE;
        for i in 0..MAX_KEYS {
            values[i].copy_from_slice(&data[offset..offset + VALUE_SIZE]);
            offset += VALUE_SIZE;
        }
        
        // Read all lengths
        offset = 4 + MAX_KEYS * KEY_SIZE + MAX_KEYS * VALUE_SIZE;
        for i in 0..MAX_KEYS {
            value_lengths[i] = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
            offset += 2;
        }

        // Read all children pointers
        offset = 4 + MAX_KEYS * (KEY_SIZE + VALUE_SIZE + 2);
        for i in 0..=MAX_KEYS {
            children[i] = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
        }

        Self { is_leaf, num_keys, keys, values, value_lengths, children }
    }

    /// Serializes the B-Tree node into a raw 4KB block for persistence.
    pub fn to_bytes(&self, data: &mut [u8; BLOCK_SIZE]) {
        data.fill(0);
        data[0] = if self.is_leaf { 1 } else { 0 };
        let nk_bytes = (self.num_keys as u16).to_le_bytes();
        data[1] = nk_bytes[0];
        data[2] = nk_bytes[1];

        let mut offset = 4;
        for i in 0..MAX_KEYS {
            data[offset..offset + KEY_SIZE].copy_from_slice(&self.keys[i]);
            offset += KEY_SIZE;
        }

        offset = 4 + MAX_KEYS * KEY_SIZE;
        for i in 0..MAX_KEYS {
            data[offset..offset + VALUE_SIZE].copy_from_slice(&self.values[i]);
            offset += VALUE_SIZE;
        }
        
        offset = 4 + MAX_KEYS * KEY_SIZE + MAX_KEYS * VALUE_SIZE;
        for i in 0..MAX_KEYS {
            data[offset..offset + 2].copy_from_slice(&self.value_lengths[i].to_le_bytes());
            offset += 2;
        }

        offset = 4 + MAX_KEYS * (KEY_SIZE + VALUE_SIZE + 2);
        for i in 0..=MAX_KEYS {
            data[offset..offset + 4].copy_from_slice(&self.children[i].to_le_bytes());
            offset += 4;
        }
    }
}

/// Global metadata for the B-Tree, stored in Page 0.
pub struct MetaData {
    /// Page ID of the root node of the B-Tree.
    pub root_page_id: u32,
    /// Counter used for basic page allocation.
    pub next_free_page: u32,
    /// Head of the linked list of free pages that can be reused.
    pub first_free_page: u32,
}

impl MetaData {
    pub fn from_bytes(data: &[u8; BLOCK_SIZE]) -> Self {
        // Use a magic number to verify that the storage is initialized.
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != 0xBEEFCB02 {
            // Uninitialized or corrupted
            return Self { root_page_id: 0, next_free_page: 1, first_free_page: 0 };
        }
        let root_page_id = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let next_free_page = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let first_free_page = u32::from_le_bytes(data[12..16].try_into().unwrap());
        Self { root_page_id, next_free_page, first_free_page }
    }

    pub fn to_bytes(&self, data: &mut [u8; BLOCK_SIZE]) {
        data.fill(0);
        data[0..4].copy_from_slice(&0xBEEFCB02_u32.to_le_bytes());
        data[4..8].copy_from_slice(&self.root_page_id.to_le_bytes());
        data[8..12].copy_from_slice(&self.next_free_page.to_le_bytes());
        data[12..16].copy_from_slice(&self.first_free_page.to_le_bytes());
    }
}

pub struct BTree {
    pub bpm: BufferPool,
    meta: MetaData,
}

impl BTree {
    pub fn new(mut bpm: BufferPool) -> Self {
        // Kick watchdog during initialization of each shard
        crate::watchdog::kick();

        let meta = {
            let guard = bpm.fetch_page(0).unwrap();
            MetaData::from_bytes(guard.data())
        };
        
        let mut tree = Self { bpm, meta };
        
        // Print shard parameters
        crate::uart::print(b"[btree] Init: root=");
        crate::uart::print_usize(tree.meta.root_page_id as usize);
        crate::uart::print(b" next=");
        crate::uart::print_usize(tree.meta.next_free_page as usize);
        crate::uart::print(b"\n");
        
        if tree.meta.root_page_id == 0 {
            if tree.meta.next_free_page <= 1 {
                tree.meta.next_free_page = 1; 
                let root_id = tree.allocate_page();
                let root_node = BTreeNode::new(true);
                tree.write_node(root_id, &root_node);
                tree.meta.root_page_id = root_id;
                tree.save_meta();
                let _ = tree.flush();
            } else {
                crate::uart::print(b"[storage] BTree metadata corruption detected! root_page_id is 0 but next_free_page > 1. Wiping and re-initializing...\n");
                tree.meta.next_free_page = 1; 
                let root_id = tree.allocate_page();
                let root_node = BTreeNode::new(true);
                tree.write_node(root_id, &root_node);
                tree.meta.root_page_id = root_id;
                tree.save_meta();
                let _ = tree.flush();
            }
        }
        tree
    }

    fn save_meta(&mut self) {
        let mut guard = self.bpm.fetch_page(0).unwrap();
        self.meta.to_bytes(guard.data_mut());
    }

    pub fn flush(&mut self) -> Result<(), crate::storage::StorageError> {
        self.bpm.flush_all()
    }

    pub fn wipe(&mut self) {
        self.meta = MetaData {
            root_page_id: 0,
            next_free_page: 0,
            first_free_page: 0,
        };
        self.save_meta();
        let _ = self.flush();
    }

    pub fn allocate_page(&mut self) -> u32 {
        if self.meta.first_free_page != 0 {
            // Reuse a page from the free list
            let reused_id = self.meta.first_free_page;
            let next_free = {
                let guard = self.bpm.fetch_page(reused_id as usize).unwrap();
                u32::from_le_bytes(guard.data()[0..4].try_into().unwrap())
            };
            self.meta.first_free_page = next_free;
            self.save_meta();
            reused_id
        } else {
            if self.meta.next_free_page == 0 {
                self.meta.next_free_page = 1;
            }
            let id = self.meta.next_free_page;
            self.meta.next_free_page += 1;
            
            // Update metadata in memory and mark dirty
            self.save_meta();
            id
        }
    }

    /// Adds a page to the free list for future reuse.
    pub fn free_page(&mut self, page_id: u32) {
        if page_id == 0 || page_id == self.meta.root_page_id {
            return;
        }
        
        let first_free = self.meta.first_free_page;
        {
            let mut guard = self.bpm.fetch_page(page_id as usize).unwrap();
            guard.data_mut()[0..4].copy_from_slice(&first_free.to_le_bytes());
        }
        
        // Update the head to this page.
        self.meta.first_free_page = page_id;
        self.save_meta();
    }

    pub fn read_page(&mut self, page_id: u32) -> [u8; BLOCK_SIZE] {
        let guard = self.bpm.fetch_page(page_id as usize).unwrap();
        *guard.data()
    }

    pub fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) {
        let mut guard = self.bpm.fetch_page(page_id as usize).unwrap();
        guard.data_mut().copy_from_slice(data);
    }

    fn read_node(&mut self, page_id: u32) -> BTreeNode {
        let guard = self.bpm.fetch_page(page_id as usize).expect("Failed to fetch node");
        BTreeNode::from_bytes(guard.data())
    }

    fn write_node(&mut self, page_id: u32, node: &BTreeNode) {
        let mut guard = self.bpm.fetch_page(page_id as usize).expect("Failed to fetch node for write");
        node.to_bytes(guard.data_mut());
    }

    /// Searches for a key in the B-Tree and returns the associated value if found.
    ///
    /// This is a standard B-Tree search: starting from the root, we binary search 
    /// (or linear search here due to small MAX_KEYS) within each node to find 
    /// the key or determine which child to descend into.
    pub fn search(&mut self, key: &[u8; KEY_SIZE]) -> Option<Vec<u8>> {
        let mut current_id = self.meta.root_page_id;
        loop {
            let node = self.read_node(current_id);

            // Binary search for the first key >= target key
            let mut i = 0;
            let mut low = 0;
            let mut high = node.num_keys as isize - 1;
            while low <= high {
                let mid = (low + high) / 2;
                if key > &node.keys[mid as usize] {
                    low = mid + 1;
                    i = low as usize;
                } else if key < &node.keys[mid as usize] {
                    high = mid - 1;
                    i = mid as usize;
                } else {
                    i = mid as usize;
                    break;
                }
            }
            if i < node.num_keys && &node.keys[i] == key {
                let len = node.value_lengths[i] as usize;
                return Some(node.values[i][0..len].to_vec());
            }

            if node.is_leaf {
                return None;
            } else {
                let next_id = node.children[i];
                current_id = next_id;
            }
        }
    }

    pub fn insert(&mut self, key: [u8; KEY_SIZE], value: &[u8]) {
        let root_id = self.meta.root_page_id;
        let mut root = self.read_node(root_id);

        if root.num_keys == MAX_KEYS {
            // Root is full, grow the tree in height.
            let new_root_id = self.allocate_page();
            let mut new_root = BTreeNode::new(false);
            new_root.children[0] = root_id;
            
            // Split the old root and move the median key to the new root.
            self.split_child(&mut new_root, 0, root_id, &mut root);
            self.write_node(new_root_id, &new_root);
            
            self.meta.root_page_id = new_root_id;
            self.save_meta();
            
            // Insert into the now non-full root.
            self.insert_non_full(new_root_id, &mut new_root, key, value);
        } else {
            // Root is not full, insert directly.
            self.insert_non_full(root_id, &mut root, key, value);
        }
    }

    /// Splits a full child node of a parent node.
    ///
    /// The median key of the child is pushed up into the parent, and the 
    /// remaining keys are split between the old child and a newly allocated child.
    fn split_child(&mut self, parent: &mut BTreeNode, i: usize, child_id: u32, child: &mut BTreeNode) {
        let new_child_id = self.allocate_page();
        let mut new_child = BTreeNode::new(child.is_leaf);
        
        let t = MAX_KEYS / 2; // Midpoint index
        new_child.num_keys = MAX_KEYS - t - 1;

        // Copy the upper half of keys and values to the new child node.
        for j in 0..new_child.num_keys {
            new_child.keys[j] = child.keys[j + t + 1];
            new_child.values[j] = child.values[j + t + 1];
            new_child.value_lengths[j] = child.value_lengths[j + t + 1];
        }

        // Copy child pointers if this is an internal node.
        if !child.is_leaf {
            for j in 0..=new_child.num_keys {
                new_child.children[j] = child.children[j + t + 1];
            }
        }

        // The median key (at index t) will be moved up to the parent.
        let mid_key = child.keys[t];
        let mid_val = child.values[t];
        let mid_len = child.value_lengths[t];
        child.num_keys = t; // Truncate the old child.

        // Shift parent children to the right to make space for the new child pointer.
        for j in (i + 1..=parent.num_keys).rev() {
            parent.children[j + 1] = parent.children[j];
        }
        parent.children[i + 1] = new_child_id;

        // Shift parent keys/values to the right to make space for the median key.
        for j in (i..parent.num_keys).rev() {
            parent.keys[j + 1] = parent.keys[j];
            parent.values[j + 1] = parent.values[j];
            parent.value_lengths[j + 1] = parent.value_lengths[j];
        }
        parent.keys[i] = mid_key;
        parent.values[i] = mid_val;
        parent.value_lengths[i] = mid_len;
        parent.num_keys += 1;

        // Persist both children back to the Buffer Pool.
        self.write_node(child_id, child);
        self.write_node(new_child_id, &new_child);
    }

    fn insert_non_full(&mut self, node_id: u32, node: &mut BTreeNode, key: [u8; KEY_SIZE], value: &[u8]) {
        let mut i = node.num_keys as isize - 1;

        if node.is_leaf {
            // First check if key already exists (linear scan)
            for j in 0..node.num_keys {
                if node.keys[j] == key {
                    // Update in place
                    let len = value.len().min(VALUE_SIZE);
                    node.values[j].fill(0);
                    node.values[j][0..len].copy_from_slice(&value[..len]);
                    node.value_lengths[j] = len as u16;
                    self.write_node(node_id, node);
                    return;
                }
            }
            // Binary search for insertion point
            let mut low = 0;
            let mut high = node.num_keys as isize - 1;
            let mut insert_pos = node.num_keys;

            while low <= high {
                let mid = (low + high) / 2;
                if key > node.keys[mid as usize] {
                    low = mid + 1;
                    insert_pos = low as usize;
                } else {
                    high = mid - 1;
                    insert_pos = mid as usize;
                }
            }

            // Shift elements to the right
            for j in (insert_pos..node.num_keys).rev() {
                node.keys[j + 1] = node.keys[j];
                node.values[j + 1] = node.values[j];
                node.value_lengths[j + 1] = node.value_lengths[j];
            }
            let len = value.len().min(VALUE_SIZE);
            node.keys[insert_pos] = key;
            node.values[insert_pos].fill(0);
            node.values[insert_pos][0..len].copy_from_slice(&value[..len]);
            node.value_lengths[insert_pos] = len as u16;
            node.num_keys += 1;
            self.write_node(node_id, node);
        } else {
            while i >= 0 && node.keys[i as usize] > key {
                i -= 1;
            }
            
            if i >= 0 && node.keys[i as usize] == key {
                // Key exists in internal node, update value and return
                let len = value.len().min(VALUE_SIZE);
                node.values[i as usize].fill(0);
                node.values[i as usize][0..len].copy_from_slice(&value[..len]);
                node.value_lengths[i as usize] = len as u16;
                self.write_node(node_id, node);
                return;
            }
            
            i += 1;
            let child_id = node.children[i as usize];
            let mut child = self.read_node(child_id);
            
            if child.num_keys == MAX_KEYS {
                self.split_child(node, i as usize, child_id, &mut child);
                
                if key > node.keys[i as usize] {
                    i += 1;
                } else if key == node.keys[i as usize] {
                    // key became the parent key after split, update it
                    let len = value.len().min(VALUE_SIZE);
                    node.values[i as usize].fill(0);
                    node.values[i as usize][0..len].copy_from_slice(&value[..len]);
                    node.value_lengths[i as usize] = len as u16;
                    self.write_node(node_id, node);
                    return;
                }
                
                let next_child_id = node.children[i as usize];
                let mut next_child = self.read_node(next_child_id);
                self.insert_non_full(next_child_id, &mut next_child, key, value);
            } else {
                self.insert_non_full(child_id, &mut child, key, value);
            }
            
            self.write_node(node_id, node);
        }
    }

    pub fn delete(&mut self, key: [u8; KEY_SIZE]) {
        let root_id = self.meta.root_page_id;
        self.delete_recursive(root_id, &key);

        let root = self.read_node(root_id);
        if !root.is_leaf && root.num_keys == 0 {
            // Root is empty, but has a child. Move root to its child.
            let new_root_id = root.children[0];
            self.meta.root_page_id = new_root_id;
            self.save_meta();
            self.free_page(root_id);
        }
    }

    fn delete_recursive(&mut self, node_id: u32, key: &[u8; KEY_SIZE]) {
        let mut node = self.read_node(node_id);
        let mut i = 0;
        while i < node.num_keys && key > &node.keys[i] {
            i += 1;
        }

        if i < node.num_keys && &node.keys[i] == key {
            if node.is_leaf {
                for j in i + 1..node.num_keys {
                    node.keys[j - 1] = node.keys[j];
                    node.values[j - 1] = node.values[j];
                    node.value_lengths[j - 1] = node.value_lengths[j];
                }
                node.num_keys -= 1;
                self.write_node(node_id, &node);
            } else {
                let left_child_id = node.children[i];
                let right_child_id = node.children[i + 1];
                let mut left_child = self.read_node(left_child_id);

                if left_child.num_keys > MIN_KEYS {
                    let (pred_key, pred_val, pred_len) = self.get_predecessor(&left_child);
                    node.keys[i] = pred_key;
                    node.values[i] = pred_val;
                    node.value_lengths[i] = pred_len;
                    self.write_node(node_id, &node);
                    self.delete_recursive(left_child_id, &pred_key);
                } else {
                    let mut right_child = self.read_node(right_child_id);
                    if right_child.num_keys > MIN_KEYS {
                        let (succ_key, succ_val, succ_len) = self.get_successor(&right_child);
                        node.keys[i] = succ_key;
                        node.values[i] = succ_val;
                        node.value_lengths[i] = succ_len;
                        self.write_node(node_id, &node);
                        self.delete_recursive(right_child_id, &succ_key);
                    } else {
                        self.merge_nodes(node_id, &mut node, i, left_child_id, &mut left_child, right_child_id, &mut right_child);
                        self.delete_recursive(left_child_id, key);
                    }
                }
            }
        } else {
            if node.is_leaf {
                return;
            }

            let child_id = node.children[i];
            let mut child = self.read_node(child_id);

            if child.num_keys == MIN_KEYS {
                if i > 0 && self.read_node(node.children[i - 1]).num_keys > MIN_KEYS {
                    self.borrow_from_prev(node_id, &mut node, i);
                } else if i < node.num_keys && self.read_node(node.children[i + 1]).num_keys > MIN_KEYS {
                    self.borrow_from_next(node_id, &mut node, i);
                } else {
                    if i < node.num_keys {
                        let right_id = node.children[i + 1];
                        let mut right = self.read_node(right_id);
                        self.merge_nodes(node_id, &mut node, i, child_id, &mut child, right_id, &mut right);
                    } else {
                        let left_id = node.children[i - 1];
                        let mut left = self.read_node(left_id);
                        self.merge_nodes(node_id, &mut node, i - 1, left_id, &mut left, child_id, &mut child);
                        self.delete_recursive(left_id, key);
                        return;
                    }
                }
                self.delete_recursive(node.children[i], key);
            } else {
                self.delete_recursive(child_id, key);
            }
        }
    }

    fn get_predecessor(&mut self, node: &BTreeNode) -> ([u8; KEY_SIZE], [u8; VALUE_SIZE], u16) {
        let mut curr = node.clone();
        while !curr.is_leaf {
            let curr_id = curr.children[curr.num_keys];
            curr = self.read_node(curr_id);
        }
        (curr.keys[curr.num_keys - 1], curr.values[curr.num_keys - 1], curr.value_lengths[curr.num_keys - 1])
    }

    fn get_successor(&mut self, node: &BTreeNode) -> ([u8; KEY_SIZE], [u8; VALUE_SIZE], u16) {
        let mut curr = node.clone();
        while !curr.is_leaf {
            let curr_id = curr.children[0];
            curr = self.read_node(curr_id);
        }
        (curr.keys[0], curr.values[0], curr.value_lengths[0])
    }

    fn merge_nodes(&mut self, parent_id: u32, parent: &mut BTreeNode, i: usize, left_id: u32, left: &mut BTreeNode, right_id: u32, right: &mut BTreeNode) {
        left.keys[left.num_keys] = parent.keys[i];
        left.values[left.num_keys] = parent.values[i];
        left.value_lengths[left.num_keys] = parent.value_lengths[i];
        left.num_keys += 1;

        for j in 0..right.num_keys {
            left.keys[left.num_keys] = right.keys[j];
            left.values[left.num_keys] = right.values[j];
            left.value_lengths[left.num_keys] = right.value_lengths[j];
            left.num_keys += 1;
        }

        if !left.is_leaf {
            for j in 0..=right.num_keys {
                left.children[left.num_keys - right.num_keys + j] = right.children[j];
            }
        }

        for j in i + 1..parent.num_keys {
            parent.keys[j - 1] = parent.keys[j];
            parent.values[j - 1] = parent.values[j];
            parent.value_lengths[j - 1] = parent.value_lengths[j];
            parent.children[j] = parent.children[j + 1];
        }
        parent.children[parent.num_keys] = 0;
        parent.num_keys -= 1;

        self.write_node(parent_id, parent);
        self.write_node(left_id, left);
        self.free_page(right_id);
    }

    fn borrow_from_prev(&mut self, parent_id: u32, parent: &mut BTreeNode, i: usize) {
        let child_id = parent.children[i];
        let mut child = self.read_node(child_id);
        let left_id = parent.children[i - 1];
        let mut left = self.read_node(left_id);

        for j in (0..child.num_keys).rev() {
            child.keys[j + 1] = child.keys[j];
            child.values[j + 1] = child.values[j];
            child.value_lengths[j + 1] = child.value_lengths[j];
        }
        if !child.is_leaf {
            for j in (0..=child.num_keys).rev() {
                child.children[j + 1] = child.children[j];
            }
        }

        child.keys[0] = parent.keys[i - 1];
        child.values[0] = parent.values[i - 1];
        child.value_lengths[0] = parent.value_lengths[i - 1];
        if !child.is_leaf {
            child.children[0] = left.children[left.num_keys];
        }
        child.num_keys += 1;

        parent.keys[i - 1] = left.keys[left.num_keys - 1];
        parent.values[i - 1] = left.values[left.num_keys - 1];
        parent.value_lengths[i - 1] = left.value_lengths[left.num_keys - 1];
        left.num_keys -= 1;

        self.write_node(parent_id, parent);
        self.write_node(child_id, &child);
        self.write_node(left_id, &left);
    }

    fn borrow_from_next(&mut self, parent_id: u32, parent: &mut BTreeNode, i: usize) {
        let child_id = parent.children[i];
        let mut child = self.read_node(child_id);
        let right_id = parent.children[i + 1];
        let mut right = self.read_node(right_id);

        child.keys[child.num_keys] = parent.keys[i];
        child.values[child.num_keys] = parent.values[i];
        child.value_lengths[child.num_keys] = parent.value_lengths[i];
        if !child.is_leaf {
            child.children[child.num_keys + 1] = right.children[0];
        }
        child.num_keys += 1;

        parent.keys[i] = right.keys[0];
        parent.values[i] = right.values[0];
        parent.value_lengths[i] = right.value_lengths[0];

        for j in 1..right.num_keys {
            right.keys[j - 1] = right.keys[j];
            right.values[j - 1] = right.values[j];
            right.value_lengths[j - 1] = right.value_lengths[j];
        }
        if !right.is_leaf {
            for j in 1..=right.num_keys {
                right.children[j - 1] = right.children[j];
            }
        }
        right.num_keys -= 1;

        self.write_node(parent_id, parent);
        self.write_node(child_id, &child);
        self.write_node(right_id, &right);
    }

    pub fn iter(&mut self, start_key: [u8; KEY_SIZE]) -> RangeIterator {
        let mut stack = Vec::new();
        let mut current_id = self.meta.root_page_id;
        
        loop {
            let node = self.read_node(current_id);
            let mut i = 0;
            while i < node.num_keys && start_key > node.keys[i] {
                i += 1;
            }

            stack.push((current_id, i));

            if node.is_leaf {
                break;
            } else {
                current_id = node.children[i];
            }
        }

        RangeIterator { tree: self, stack, current_node: None }
    }

    pub fn page_count(&self) -> usize {
        self.meta.next_free_page as usize
    }

    pub fn free_page_count(&mut self) -> usize {
        let mut count = 0;
        let mut curr = self.meta.first_free_page;
        while curr != 0 {
            count += 1;
            if let Ok(guard) = self.bpm.fetch_page_read_only(curr as usize) {
                curr = u32::from_le_bytes(guard.data()[0..4].try_into().unwrap());
            } else {
                break;
            }
        }
        count
    }

    pub fn capacity_remaining(&self, shard_max_pages: usize) -> usize {
        shard_max_pages.saturating_sub(self.meta.next_free_page as usize)
    }
}

pub struct RangeIterator<'tree> {
    tree: &'tree mut BTree,
    stack: Vec<(u32, usize)>,
    current_node: Option<(u32, BTreeNode)>,
}

impl<'tree> RangeIterator<'tree> {
    pub fn next_kv(&mut self) -> Option<([u8; KEY_SIZE], Vec<u8>)> {
        while let Some((node_id, index)) = self.stack.pop() {
            let node = if let Some((cached_id, cached_node)) = &self.current_node {
                if *cached_id == node_id {
                    cached_node
                } else {
                    let new_node = self.tree.read_node(node_id);
                    self.current_node = Some((node_id, new_node));
                    &self.current_node.as_ref().unwrap().1
                }
            } else {
                let new_node = self.tree.read_node(node_id);
                self.current_node = Some((node_id, new_node));
                &self.current_node.as_ref().unwrap().1
            };
            
            if index < node.num_keys {
                let len = node.value_lengths[index] as usize;
                let kv = (node.keys[index], node.values[index][0..len].to_vec());
                
                self.stack.push((node_id, index + 1));
                
                if !node.is_leaf {
                    let mut curr_id = node.children[index + 1];
                    loop {
                        let curr_node = self.tree.read_node(curr_id);
                        self.stack.push((curr_id, 0));
                        if curr_node.is_leaf {
                            break;
                        }
                        curr_id = curr_node.children[0];
                    }
                }
                
                return Some(kv);
            }
        }
        None
    }

    pub fn next_key(&mut self) -> Option<[u8; KEY_SIZE]> {
        while let Some((node_id, index)) = self.stack.pop() {
            let node = self.tree.read_node(node_id);
            
            if index < node.num_keys {
                let key = node.keys[index];
                
                self.stack.push((node_id, index + 1));
                
                if !node.is_leaf {
                    let mut curr_id = node.children[index + 1];
                    loop {
                        let curr_node = self.tree.read_node(curr_id);
                        self.stack.push((curr_id, 0));
                        if curr_node.is_leaf {
                            break;
                        }
                        curr_id = curr_node.children[0];
                    }
                }
                
                return Some(key);
            }
        }
        None
    }
}

impl BTree {
    pub fn count_prefixed(&mut self, prefix: u8) -> usize {
        let mut count = 0;
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = prefix;
        let mut it = self.iter(start_key);
        while let Some(k) = it.next_key() {
            if k[0] != prefix {
                break;
            }
            count += 1;
            if count % 1000 == 0 {
                crate::watchdog::kick();
            }
        }
        count
    }
}
