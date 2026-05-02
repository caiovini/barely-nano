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
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};
use spin::Mutex;

/// A type-erased future that outputs `()`.
type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A task submitted to the executor.
struct Task {
    future: Mutex<Option<TaskFuture>>,
    is_queued: AtomicBool,
}

impl Task {
    fn new(future: impl Future<Output = ()> + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            future: Mutex::new(Some(Box::pin(future))),
            is_queued: AtomicBool::new(true),
        })
    }
}

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.is_queued.swap(true, Ordering::SeqCst) {
            // tracing::info!("[boot] Task wake: re-queueing task");
            if let Some(executor) = EXECUTOR.lock().as_ref() {
                executor.run_queue.lock().push_back(self.clone());
            }
        }
    }
}

/// A minimal single-threaded executor for bare-metal `#![no_std]`.
#[derive(Clone)]
pub struct Executor {
    run_queue: Arc<Mutex<VecDeque<Arc<Task>>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            run_queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Spawns a new asynchronous task.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let task = Task::new(future);
        self.run_queue.lock().push_back(task);
    }

    /// Runs all currently queued tasks until the queue is empty.
    /// Does not sleep; returns immediately when no tasks are ready.
    pub fn run_ready_tasks(&self) {
        let mut count = self.run_queue.lock().len();
        while count > 0 {
            let task = {
                let mut queue = self.run_queue.lock();
                queue.pop_front()
            };
            
            let Some(task) = task else { break; };
            count -= 1;
            
            // Unmark as queued so a waker can re-queue it.
            task.is_queued.store(false, Ordering::SeqCst);

            let mut future_slot = task.future.lock();
            if let Some(mut future) = future_slot.take() {
                let waker = Waker::from(task.clone());
                let mut context = Context::from_waker(&waker);

                match future.as_mut().poll(&mut context) {
                    Poll::Ready(()) => {
                        // Task is complete, future is dropped.
                    }
                    Poll::Pending => {
                        // Task is pending, put it back.
                        *future_slot = Some(future);
                    }
                }
            }
        }
    }

    pub fn task_count(&self) -> usize {
        self.run_queue.lock().len()
    }
}

/// A global singleton for spawning tasks easily.
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
