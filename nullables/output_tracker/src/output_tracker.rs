use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::cell::RefCell;

pub struct OutputTracker<T: Clone + 'static> {
    output: RefCell<Vec<T>>,
}

impl<T: Clone + 'static> OutputTracker<T> {
    pub fn new() -> Self {
        Self {
            output: RefCell::new(Vec::new()),
        }
    }

    pub fn add(&self, t: T) {
        self.output.borrow_mut().push(t);
    }

    pub fn output(&self) -> Vec<T> {
        self.output.borrow().clone()
    }
}

impl<T> Default for OutputTracker<T>
where
    T: Clone + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

pub struct OutputListener<T: Clone + 'static> {
    trackers: RefCell<Vec<Weak<OutputTracker<T>>>>,
}

impl<T: Clone + 'static> OutputListener<T> {
    pub fn new() -> Self {
        Self {
            trackers: RefCell::new(Vec::new()),
        }
    }

    pub fn track(&self) -> Rc<OutputTracker<T>> {
        let tracker = Rc::new(OutputTracker::new());
        self.trackers.borrow_mut().push(Rc::downgrade(&tracker));
        tracker
    }

    pub fn emit(&self, t: T) {
        let mut guard = self.trackers.borrow_mut();
        let mut should_clean = false;
        for tracker in guard.iter() {
            if let Some(tracker) = tracker.upgrade() {
                tracker.add(t.clone());
            } else {
                should_clean = true;
            }
        }

        if should_clean {
            guard.retain(|t| t.strong_count() > 0);
        }
    }

    pub fn is_tracked(&self) -> bool {
        self.tracker_count() > 0
    }

    pub fn tracker_count(&self) -> usize {
        self.trackers.borrow().len()
    }
}

impl<T> Default for OutputListener<T>
where
    T: Clone + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
