use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use spin::Mutex;

#[cfg(feature = "output_tracking")]
use super::timer::TimerEvent;
#[cfg(feature = "output_tracking")]
use rsnano_output_tracker::OutputTrackerMt;

use super::{NullTimer, Timer, TimerStrategy, TimerWrapper};

pub trait ThreadPool: Send + Sync {
    fn post(&self, callback: Box<dyn FnOnce() + Send>);
    fn post_delayed(&self, delay: Duration, callback: Box<dyn FnOnce() + Send>);
    fn stop(&self);
    fn num_queued_tasks(&self) -> usize;
}

pub struct ThreadPoolImpl<T: TimerStrategy + 'static = TimerWrapper> {
    data: Arc<Mutex<Option<ThreadPoolData<T>>>>,
    stopped: Arc<Mutex<bool>>,
    queued_tasks: Arc<AtomicUsize>,
}

struct ThreadPoolData<T: TimerStrategy> {
    timer: Timer<T>,
}

impl ThreadPoolImpl<TimerWrapper> {
    pub fn create(_num_threads: usize, _thread_name: impl Into<String>) -> Self {
        Self::new(0, String::from(""), Timer::new())
    }

    pub fn new_test_instance() -> Self {
        Self::create(2, "test pool")
    }
}

impl ThreadPoolImpl<NullTimer> {
    pub fn new_null() -> Self {
        Self::new(1, String::from("nulled thread pool"), Timer::new_null())
    }
}

impl<T: TimerStrategy> ThreadPoolImpl<T> {
    pub fn new(_num_threads: usize, _thread_name: String, timer: Timer<T>) -> Self {
        Self {
            stopped: Arc::new(Mutex::new(false)),
            data: Arc::new(Mutex::new(Some(ThreadPoolData { timer }))),
            queued_tasks: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(feature = "output_tracking")]
    pub fn track(&self) -> Arc<OutputTrackerMt<TimerEvent>> {
        self.data.lock().as_ref().unwrap().timer.track()
    }
}

impl<T: TimerStrategy + 'static> ThreadPool for ThreadPoolImpl<T> {
    fn post(&self, callback: Box<dyn FnOnce() + Send>) {
        let can_run = {
            let stopped_guard = self.stopped.lock();
            !*stopped_guard && self.data.lock().is_some()
        };
        if !can_run {
            return;
        }
        if kernel_common::async_rt::EXECUTOR.lock().is_some() {
            let queued = self.queued_tasks.clone();
            let stopped = self.stopped.clone();
            queued.fetch_add(1, Ordering::Relaxed);
            kernel_common::async_rt::spawn(async move {
                if !*stopped.lock() {
                    callback();
                }
                queued.fetch_sub(1, Ordering::Relaxed);
            });
        } else {
            callback();
        }
    }

    fn post_delayed(&self, delay: Duration, callback: Box<dyn FnOnce() + Send>) {
        let stopped_guard = self.stopped.lock();
        if !*stopped_guard {
            let data_guard = self.data.lock();
            drop(stopped_guard);
            let mut option_callback = Some(callback);
            let stopped_clone = self.stopped.clone();
            if let Some(data) = data_guard.as_ref() {
                data.timer.schedule_with_delay(delay, move || {
                    if let Some(cb) = option_callback.take() {
                        if !*stopped_clone.lock() {
                            cb();
                        }
                    }
                });
            }
        }
    }

    fn stop(&self) {
        let mut stopped_guard = self.stopped.lock();
        if !*stopped_guard {
            let mut data_guard = self.data.lock();
            *stopped_guard = true;
            drop(stopped_guard);
            let _ = data_guard.take();
        }
    }

    fn num_queued_tasks(&self) -> usize {
        self.queued_tasks.load(Ordering::Relaxed)
    }
}
