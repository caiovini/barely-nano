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
const STATE_RUNNING: u8 = 3;
const STATE_NOTIFIED: u8 = 4;

/// A type-erased future that outputs `()`.
type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A task submitted to the executor.
pub struct Task {
    future: Mutex<Option<TaskFuture>>,
    state: AtomicU8,
    name: Option<&'static str>,
    assigned_hart: AtomicU8,
}

impl Task {
    fn new(future: impl Future<Output = ()> + Send + 'static, name: Option<&'static str>, hart_id: u8) -> Arc<Self> {
        Arc::new(Self {
            future: Mutex::new(Some(Box::pin(future))),
            state: AtomicU8::new(STATE_QUEUED),
            name,
            assigned_hart: AtomicU8::new(hart_id),
        })
    }
}

use core::sync::atomic::AtomicPtr;
static EXECUTOR_PTR: AtomicPtr<Executor> = AtomicPtr::new(core::ptr::null_mut());

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        loop {
            match self.state.load(Ordering::SeqCst) {
                STATE_QUEUED | STATE_NOTIFIED => return,
                STATE_RUNNING => {
                    if self
                        .state
                        .compare_exchange(
                            STATE_RUNNING,
                            STATE_NOTIFIED,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                s => {
                    if self
                        .state
                        .compare_exchange(
                            s,
                            STATE_QUEUED,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        let ptr = EXECUTOR_PTR.load(Ordering::Acquire);
                        if !ptr.is_null() {
                            let executor = unsafe { &*ptr };
                            let hart_id = self.assigned_hart.load(Ordering::Relaxed) as usize;
                            if hart_id < 8 {
                                executor.local_queues[hart_id].lock().push_back(self.clone());
                            } else {
                                executor.injector_queue.lock().push_back(self.clone());
                            }
                        }
                        return;
                    }
                }
            }
        }
    }
}

pub struct TimerEntry {
    pub target_ticks: u64,
    pub waker: Waker,
}

/// A minimal multicore-safe executor for bare-metal.
#[derive(Clone)]
pub struct Executor {
    pub local_queues: Arc<[Mutex<VecDeque<Arc<Task>>>; 8]>,
    pub injector_queue: Arc<Mutex<VecDeque<Arc<Task>>>>,
    pub timers: Arc<Mutex<alloc::vec::Vec<TimerEntry>>>,
    pub spawned_count: Arc<core::sync::atomic::AtomicUsize>,
    pub completed_count: Arc<core::sync::atomic::AtomicUsize>,
    next_spawn_hart: Arc<core::sync::atomic::AtomicU8>,
}

impl Executor {
    pub fn new() -> Self {
        let queues = [
            Mutex::new(VecDeque::new()), Mutex::new(VecDeque::new()),
            Mutex::new(VecDeque::new()), Mutex::new(VecDeque::new()),
            Mutex::new(VecDeque::new()), Mutex::new(VecDeque::new()),
            Mutex::new(VecDeque::new()), Mutex::new(VecDeque::new()),
        ];

        Self {
            local_queues: Arc::new(queues),
            injector_queue: Arc::new(Mutex::new(VecDeque::new())),
            timers: Arc::new(Mutex::new(alloc::vec::Vec::new())),
            spawned_count: Arc::new(core::sync::atomic::AtomicUsize::new(0)),
            completed_count: Arc::new(core::sync::atomic::AtomicUsize::new(0)),
            next_spawn_hart: Arc::new(core::sync::atomic::AtomicU8::new(0)),
        }
    }

    pub fn poll_timers(&self) {
        let now = crate::clint::mtime();
        if let Some(mut timers) = self.timers.try_lock() {
            if timers.is_empty() {
                return;
            }
            let mut i = 0;
            while i < timers.len() {
                if now >= timers[i].target_ticks {
                    let entry = timers.swap_remove(i);
                    entry.waker.wake();
                } else {
                    i += 1;
                }
            }
        }
    }

    #[inline(always)]
    pub fn has_pending_tasks(&self) -> bool {
        self.spawned_count.load(Ordering::Relaxed) != self.completed_count.load(Ordering::Relaxed)
    }

    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawn_named(future, None);
    }

    pub fn spawn_named<F>(&self, future: F, name: Option<&'static str>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Hart 0 is dedicated to VoteProc (VoteProcessor, VoteCacheProcessor, VoteRebroadcaster),
        // Hart 1 is dedicated to Network/RPC polling, Harts 2 & 3 are dedicated to lock-free
        // Crypto (Ed25519) & MessageProcessor draining, and Harts 5 & 6 are dedicated to
        // Sharded Ledger Commit (Ledger-B / Ledger-A), so schedule async tasks across worker harts [4, 7].
        const WORKER_HARTS: [u8; 2] = [4, 7];
        let idx = (self.next_spawn_hart.fetch_add(1, Ordering::Relaxed) as usize) % WORKER_HARTS.len();
        let hart_id = WORKER_HARTS[idx];
        
        let task = Arc::new(Task {
            future: Mutex::new(Some(Box::pin(future))),
            state: AtomicU8::new(STATE_QUEUED),
            name,
            assigned_hart: AtomicU8::new(hart_id),
        });
        self.spawned_count.fetch_add(1, Ordering::SeqCst);
        self.local_queues[hart_id as usize].lock().push_back(task);
    }

    pub fn run_ready_tasks(&self, hart_id: usize) {
        while self.run_once(hart_id) {}
    }

    pub fn run_once(&self, hart_id: usize) -> bool {
        self.poll_timers();

        let task = self.pop_task(hart_id);
        let Some(task) = task else { return false; };

        let waker = Waker::from(task.clone());
        let mut cx = Context::from_waker(&waker);

        let mut future_guard = task.future.lock();
        task.state.store(STATE_RUNNING, Ordering::SeqCst);
        let future = match future_guard.as_mut() {
            Some(f) => f,
            None => {
                task.state.store(STATE_SUSPENDED, Ordering::SeqCst);
                return true;
            }
        };

        let poll = future.as_mut().poll(&mut cx);

        match poll {
            Poll::Ready(_) => {
                *future_guard = None;
                task.state.store(STATE_SUSPENDED, Ordering::SeqCst);
                drop(future_guard);
                self.completed_count.fetch_add(1, Ordering::SeqCst);
            }
            Poll::Pending => {
                drop(future_guard);
                if task
                    .state
                    .compare_exchange(
                        STATE_RUNNING,
                        STATE_SUSPENDED,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_err()
                {
                    task.state.store(STATE_QUEUED, Ordering::SeqCst);
                    let target = (task.assigned_hart.load(Ordering::Relaxed) as usize).min(7);
                    self.local_queues[target].lock().push_back(task);
                }
            }
        }
        true
    }

    fn pop_task(&self, hart_id: usize) -> Option<Arc<Task>> {
        // 1. Try local queue
        if let Some(task) = self.local_queues[hart_id].lock().pop_front() {
            return Some(task);
        }

        // 2. Try injector queue
        if let Some(task) = self.injector_queue.lock().pop_front() {
            // Update affinity to this hart to keep it local for now
            task.assigned_hart.store(hart_id as u8, Ordering::Relaxed);
            return Some(task);
        }

        // 3. Work Stealing: Try other worker queues
        const WORKER_HARTS: [usize; 2] = [4, 7];
        for &victim_id in &WORKER_HARTS {
            if victim_id == hart_id {
                continue;
            }
            if let Some(mut victim_queue) = self.local_queues[victim_id].try_lock() {
                if let Some(task) = victim_queue.pop_back() {
                    task.assigned_hart.store(hart_id as u8, Ordering::Relaxed);
                    return Some(task);
                }
            } else {
                core::hint::spin_loop();
            }
        }

        None
    }

    pub fn wake_all(&self) {
        // This is only used as a fallback if hardware events don't trigger wakers correctly.
    }

    pub fn task_count(&self) -> usize {
        self.spawned_count.load(Ordering::SeqCst) - self.completed_count.load(Ordering::SeqCst)
    }
}

pub static EXECUTOR: Mutex<Option<Executor>> = Mutex::new(None);

pub fn set_global_executor(exec: Executor) {
    let boxed = Box::new(exec.clone());
    let ptr = Box::into_raw(boxed);
    EXECUTOR_PTR.store(ptr, Ordering::Release);
    *EXECUTOR.lock() = Some(exec);
}

pub fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    spawn_named(future, None);
}

pub fn spawn_named(future: impl Future<Output = ()> + Send + 'static, name: Option<&'static str>) {
    let ptr = EXECUTOR_PTR.load(Ordering::Acquire);
    if !ptr.is_null() {
        let executor = unsafe { &*ptr };
        executor.spawn_named(future, name);
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
    registered: bool,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let now = crate::clint::mtime();
        if now >= self.target_ticks {
            Poll::Ready(())
        } else {
            if !self.registered {
                let ptr = EXECUTOR_PTR.load(Ordering::Acquire);
                if !ptr.is_null() {
                    let executor = unsafe { &*ptr };
                    executor.timers.lock().push(TimerEntry {
                        target_ticks: self.target_ticks,
                        waker: cx.waker().clone(),
                    });
                    self.registered = true;
                }
            }
            Poll::Pending
        }
    }
}

pub fn sleep(duration: core::time::Duration) -> Sleep {
    let ticks = (duration.as_nanos() * crate::clint::FREQ as u128 / 1_000_000_000) as u64;
    Sleep {
        target_ticks: crate::clint::mtime() + ticks,
        registered: false,
    }
}

pub const FREQ: u64 = crate::clint::FREQ;
