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

/// A waker that puts the task back into the executor's run queue.
struct TaskWaker {
    task: Arc<Task>,
    queue: Arc<Mutex<VecDeque<Arc<Task>>>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.task.is_queued.swap(true, Ordering::SeqCst) {
            self.queue.lock().push_back(self.task.clone());
        }
    }
}

/// A minimal single-threaded executor for bare-metal `#![no_std]`.
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
        while let Some(task) = {
            let mut queue = self.run_queue.lock();
            queue.pop_front()
        } {
            // Unmark as queued so a waker can re-queue it.
            task.is_queued.store(false, Ordering::SeqCst);

            let mut future_slot = task.future.lock();
            if let Some(mut future) = future_slot.take() {
                let waker = Arc::new(TaskWaker {
                    task: task.clone(),
                    queue: self.run_queue.clone(),
                });
                let waker = Waker::from(waker);
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
