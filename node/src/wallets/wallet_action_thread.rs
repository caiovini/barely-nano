use super::Wallet;
use rsnano_core::Amount;
use alloc::sync::Arc;
use spin::Mutex;
use alloc::vec::Vec;
use alloc::boxed::Box;

pub struct WalletActionThread {}

impl WalletActionThread {
    pub fn new() -> Self {
        Self {}
    }

    pub fn start(&self) {}
    pub fn stop(&self) {}

    pub fn queue_wallet_action(
        &self,
        _amount: Amount,
        _wallet: Arc<Wallet>,
        _action: Box<dyn Fn(Arc<Wallet>) + Send>,
    ) {}

    pub fn len(&self) -> usize {
        0
    }

    pub fn set_observer(&self, _observer: Box<dyn Fn(bool) + Send>) {}

    pub fn lock_safe(&self) {}
}
