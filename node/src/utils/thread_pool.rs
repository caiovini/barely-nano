use alloc::string::String;
use alloc::sync::Arc;
use alloc::boxed::Box;
use spin::{ Mutex };
use core::time::Duration;

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
}

struct ThreadPoolData<T: TimerStrategy> {
    timer: Timer<T>,
}

impl<T: TimerStrategy> ThreadPoolData<T> {
    fn push_task(&self, callback: Box<dyn FnOnce() + Send>) {
        // Stub: In bare-metal, execute directly or put in a task queue
        callback();
    }
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
            data: Arc::new(Mutex::new(Some(ThreadPoolData {
                timer,
            }))),
        }
    }

    #[cfg(feature = "output_tracking")]
    pub fn track(&self) -> Arc<OutputTrackerMt<TimerEvent>> {
        self.data.lock().as_ref().unwrap().timer.track()
    }
}

impl<T: TimerStrategy + 'static> ThreadPool for ThreadPoolImpl<T> {
    fn post(&self, callback: Box<dyn FnOnce() + Send>) {
        let stopped_guard = self.stopped.lock();
        if !*stopped_guard {
            let data_guard = self.data.lock();
            drop(stopped_guard);
            if let Some(data) = data_guard.as_ref() {
                data.push_task(callback);
            }
        }
    }

    fn post_delayed(&self, delay: Duration, callback: Box<dyn FnOnce() + Send>) {
        let stopped_guard = self.stopped.lock();
        if !*stopped_guard {
            let data_guard = self.data.lock();
            drop(stopped_guard);
            let mut option_callback = Some(callback);
            let data_clone = self.data.clone();
            let stopped_clone = self.stopped.clone();
            if let Some(data) = data_guard.as_ref() {
                data.timer.schedule_with_delay(
                    delay,
                    move || {
                        if let Some(cb) = option_callback.take() {
                            let stopped_guard = stopped_clone.lock();
                            if !*stopped_guard {
                                let data_guard = data_clone.lock();
                                drop(stopped_guard);
                                if let Some(data) = data_guard.as_ref() {
                                    data.push_task(cb);
                                }
                            }
                        }
                    },
                );
            }
        }
    }

    fn stop(&self) {
        let mut stopped_guard = self.stopped.lock();
        if !*stopped_guard {
            let mut data_guard = self.data.lock();
            *stopped_guard = true;
            drop(stopped_guard);
            if let Some(_data) = data_guard.take() {
                drop(data_guard);
            }
        }
    }

    fn num_queued_tasks(&self) -> usize {
        0
    }
}

impl<T: TimerStrategy + 'static> Drop for ThreadPoolImpl<T> {
    fn drop(&mut self) {
        self.stop()
    }
}
