use core::time::Duration;
use crate::utils::JoinHandle;
use alloc::string::String;
use alloc::sync::Arc;
use spin::{ Mutex };
// // // // // // // use std::{    thread::JoinHandle, time::Duration, time::Instant    }; // TODO: fix this

use rsnano_core::utils::{CancellationToken, Tickable};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_nullable_clock::SteadyClock;

// Runs a task periodically in it's own thread
pub struct TimerThread<T: Tickable + 'static> {
    thread_name: String,
    task: Mutex<Option<T>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    cancel_token: CancellationToken,
    start_listener: OutputListenerMt<TimerStartEvent>,
    clock: Arc<SteadyClock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerStartEvent {
    pub thread_name: String,
    pub start_type: TimerStartType,
    pub interval: Duration,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimerStartType {
    Start,
    StartDelayed,
    RunOnceThenStart,
}

impl<T: Tickable> TimerThread<T> {
    pub fn new(name: impl Into<String>, task: T, clock: Arc<SteadyClock>) -> Self {
        Self {
            thread_name: name.into(),
            task: Mutex::new(Some(task)),
            thread: Mutex::new(None),
            cancel_token: CancellationToken::new(),
            start_listener: OutputListenerMt::new(),
            clock,
        }
    }

    pub fn task(&self) -> spin::MutexGuard<'_, Option<T>> {
        self.task.lock()
    }

    pub fn is_running(&self) -> bool {
        self.thread.lock().is_some()
    }

    pub fn track_start(&self) -> Arc<OutputTrackerMt<TimerStartEvent>> {
        self.start_listener.track()
    }

    /// Start the thread which periodically runs the task
    pub fn start(&self, interval: Duration) {
        self.start_impl(interval, TimerStartType::Start);
    }

    /// Starts the thread and waits for the given interval before the first run
    pub fn start_delayed(&self, interval: Duration) {
        self.start_impl(interval, TimerStartType::StartDelayed);
    }

    /// Runs the task in the current thread once before the thread is started
    pub fn run_once_then_start(&self, interval: Duration) {
        self.start_impl(interval, TimerStartType::RunOnceThenStart);
    }

    fn start_impl(&self, interval: Duration, start_type: TimerStartType) {
        self.start_listener.emit(TimerStartEvent {
            thread_name: self.thread_name.clone(),
            interval,
            start_type,
        });

        let mut task = self
            .task
            .lock()
            .take()
            .expect("task already taken");

        let cancel_token = self.cancel_token.clone();

        if start_type == TimerStartType::RunOnceThenStart {
            task.tick(&cancel_token);
        }

        let _timer_loop = TimerLoop {
            interval,
            start_type,
            cancel_token,
            task,
            next_wait_duration: interval,
            clock: self.clock.clone(),
        };

//         let _handle = // /* /* std::/* /* /* thread::Builder::new()
//             .name(self.thread_name.clone())
//             .spawn(move || timer_loop.run())
//             .unwrap(); */ */ */ */ */

        *self.thread.lock() = Some(crate::utils::JoinHandle::new());
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();
        let handle = self.thread.lock().take();
        if let Some(_handle) = handle {
            // _handle.join().unwrap();
        }
    }
}

struct TimerLoop<T: Tickable> {
    interval: Duration,
    start_type: TimerStartType,
    cancel_token: CancellationToken,
    task: T,
    next_wait_duration: Duration,
    clock: Arc<SteadyClock>,
}

impl<T: Tickable> TimerLoop<T> {
    fn run(&mut self) {
        if self.start_type == TimerStartType::Start {
            self.run_one();
        }

        loop {
            if self
                .cancel_token
                .wait_for_cancellation(self.next_wait_duration)
            {
                break;
            }
            self.run_one();
        }
    }

    fn run_one(&mut self) {
        let start = self.clock.now();
        self.task.tick(&self.cancel_token);
        let elapsed = start.elapsed(self.clock.now());
        self.next_wait_duration = self.interval.saturating_sub(elapsed);
    }
}

impl<T: Tickable> Drop for TimerThread<T> {
    fn drop(&mut self) {
        self.stop();
    }
}
