use core::future::poll_fn;
use core::task::Poll;

struct QueryGuard {
    done: bool,
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        if !self.done {
            println!("Canceled");
        }
    }
}

pub async fn resolve() {
    let mut guard = QueryGuard { done: false };
    poll_fn(move |_cx| {
        guard.done = true;
        Poll::Ready(())
    }).await
}
