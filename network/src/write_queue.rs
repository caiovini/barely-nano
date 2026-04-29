use crate::{channel_stats::ChannelStats, TrafficType};
use rsnano_core::utils::FairQueue;
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use core::task::{Context, Poll, Waker};
use core::future::Future;
use core::pin::Pin;

pub struct WriteQueue {
    queue: Mutex<FairQueue<TrafficType, Entry>>,
    notify_enqueued: SimpleNotify,
    notify_dequeued: SimpleNotify,
    closed: AtomicBool,
    stats: Arc<ChannelStats>,
}

impl WriteQueue {
    pub(crate) fn new(max_size: usize, stats: Arc<ChannelStats>) -> Self {
        Self {
            queue: Mutex::new(FairQueue::new(
                move |_| max_size,
                |traffic_type| match traffic_type {
                    TrafficType::BlockBroadcast | TrafficType::VoteRebroadcast => 1,
                    _ => 4,
                },
            )),
            notify_enqueued: SimpleNotify::new(),
            notify_dequeued: SimpleNotify::new(),
            closed: AtomicBool::new(false),
            stats,
        }
    }

    pub async fn insert(&self, buffer: Arc<Vec<u8>>, traffic_type: TrafficType) {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return;
            }

            {
                let mut guard = self.queue.lock();
                if guard.free_capacity(&traffic_type) > 0 {
                    let entry = Entry { buffer };
                    guard.push(traffic_type, entry);
                    break;
                }
            }

            self.notify_dequeued.notified().await;
        }

        self.notify_enqueued.notify_one();
    }

    /// returns: inserted
    pub fn try_insert(&self, buffer: Arc<Vec<u8>>, traffic_type: TrafficType) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let entry = Entry { buffer };
        let inserted = self.queue.lock().push(traffic_type, entry);

        if inserted {
            self.notify_enqueued.notify_one();
        }

        inserted
    }

    pub fn free_capacity(&self, traffic_type: TrafficType) -> usize {
        self.queue.lock().free_capacity(&traffic_type)
    }

    pub fn len(&self) -> usize {
        self.queue.lock().len()
    }

    pub async fn pop(&self) -> Option<Entry> {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }

            let result = self.queue.lock().next();
            if let Some((traffic_type, entry)) = result {
                self.notify_dequeued.notify_one();
                self.stats
                    .write_succeeded
                    .fetch_add(entry.buffer.len(), Ordering::Relaxed);
                self.stats.sent_by_type[traffic_type as usize]
                    .fetch_add(entry.buffer.len(), Ordering::Relaxed);
                return Some(entry);
            }

            self.notify_enqueued.notified().await;
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify_enqueued.notify_one();
        self.notify_dequeued.notify_one();
    }
}

impl Drop for WriteQueue {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct Entry {
    pub buffer: Arc<Vec<u8>>,
}

struct SimpleNotify {
    notified: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl SimpleNotify {
    fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn notify_one(&self) {
        self.notified.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().take() {
            waker.wake();
        }
    }

    fn notified(&self) -> Notified<'_> {
        Notified { notify: self }
    }
}

struct Notified<'a> {
    notify: &'a SimpleNotify,
}

impl<'a> Future for Notified<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.notify.notified.swap(false, Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            let mut guard = self.notify.waker.lock();
            if self.notify.notified.swap(false, Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                *guard = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
