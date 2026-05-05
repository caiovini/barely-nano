use alloc::sync::Arc;
use alloc::string::String;
mod backpressure_event_processor;
mod processing_queue;
mod rate_calculator;
mod thread_pool;
mod timer;
mod timer_thread;
mod bounded_vec_deque;


use rsnano_sha::blake2b::Blake2bHasher;


pub(crate) use backpressure_event_processor::{
    spawn_backpressure_processor, BackpressureEventProcessor,
};

pub use timer::*;
pub use processing_queue::*;
pub use rate_calculator::RateCalculator;
pub use thread_pool::*;
pub use timer_thread::*;
pub use bounded_vec_deque::BoundedVecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorCode {
    pub val: i32,
    pub category: u8,
}

pub mod error_category {
    pub const GENERIC: u8 = 0;
    pub const SYSTEM: u8 = 1;
}

pub fn get_env_or_default_string(name: &str, default: &str) -> String {
    env_var_stub(name).unwrap_or_else(|_| default.into())
}

pub fn get_env_or_default_u64(name: &str, default: u64) -> u64 {
    env_var_stub(name)
        .map(|v| v.parse().unwrap_or(default))
        .unwrap_or(default)
}

pub fn get_env_or_default_bool(name: &str, default: bool) -> bool {
    env_var_stub(name)
        .map(|v| v.parse().unwrap_or(default))
        .unwrap_or(default)
}

pub fn blake2b_256_hash(data: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2bHasher::new(32);
    for chunk in data {
        hasher.update(chunk);
    }
    let full = hasher.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&full[..32]);
    result
}

pub fn blake2b_512_hash(data: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Blake2bHasher::new(64);
    for chunk in data {
        hasher.update(chunk);
    }
    hasher.finalize()
}

pub struct MemoryStream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> MemoryStream<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn read_u8(&mut self) -> Result<u8, ()> {
        if self.pos < self.data.len() {
            let val = self.data[self.pos];
            self.pos += 1;
            Ok(val)
        } else {
            Err(())
        }
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ()> {
        if self.pos + len <= self.data.len() {
            let val = &self.data[self.pos..self.pos + len];
            self.pos += len;
            Ok(val)
        } else {
            Err(())
        }
    }
}

pub fn random_u64() -> u64 {
    // Stub: In a real bare-metal app, use a hardware RNG
    static mut SEED: u64 = 0x123456789ABCDEF0;
    unsafe {
        SEED = SEED.wrapping_mul(6364136223846793005).wrapping_add(1);
        SEED
    }
}

pub fn hash_pow(data: &[u8], work: u64) -> u64 {
    let mut hasher = Blake2bHasher::new(8);
    hasher.update(&work.to_le_bytes());
    hasher.update(data);
    let full = hasher.finalize();
    let mut buffer = [0; 8];
    buffer.copy_from_slice(&full[..8]);
    u64::from_ne_bytes(buffer)
}

pub fn env_var_stub(_name: &str) -> Result<String, ()> {
    Err(())
}

pub struct Condvar;
impl Condvar {
    pub fn new() -> Self { Self }
    pub fn notify_one(&self) {}
    pub fn notify_all(&self) {}

    pub fn wait<'a, T>(&self, guard: spin::MutexGuard<'a, T>) -> Result<spin::MutexGuard<'a, T>, ()> {
        // In a no_std environment without a proper scheduler, we can't easily implement a real Condvar
        // that releases the lock and waits for a signal.
        // Instead, we turn this into a "yield" that keeps the lock but allows other harts a tiny window
        // if we were to drop it. But since we don't have the Mutex reference, we just spin a bit.
        // To actually work with other harts, we'd need a better Mutex/Condvar pair.
        // For now, returning the guard immediately turns this into a busy-wait loop in pop_blocking,
        // which is better than being stuck forever.
        core::hint::spin_loop();
        Ok(guard)
    }

    pub fn wait_while<'a, T, F>(&self, mut guard: spin::MutexGuard<'a, T>, mut f: F) -> Result<spin::MutexGuard<'a, T>, ()> 
    where F: FnMut(&mut T) -> bool {
        while f(&mut *guard) {
            guard = self.wait(guard)?;
            // NOTE: This will spin forever if the condition doesn't change, 
            // and because we don't release the lock in wait(), it might prevent the condition from changing!
            // This is why pop_blocking was refactored to not use this.
        }
        Ok(guard)
    }

    pub fn wait_timeout<'a, T>(&self, guard: spin::MutexGuard<'a, T>, _dur: core::time::Duration) -> Result<(spin::MutexGuard<'a, T>, ()), ()> {
        Ok((self.wait(guard)?, ()))
    }

    pub fn wait_timeout_while<'a, T, F>(&self, guard: spin::MutexGuard<'a, T>, _dur: core::time::Duration, mut f: F) -> Result<(spin::MutexGuard<'a, T>, ()), ()> 
    where F: FnMut(&mut T) -> bool {
        let mut guard = guard;
        while f(&mut *guard) {
            guard = self.wait(guard)?;
        }
        Ok((guard, ()))
    }
}

pub struct JoinHandle<T>(core::marker::PhantomData<T>);
impl<T> JoinHandle<T> {
    pub fn new() -> Self { Self(core::marker::PhantomData) }
    pub fn join(self) -> Result<T, ()> { Err(()) }
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

pub struct Receiver<T>(core::marker::PhantomData<T>);
impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, ()> { Err(()) }
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
