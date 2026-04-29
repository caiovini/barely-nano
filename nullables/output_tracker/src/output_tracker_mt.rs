use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

// Multi threaded output tracker
pub struct OutputTrackerMt<T: Clone + 'static> {
    output: Mutex<Vec<T>>,
}

impl<T: Clone + 'static> OutputTrackerMt<T> {
    pub fn new() -> Self {
        Self {
            output: Mutex::new(Vec::new()),
        }
    }

    pub fn add(&self, t: T) {
        self.output.lock().push(t);
    }

    pub fn output(&self) -> Vec<T> {
        self.output.lock().clone()
    }

    pub fn clear(&self) {
        self.output.lock().clear();
    }
}

impl<T> Default for OutputTrackerMt<T>
where
    T: Clone + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

pub struct OutputListenerMt<T: Clone + 'static> {
    trackers: Mutex<Vec<Weak<OutputTrackerMt<T>>>>,
    count: AtomicUsize,
}

impl<T: Clone + 'static> OutputListenerMt<T> {
    pub fn new() -> Self {
        Self {
            trackers: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        }
    }

    pub fn is_tracked(&self) -> bool {
        self.count.load(Ordering::Relaxed) > 0
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<T>> {
        let tracker = Arc::new(OutputTrackerMt::new());
        let mut guard = self.trackers.lock();
        guard.push(Arc::downgrade(&tracker));
        self.count.store(guard.len(), Ordering::SeqCst);
        tracker
    }

    pub fn emit(&self, t: T) {
        if self.count.load(Ordering::SeqCst) == 0 {
            return;
        }

        let mut guard = self.trackers.lock();
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
            self.count.store(guard.len(), Ordering::SeqCst);
        }
    }

    pub fn tracker_count(&self) -> usize {
        self.trackers.lock().len()
    }
}

impl<T> Default for OutputListenerMt<T>
where
    T: Clone + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
