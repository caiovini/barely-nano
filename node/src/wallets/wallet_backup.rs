use super::Wallets;
use crate::utils::ThreadPool;
use alloc::sync::Arc;
use core::time::Duration;
use alloc::boxed::Box;
use alloc::string::String;

pub(crate) struct WalletBackup {
    pub data_path: String,
    pub workers: Arc<dyn ThreadPool>,
    pub wallets: Arc<Wallets>,
}

impl WalletBackup {
    pub fn start(&self) {
        let backup_path = self.data_path.clone() + "/backup";
        ongoing_backup(backup_path, self.workers.clone(), self.wallets.clone());
    }
}

fn ongoing_backup(backup_path: String, workers: Arc<dyn ThreadPool>, wallets: Arc<Wallets>) {
    // Stub for no_std
    // if let Err(e) = wallets.backup(&backup_path) {
    //     error!(error = ?e, "Could not create backup of wallets");
    // }

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
            ongoing_backup(backup_path, workers, wallets);
        }),
    )
}

const BACKUP_INTERVAL: Duration = Duration::from_secs(60 * 5);
