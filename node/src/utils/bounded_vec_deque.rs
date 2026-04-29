use alloc::collections::VecDeque;

pub struct BoundedVecDeque<T> {
    queue: VecDeque<T>,
    capacity: usize,
}

impl<T> BoundedVecDeque<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push_back(&mut self, item: T) -> Option<T> {
        let mut popped = None;
        if self.queue.len() >= self.capacity {
            popped = self.queue.pop_front();
        }
        self.queue.push_back(item);
        popped
    }

    pub fn push_front(&mut self, item: T) -> Option<T> {
        let mut popped = None;
        if self.queue.len() >= self.capacity {
            popped = self.queue.pop_back();
        }
        self.queue.push_front(item);
        popped
    }

    pub fn pop_back(&mut self) -> Option<T> {
        self.queue.pop_back()
    }

    pub fn pop_front(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }

    pub fn iter(&self) -> alloc::collections::vec_deque::Iter<'_, T> {
        self.queue.iter()
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.queue.get(index)
    }
}

impl<T> core::ops::Deref for BoundedVecDeque<T> {
    type Target = VecDeque<T>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}
impl<T> Default for BoundedVecDeque<T> {
    fn default() -> Self {
        Self::new(0)
    }
}
