use alloc::sync::Arc;
mod backpressure_event_processor;
mod bounded_vec_deque;
mod processing_queue;
mod rate_calculator;
mod thread_pool;
mod timer;

pub(crate) use backpressure_event_processor::{
    spawn_backpressure_processor, BackpressureEventProcessor,
};

pub use bounded_vec_deque::BoundedVecDeque;
pub use processing_queue::*;
pub use rate_calculator::RateCalculator;
pub use thread_pool::*;
pub use timer::*;

pub fn random_u64() -> u64 {
    rsnano_nullable_random::random_u64()
}

pub struct Condvar;
impl Condvar {
    pub fn new() -> Self {
        Self
    }
    pub fn notify_one(&self) {}
    pub fn notify_all(&self) {}

    pub fn wait<'a, T>(
        &self,
        guard: spin::MutexGuard<'a, T>,
    ) -> Result<spin::MutexGuard<'a, T>, ()> {
        for _ in 0..100 {
            core::hint::spin_loop();
        }
        Ok(guard)
    }

    pub fn wait_while<'a, T, F>(
        &self,
        mut guard: spin::MutexGuard<'a, T>,
        mut f: F,
    ) -> Result<spin::MutexGuard<'a, T>, ()>
    where
        F: FnMut(&mut T) -> bool,
    {
        while f(&mut *guard) {
            guard = self.wait(guard)?;
        }
        Ok(guard)
    }

    pub fn wait_timeout<'a, T>(
        &self,
        guard: spin::MutexGuard<'a, T>,
        _dur: core::time::Duration,
    ) -> Result<(spin::MutexGuard<'a, T>, ()), ()> {
        Ok((self.wait(guard)?, ()))
    }

    pub fn wait_timeout_while<'a, T, F>(
        &self,
        mut guard: spin::MutexGuard<'a, T>,
        _dur: core::time::Duration,
        mut f: F,
    ) -> Result<(spin::MutexGuard<'a, T>, ()), ()>
    where
        F: FnMut(&mut T) -> bool,
    {
        while f(&mut *guard) {
            guard = self.wait(guard)?;
        }
        Ok((guard, ()))
    }
}

pub struct JoinHandle<T>(core::marker::PhantomData<T>);
impl<T> JoinHandle<T> {
    pub fn new() -> Self {
        Self(core::marker::PhantomData)
    }
    pub fn join(self) -> Result<T, ()> {
        Err(())
    }
}

pub struct SyncSender<T>(Option<Arc<dyn Fn(T) + Send + Sync>>);

impl<T> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> SyncSender<T> {
    pub fn new(f: Arc<dyn Fn(T) + Send + Sync>) -> Self {
        Self(Some(f))
    }
    pub fn send(&self, val: T) -> Result<(), ()> {
        if let Some(f) = &self.0 {
            f(val);
            Ok(())
        } else {
            Err(())
        }
    }
}

pub struct LazyLock<T, F = fn() -> T> {
    once: spin::Once<T>,
    init: F,
}

impl<T, F: Fn() -> T> LazyLock<T, F> {
    pub const fn new(init: F) -> Self {
        Self {
            once: spin::Once::new(),
            init,
        }
    }
}

impl<T, F: Fn() -> T> core::ops::Deref for LazyLock<T, F> {
    type Target = T;
    fn deref(&self) -> &T {
        self.once.call_once(|| (self.init)())
    }
}

unsafe impl<T: Sync + Send, F: Send> Sync for LazyLock<T, F> {}
