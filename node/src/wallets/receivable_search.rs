use rsnano_core::Networks;
use alloc::sync::Arc;
use core::time::Duration;
use alloc::boxed::Box;

use super::Wallets;
use crate::utils::ThreadPool;

pub(crate) struct ReceivableSearch {
    wallets: Arc<Wallets>,
    workers: Arc<dyn ThreadPool>,
    interval: Duration,
}

impl ReceivableSearch {
    pub(crate) fn new(
        wallets: Arc<Wallets>,
        workers: Arc<dyn ThreadPool>,
        network: Networks,
    ) -> Self {
        Self {
            wallets,
            workers,
            interval: Self::interval_for(network),
        }
    }

    fn interval_for(network: Networks) -> Duration {
        match network {
            Networks::NanoDevNetwork => Duration::from_secs(1),
            _ => Duration::from_secs(5),
        }
    }

    pub fn start(&self) {
        search_receivables(
            self.wallets.clone(),
            self.workers.clone(),
            self.interval.clone(),
        );
    }
}

fn search_receivables(wallets: Arc<Wallets>, workers: Arc<dyn ThreadPool>, interval: Duration) {
    wallets.reload();
    // wallets.search_receivable_all(); // TODO: implement in Wallets stub if needed

    let wallets_w = Arc::downgrade(&wallets);
    let workers_w = Arc::downgrade(&workers);

    workers.post_delayed(
        interval,
        Box::new(move || {
            let Some(wallets) = wallets_w.upgrade() else {
                return;
            };
            let Some(workers) = workers_w.upgrade() else {
                return;
            };
            search_receivables(wallets, workers, interval);
        }),
    )
}
