use super::Wallets;
use crate::utils::ThreadPool;
use alloc::sync::Arc;
use core::time::Duration;
use alloc::boxed::Box;

pub(crate) struct WalletBackup {
    pub workers: Arc<dyn ThreadPool>,
    pub wallets: Arc<Wallets>,
}

impl WalletBackup {
    pub fn start(&self) {
        ongoing_backup(self.workers.clone(), self.wallets.clone());
    }
}

fn ongoing_backup(workers: Arc<dyn ThreadPool>, wallets: Arc<Wallets>) {
    let workers_w = Arc::downgrade(&workers);
    let wallets_w = Arc::downgrade(&wallets);

    workers.post_delayed(
        BACKUP_INTERVAL,
        Box::new(move || {
            let Some(workers) = workers_w.upgrade() else {
                return;
            };
            let Some(wallets) = wallets_w.upgrade() else {
                return;
            };
            ongoing_backup(workers, wallets);
        }),
    )
}

const BACKUP_INTERVAL: Duration = Duration::from_secs(60 * 5);
