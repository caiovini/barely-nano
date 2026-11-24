use core::cmp::max;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use core::task::{Context, Poll, Waker};
use alloc::sync::Arc;
use alloc::collections::VecDeque;
use spin::Mutex;

pub struct BackpressureSender<T> {
    queue: Arc<Mutex<VecDeque<T>>>,
    queue_size: Arc<AtomicI32>,
    waker: Arc<Mutex<Option<Waker>>>,
    has_waker: Arc<AtomicBool>,
    hard_limit: usize,
}
 
pub struct BackpressureReceiver<T> {
    queue: Arc<Mutex<VecDeque<T>>>,
    queue_size: Arc<AtomicI32>,
    waker: Arc<Mutex<Option<Waker>>>,
    has_waker: Arc<AtomicBool>,
    soft_limit: usize,
    is_cooling_down: AtomicBool,
}

#[derive(Debug)]
pub struct SendError<T>(pub T);

#[derive(Debug)]
pub struct RecvError;

#[derive(Debug)]
pub enum TryRecvError {
    Empty,
    Disconnected,
}

pub fn backpressure_channel<T>(
    soft_limit: usize,
) -> (BackpressureSender<T>, BackpressureReceiver<T>) {
    let cap = if soft_limit > 0 { soft_limit.max(1024) } else { 0 };
    let hard_limit = if cap > 0 { cap * 2 } else { 0 };
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(hard_limit)));
    let queue_size = Arc::new(AtomicI32::new(0));
    let waker = Arc::new(Mutex::new(None));
    let has_waker = Arc::new(AtomicBool::new(false));
 
    let tx = BackpressureSender {
        queue: Arc::clone(&queue),
        queue_size: Arc::clone(&queue_size),
        waker: Arc::clone(&waker),
        has_waker: Arc::clone(&has_waker),
        hard_limit,
    };
 
    let rx = BackpressureReceiver {
        queue,
        queue_size,
        waker,
        has_waker,
        soft_limit,
        is_cooling_down: AtomicBool::new(false),
    };
 
    (tx, rx)
}

impl<T> BackpressureSender<T> {
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        let was_empty;
        {
            let mut q = self.queue.lock();
            if self.hard_limit != 0 && q.len() >= self.hard_limit {
                return Err(SendError(t));
            }
            was_empty = q.is_empty();
            q.push_back(t);
            self.queue_size.fetch_add(1, Ordering::Release);
        }
        if was_empty || self.has_waker.load(Ordering::Acquire) {
            if let Some(w) = self.waker.lock().take() {
                self.has_waker.store(false, Ordering::Release);
                w.wake();
            }
        }
        Ok(())
    }

    pub fn try_send(&self, t: T) -> Result<(), SendError<T>> {
        self.send(t)
    }

    pub fn len(&self) -> usize {
        let len = self.queue_size.load(Ordering::Relaxed);
        max(len, 0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for BackpressureSender<T> {
    fn drop(&mut self) {
        // When the last sender drops (`strong_count == 2`: this sender + the receiver),
        // wake any parked receiver so it can observe disconnection and exit cleanly.
        if Arc::strong_count(&self.queue) <= 2 && self.has_waker.load(Ordering::Acquire) {
            if let Some(w) = self.waker.lock().take() {
                self.has_waker.store(false, Ordering::Release);
                w.wake();
            }
        }
    }
}

pub struct WaitForData<'a, T> {
    receiver: &'a BackpressureReceiver<T>,
}

impl<T> Future for WaitForData<'_, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if !self.receiver.queue.lock().is_empty() || self.receiver.is_disconnected() {
            return Poll::Ready(());
        }
        {
            let mut wg = self.receiver.waker.lock();
            match wg.as_ref() {
                Some(existing) if existing.will_wake(cx.waker()) => {}
                _ => *wg = Some(cx.waker().clone()),
            }
            self.receiver.has_waker.store(true, Ordering::Release);
        }
        if !self.receiver.queue.lock().is_empty() || self.receiver.is_disconnected() {
            if self.receiver.waker.lock().take().is_some() {
                self.receiver.has_waker.store(false, Ordering::Release);
            }
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl<T> BackpressureReceiver<T> {
    /// Receive a value from the channel and update the size
    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            if let Some(t) = self.queue.lock().pop_front() {
                self.received();
                return Ok(t);
            }
            if self.is_disconnected() {
                return Err(RecvError);
            }
            core::hint::spin_loop();
        }
    }
 
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(t) = self.queue.lock().pop_front() {
            self.received();
            Ok(t)
        } else if self.is_disconnected() {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    #[inline]
    pub fn is_disconnected(&self) -> bool {
        Arc::strong_count(&self.queue) == 1
    }

    #[inline]
    pub fn wait_for_data(&self) -> WaitForData<'_, T> {
        WaitForData { receiver: self }
    }

    fn received(&self) {
        self.queue_size.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get the current size of the channel
    pub fn len(&self) -> usize {
        let len = self.queue_size.load(Ordering::Relaxed);
        max(len, 0) as usize
    }

    /// Check if the channel is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn should_cool_down(&self) -> bool {
        let cooldown = self.is_cooling_down.load(Ordering::Relaxed);
        if cooldown && self.is_recovered() {
            self.is_cooling_down.store(false, Ordering::Relaxed);
        } else if !cooldown && self.len() >= self.soft_limit {
            self.is_cooling_down.store(true, Ordering::Relaxed);
        }
        self.is_cooling_down.load(Ordering::Relaxed)
    }

    fn is_recovered(&self) -> bool {
        self.len() <= self.soft_limit / 2
    }
}

impl<T> Clone for BackpressureSender<T> {
    fn clone(&self) -> Self {
        BackpressureSender {
            queue: Arc::clone(&self.queue),
            queue_size: Arc::clone(&self.queue_size),
            waker: Arc::clone(&self.waker),
            has_waker: Arc::clone(&self.has_waker),
            hard_limit: self.hard_limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_basic_functionality() {
        let (tx, rx) = backpressure_channel(1000);

        // Initially empty
        assert_eq!(tx.len(), 0);
        assert!(tx.is_empty());

        // Send and check size
        tx.send(42).unwrap();
        assert_eq!(tx.len(), 1);
        assert!(!tx.is_empty());

        // Receive and check size
        let val = rx.recv().unwrap();
        assert_eq!(val, 42);
        assert_eq!(rx.len(), 0);
        assert!(rx.is_empty());
    }

    #[test]
    fn test_never_negative() {
        let (tx, rx) = backpressure_channel(1000);

        // Send one item
        tx.send(1).unwrap();
        assert_eq!(tx.len(), 1);

        // Receive the item
        rx.recv().unwrap();
        assert_eq!(rx.len(), 0);

        // Try to receive more than sent (should error but not affect counter)
        let result = rx.try_recv();
        assert!(result.is_err());
        assert_eq!(rx.len(), 0); // Counter should still be zero, not negative
    }

    #[test]
    fn test_channel_cloning() {
        let (tx1, rx) = backpressure_channel(1000);
        let tx2 = tx1.clone();

        // Send from both senders
        tx1.send(1).unwrap();
        tx2.send(2).unwrap();

        assert_eq!(tx1.len(), 2);
        assert_eq!(tx2.len(), 2);

        // Receive both items
        assert_eq!(rx.recv().unwrap(), 1);
        assert_eq!(rx.recv().unwrap(), 2);

        assert_eq!(rx.len(), 0);
    }

    #[test]
    fn test_simple_threading() {
        let (tx, rx) = backpressure_channel(1000);

        // Get a copy of the counter for testing after rx is moved
        let queue_size = Arc::clone(&rx.queue_size);

        // Spawn thread to send items
        let sender_thread = thread::spawn(move || {
            tx.send(1).unwrap();
            tx.send(2).unwrap();
            tx.send(3).unwrap();
        });

        // Spawn thread to receive items
        let receiver_thread = thread::spawn(move || {
            let sum = rx.recv().unwrap() + rx.recv().unwrap() + rx.recv().unwrap();
            sum
        });

        // Wait for threads to complete
        sender_thread.join().unwrap();
        let sum = receiver_thread.join().unwrap();

        // Verify results
        assert_eq!(sum, 6); // 1 + 2 + 3
        assert_eq!(queue_size.load(Ordering::Relaxed), 0); // Counter should be back to zero
    }

    #[test]
    fn should_know_when_to_cool_down() {
        let (tx, rx) = backpressure_channel(4);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        tx.send(3).unwrap();
        assert_eq!(rx.should_cool_down(), false);
        tx.send(4).unwrap();
        assert_eq!(rx.should_cool_down(), true);
    }

    #[test]
    fn should_end_cooldown_when_half_of_soft_limit_reached() {
        let (tx, rx) = backpressure_channel(4);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        tx.send(3).unwrap();
        tx.send(4).unwrap();
        assert_eq!(rx.should_cool_down(), true);
        rx.recv().unwrap();
        assert_eq!(rx.should_cool_down(), true);
        rx.recv().unwrap();
        assert_eq!(rx.should_cool_down(), false);
    }

    #[test]
    fn wakes_waiting_receiver_immediately_on_send_and_disconnect() {
        use alloc::task::Wake;
        use core::sync::atomic::AtomicUsize;

        struct CountWaker(AtomicUsize);
        impl Wake for CountWaker {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let (tx, rx) = backpressure_channel(16);
        let counter = Arc::new(CountWaker(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);

        let mut fut = Box::pin(rx.wait_for_data());
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        tx.send(99).unwrap();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(()));
        assert_eq!(rx.try_recv().unwrap(), 99);

        let mut fut2 = Box::pin(rx.wait_for_data());
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Pending);
        drop(tx);
        assert_eq!(counter.0.load(Ordering::SeqCst), 2);
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Ready(()));
        assert!(rx.is_disconnected());
    }
}
