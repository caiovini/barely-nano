use crate::storage::bpm::{BufferPool, BLOCK_SIZE};
extern crate alloc;
use alloc::vec::Vec;

pub const MAX_KEYS: usize = 6;
pub const MIN_KEYS: usize = 2;
pub const KEY_SIZE: usize = 65; // 1 byte table ID + 64 bytes key
pub const VALUE_SIZE: usize = 512;

#[derive(Clone, Debug)]
pub struct BTreeNode {
    pub is_leaf: bool,
    pub num_keys: usize,
    pub keys: [[u8; KEY_SIZE]; MAX_KEYS],
    pub values: [[u8; VALUE_SIZE]; MAX_KEYS],
    pub children: [u32; MAX_KEYS + 1],
}

impl BTreeNode {
    pub fn new(is_leaf: bool) -> Self {
        Self {
            is_leaf,
            num_keys: 0,
            keys: [[0; KEY_SIZE]; MAX_KEYS],
            values: [[0; VALUE_SIZE]; MAX_KEYS],
            children: [0; MAX_KEYS + 1],
        }
    }

    pub fn from_bytes(data: &[u8; BLOCK_SIZE]) -> Self {
        let is_leaf = data[0] != 0;
        let mut num_keys = u16::from_le_bytes([data[1], data[2]]) as usize;
        if num_keys > MAX_KEYS {
            num_keys = MAX_KEYS;
        }
        
        let mut keys = [[0; KEY_SIZE]; MAX_KEYS];
        let mut values = [[0; VALUE_SIZE]; MAX_KEYS];
        let mut children = [0; MAX_KEYS + 1];

        let mut offset = 4;
        for i in 0..num_keys {
            keys[i].copy_from_slice(&data[offset..offset + KEY_SIZE]);
            offset += KEY_SIZE;
        }

        offset = 4 + MAX_KEYS * KEY_SIZE;
        // In a B-tree, values are present in all nodes.
        // We always read values for all keys.
        for i in 0..num_keys {
            values[i].copy_from_slice(&data[offset..offset + VALUE_SIZE]);
            offset += VALUE_SIZE;
        }
        
        // Children are only present in internal nodes.
        // We read children if it's not a leaf.
        offset = 4 + MAX_KEYS * KEY_SIZE + MAX_KEYS * VALUE_SIZE;
        if !is_leaf {
            for i in 0..=num_keys {
                children[i] = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
            }
        }

        Self { is_leaf, num_keys, keys, values, children }
    }

    pub fn to_bytes(&self, data: &mut [u8; BLOCK_SIZE]) {
        data.fill(0);
        data[0] = if self.is_leaf { 1 } else { 0 };
        let nk_bytes = (self.num_keys as u16).to_le_bytes();
        data[1] = nk_bytes[0];
        data[2] = nk_bytes[1];

        let mut offset = 4;
        for i in 0..self.num_keys {
            data[offset..offset + KEY_SIZE].copy_from_slice(&self.keys[i]);
            offset += KEY_SIZE;
        }

        offset = 4 + MAX_KEYS * KEY_SIZE;
        for i in 0..self.num_keys {
            data[offset..offset + VALUE_SIZE].copy_from_slice(&self.values[i]);
            offset += VALUE_SIZE;
        }
        
        offset = 4 + MAX_KEYS * KEY_SIZE + MAX_KEYS * VALUE_SIZE;
        if !self.is_leaf {
            for i in 0..=self.num_keys {
                data[offset..offset + 4].copy_from_slice(&self.children[i].to_le_bytes());
                offset += 4;
            }
        }
    }
}

pub struct MetaData {
    pub root_page_id: u32,
    pub next_free_page: u32,
}

impl MetaData {
    pub fn from_bytes(data: &[u8; BLOCK_SIZE]) -> Self {
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != 0xBEEFCAFE {
            // Uninitialized
            return Self { root_page_id: 0, next_free_page: 0 };
        }
        let root_page_id = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let next_free_page = u32::from_le_bytes(data[8..12].try_into().unwrap());
        Self { root_page_id, next_free_page }
    }

    pub fn to_bytes(&self, data: &mut [u8; BLOCK_SIZE]) {
        data.fill(0);
        data[0..4].copy_from_slice(&0xBEEFCAFE_u32.to_le_bytes());
        data[4..8].copy_from_slice(&self.root_page_id.to_le_bytes());
        data[8..12].copy_from_slice(&self.next_free_page.to_le_bytes());
    }
}

pub struct BTree<'a> {
    pub bpm: BufferPool<'a>,
    meta: MetaData,
}

impl<'a> BTree<'a> {
    pub fn new(mut bpm: BufferPool<'a>) -> Self {
        let meta = {
            let frame = bpm.fetch_page(0).unwrap();
            let m = MetaData::from_bytes(&frame.data);
            tracing::debug!("BTree: opening tree. root_page_id={}, next_free={}", m.root_page_id, m.next_free_page);
            m
        };
        let mut tree = Self { bpm, meta };
        
        if tree.meta.root_page_id == 0 {
            // Initialize new tree
            let root_id = tree.allocate_page();
            let root_node = BTreeNode::new(true);
            tree.write_node(root_id, &root_node);
            tree.meta.root_page_id = root_id;
            tree.save_meta();
            let _ = tree.flush();
        }
        tree
    }

    fn save_meta(&mut self) {
        let frame = self.bpm.fetch_page(0).unwrap();
        self.meta.to_bytes(&mut frame.data);
        frame.is_dirty = true;
    }

    pub fn flush(&mut self) -> Result<(), virtio_drivers::Error> {
        self.bpm.flush_all()
    }

    pub fn wipe(&mut self) {
        self.meta = MetaData {
            root_page_id: 0,
            next_free_page: 0,
        };
        self.save_meta();
        let _ = self.flush();
    }

    pub fn allocate_page(&mut self) -> u32 {
        if self.meta.next_free_page < 1 {
            self.meta.next_free_page = 1;
        }
        let id = self.meta.next_free_page;
        self.meta.next_free_page += 1;
        self.save_meta();
        id
    }

    pub fn read_page(&mut self, page_id: u32) -> [u8; BLOCK_SIZE] {
        let frame = self.bpm.fetch_page(page_id as usize).unwrap();
        frame.data
    }

    pub fn write_page(&mut self, page_id: u32, data: &[u8; BLOCK_SIZE]) {
        let frame = self.bpm.fetch_page(page_id as usize).unwrap();
        frame.data.copy_from_slice(data);
        frame.is_dirty = true;
    }

    fn read_node(&mut self, page_id: u32) -> BTreeNode {
        let frame = self.bpm.fetch_page(page_id as usize).unwrap();
        BTreeNode::from_bytes(&frame.data)
    }

    fn write_node(&mut self, page_id: u32, node: &BTreeNode) {
        let frame = self.bpm.fetch_page(page_id as usize).unwrap();
        node.to_bytes(&mut frame.data);
        frame.is_dirty = true;
    }

    pub fn search(&mut self, key: &[u8; KEY_SIZE]) -> Option<[u8; VALUE_SIZE]> {
        let mut current_id = self.meta.root_page_id;
        
        loop {
            let node = self.read_node(current_id);
            let mut i = 0;
            while i < node.num_keys && key > &node.keys[i] {
                i += 1;
            }

            if i < node.num_keys && &node.keys[i] == key {
                return Some(node.values[i]);
            }

            if node.is_leaf {
                return None;
            } else {
                current_id = node.children[i];
            }
        }
    }

    pub fn insert(&mut self, key: [u8; KEY_SIZE], value: [u8; VALUE_SIZE]) {
        let root_id = self.meta.root_page_id;
        let mut root = self.read_node(root_id);

        if root.num_keys == MAX_KEYS {
            let new_root_id = self.allocate_page();
            let mut new_root = BTreeNode::new(false);
            new_root.children[0] = root_id;
            
            self.split_child(&mut new_root, 0, root_id, &mut root);
            self.write_node(new_root_id, &new_root);
            
            self.meta.root_page_id = new_root_id;
            self.save_meta();
            
            self.insert_non_full(new_root_id, &mut new_root, key, value);
        } else {
            self.insert_non_full(root_id, &mut root, key, value);
        }
    }

    fn split_child(&mut self, parent: &mut BTreeNode, i: usize, child_id: u32, child: &mut BTreeNode) {
        let new_child_id = self.allocate_page();
        let mut new_child = BTreeNode::new(child.is_leaf);
        
        let t = MAX_KEYS / 2; // Midpoint
        new_child.num_keys = MAX_KEYS - t - 1;

        // Copy upper half keys and values to new child
        for j in 0..new_child.num_keys {
            new_child.keys[j] = child.keys[j + t + 1];
            new_child.values[j] = child.values[j + t + 1];
        }

        if !child.is_leaf {
            for j in 0..=new_child.num_keys {
                new_child.children[j] = child.children[j + t + 1];
            }
        }

        // Midpoint key moves up to parent
        let mid_key = child.keys[t];
        let mid_val = child.values[t];
        child.num_keys = t;

        // Shift parent children to make room
        for j in (i + 1..=parent.num_keys).rev() {
            parent.children[j + 1] = parent.children[j];
        }
        parent.children[i + 1] = new_child_id;

        // Shift parent keys to make room
        for j in (i..parent.num_keys).rev() {
            parent.keys[j + 1] = parent.keys[j];
            parent.values[j + 1] = parent.values[j];
        }
        parent.keys[i] = mid_key;
        parent.values[i] = mid_val;
        parent.num_keys += 1;

        self.write_node(child_id, child);
        self.write_node(new_child_id, &new_child);
    }

    fn insert_non_full(&mut self, node_id: u32, node: &mut BTreeNode, key: [u8; KEY_SIZE], value: [u8; VALUE_SIZE]) {
        let mut i = node.num_keys as isize - 1;

        if node.is_leaf {
            while i >= 0 && node.keys[i as usize] > key {
                node.keys[i as usize + 1] = node.keys[i as usize];
                node.values[i as usize + 1] = node.values[i as usize];
                i -= 1;
            }
            
            if i >= 0 && node.keys[i as usize] == key {
                // Key exists, update value
                node.values[i as usize] = value;
            } else {
                // New key
                node.keys[(i + 1) as usize] = key;
                node.values[(i + 1) as usize] = value;
                node.num_keys += 1;
            }
            self.write_node(node_id, node);
        } else {
            while i >= 0 && node.keys[i as usize] > key {
                i -= 1;
            }
            
            if i >= 0 && node.keys[i as usize] == key {
                // Key exists in internal node, update value and return
                node.values[i as usize] = value;
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
                    node.values[i as usize] = value;
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
                }
                node.num_keys -= 1;
                self.write_node(node_id, &node);
            } else {
                let left_child_id = node.children[i];
                let right_child_id = node.children[i + 1];
                let mut left_child = self.read_node(left_child_id);

                if left_child.num_keys > MIN_KEYS {
                    let (pred_key, pred_val) = self.get_predecessor(&left_child);
                    node.keys[i] = pred_key;
                    node.values[i] = pred_val;
                    self.write_node(node_id, &node);
                    self.delete_recursive(left_child_id, &pred_key);
                } else {
                    let mut right_child = self.read_node(right_child_id);
                    if right_child.num_keys > MIN_KEYS {
                        let (succ_key, succ_val) = self.get_successor(&right_child);
                        node.keys[i] = succ_key;
                        node.values[i] = succ_val;
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

    fn get_predecessor(&mut self, node: &BTreeNode) -> ([u8; KEY_SIZE], [u8; VALUE_SIZE]) {
        let mut curr = node.clone();
        while !curr.is_leaf {
            let curr_id = curr.children[curr.num_keys];
            curr = self.read_node(curr_id);
        }
        (curr.keys[curr.num_keys - 1], curr.values[curr.num_keys - 1])
    }

    fn get_successor(&mut self, node: &BTreeNode) -> ([u8; KEY_SIZE], [u8; VALUE_SIZE]) {
        let mut curr = node.clone();
        while !curr.is_leaf {
            let curr_id = curr.children[0];
            curr = self.read_node(curr_id);
        }
        (curr.keys[0], curr.values[0])
    }

    fn merge_nodes(&mut self, parent_id: u32, parent: &mut BTreeNode, i: usize, left_id: u32, left: &mut BTreeNode, _right_id: u32, right: &mut BTreeNode) {
        left.keys[left.num_keys] = parent.keys[i];
        left.values[left.num_keys] = parent.values[i];
        left.num_keys += 1;

        for j in 0..right.num_keys {
            left.keys[left.num_keys] = right.keys[j];
            left.values[left.num_keys] = right.values[j];
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
            parent.children[j] = parent.children[j + 1];
        }
        parent.children[parent.num_keys] = 0;
        parent.num_keys -= 1;

        self.write_node(parent_id, parent);
        self.write_node(left_id, left);
    }

    fn borrow_from_prev(&mut self, parent_id: u32, parent: &mut BTreeNode, i: usize) {
        let child_id = parent.children[i];
        let mut child = self.read_node(child_id);
        let left_id = parent.children[i - 1];
        let mut left = self.read_node(left_id);

        for j in (0..child.num_keys).rev() {
            child.keys[j + 1] = child.keys[j];
            child.values[j + 1] = child.values[j];
        }
        if !child.is_leaf {
            for j in (0..=child.num_keys).rev() {
                child.children[j + 1] = child.children[j];
            }
        }

        child.keys[0] = parent.keys[i - 1];
        child.values[0] = parent.values[i - 1];
        if !child.is_leaf {
            child.children[0] = left.children[left.num_keys];
        }
        child.num_keys += 1;

        parent.keys[i - 1] = left.keys[left.num_keys - 1];
        parent.values[i - 1] = left.values[left.num_keys - 1];
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
        if !child.is_leaf {
            child.children[child.num_keys + 1] = right.children[0];
        }
        child.num_keys += 1;

        parent.keys[i] = right.keys[0];
        parent.values[i] = right.values[0];

        for j in 1..right.num_keys {
            right.keys[j - 1] = right.keys[j];
            right.values[j - 1] = right.values[j];
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

    pub fn iter<'tree>(&'tree mut self, start_key: [u8; KEY_SIZE]) -> RangeIterator<'tree, 'a> {
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

        RangeIterator { tree: self, stack }
    }
}

pub struct RangeIterator<'tree, 'bpm> {
    tree: &'tree mut BTree<'bpm>,
    stack: Vec<(u32, usize)>,
}

impl<'tree, 'bpm> RangeIterator<'tree, 'bpm> {
    pub fn next_kv(&mut self) -> Option<([u8; KEY_SIZE], [u8; VALUE_SIZE])> {
        while let Some((node_id, index)) = self.stack.pop() {
            let node = self.tree.read_node(node_id);
            
            if index < node.num_keys {
                let kv = (node.keys[index], node.values[index]);
                
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
}
