#![allow(dead_code)]

extern crate alloc;
use alloc::{
    boxed::Box,
    collections::VecDeque,
    sync::Arc,
    task::Wake,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};
use spin::Mutex;

/// Task states
const STATE_IDLE: u8 = 0;
const STATE_QUEUED: u8 = 1;
const STATE_SUSPENDED: u8 = 2;

/// A type-erased future that outputs `()`.
type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A task submitted to the executor.
pub struct Task {
    future: Mutex<Option<TaskFuture>>,
    state: AtomicU8,
}

impl Task {
    fn new(future: impl Future<Output = ()> + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            future: Mutex::new(Some(Box::pin(future))),
            state: AtomicU8::new(STATE_QUEUED),
        })
    }
}

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // Only queue if it's currently IDLE or SUSPENDED
        let prev = self.state.swap(STATE_QUEUED, Ordering::SeqCst);
        if prev != STATE_QUEUED {
            if let Some(executor) = EXECUTOR.lock().as_ref() {
                executor.run_queue.lock().push_back(self.clone());
            }
        }
    }
}

/// A minimal multicore-safe executor for bare-metal.
#[derive(Clone)]
pub struct Executor {
    pub run_queue: Arc<Mutex<VecDeque<Arc<Task>>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            run_queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = Arc::new(Task {
            future: Mutex::new(Some(Box::pin(future))),
            state: AtomicU8::new(STATE_QUEUED),
        });
        self.run_queue.lock().push_back(task);
    }

    pub fn run_ready_tasks(&self) {
        while self.run_once() {}
    }

    pub fn run_once(&self) -> bool {
        let task = {
            let mut run_queue = self.run_queue.lock();
            run_queue.pop_front()
        };

        let Some(task) = task else { return false; };

        // Mark as IDLE before polling. If a waker triggers during poll,
        // it will transition it back to QUEUED.
        task.state.store(STATE_IDLE, Ordering::SeqCst);

        let waker = Waker::from(task.clone());
        let mut cx = Context::from_waker(&waker);

        let mut future_guard = task.future.lock();
        let future = match future_guard.as_mut() {
            Some(f) => f,
            None => return true,
        };

        let poll = future.as_mut().poll(&mut cx);

        match poll {
            Poll::Ready(_) => {
                *future_guard = None;
            }
            Poll::Pending => {
                // If no waker was triggered during poll, the state is still IDLE.
                // Transition to SUSPENDED.
                let _ = task.state.compare_exchange(
                    STATE_IDLE,
                    STATE_SUSPENDED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            }
        }
        true
    }

    pub fn wake_all(&self) {
        // This is only used as a fallback if hardware events don't trigger wakers correctly.
    }

    pub fn task_count(&self) -> usize {
        self.run_queue.lock().len()
    }
}

pub static EXECUTOR: Mutex<Option<Executor>> = Mutex::new(None);

pub fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    if let Some(exec) = EXECUTOR.lock().as_ref() {
        exec.spawn(future);
    } else {
        panic!("Executor not initialized");
    }
}

pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

pub struct Sleep {
    target_ticks: u64,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let now = crate::clint::mtime();
        if now >= self.target_ticks {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

pub fn sleep(duration: core::time::Duration) -> Sleep {
    let ticks = (duration.as_micros() as u64) * (crate::clint::FREQ / 1_000_000);
    Sleep {
        target_ticks: crate::clint::mtime() + ticks,
    }
}

pub const FREQ: u64 = crate::clint::FREQ;
