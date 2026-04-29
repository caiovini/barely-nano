use rsnano_core::{Account, AccountInfo, Amount, BlockHash};
use rsnano_ledger_store::BtreeStore;

use super::LedgerSet;

/// Unconfirmed Blocks of the ledger.
pub(crate) struct OwningUnconfirmedSet<'a> {
    store: &'a BtreeStore,
}

impl<'a> OwningUnconfirmedSet<'a> {
    pub fn new(store: &'a BtreeStore) -> Self {
        Self { store }
    }

    fn borrowing_set(&'a self) -> BorrowingUnconfirmedSet<'a> {
        BorrowingUnconfirmedSet {
            store: self.store,
        }
    }
}

impl<'a> LedgerSet for OwningUnconfirmedSet<'a> {
    fn block_exists(&self, hash: &BlockHash) -> bool {
        self.borrowing_set().block_exists(hash)
    }

    fn account_receivable(&self, account: &Account) -> Amount {
        self.borrowing_set().account_receivable(account)
    }

    fn account_balance(&self, account: &Account) -> Amount {
        self.borrowing_set().account_balance(account)
    }

    fn get_account(&self, account: &Account) -> Option<AccountInfo> {
        self.borrowing_set().get_account(account)
    }
}

/// Unconfirmed Blocks of the ledger
pub(crate) struct BorrowingUnconfirmedSet<'a> {
    store: &'a BtreeStore,
}

impl<'a> LedgerSet for BorrowingUnconfirmedSet<'a> {
    fn block_exists(&self, hash: &BlockHash) -> bool {
        if hash.is_zero() {
            return false;
        }

        let Some(block) = self.store.block.get(hash) else {
            return false;
        };

        let conf_info = self
            .store
            .confirmation_height
            .get(&block.account())
            .unwrap_or_default();

        block.height() > conf_info.height
    }

    fn account_receivable(&self, _account: &Account) -> Amount {
        Amount::zero()
    }

    fn account_balance(&self, _account: &Account) -> Amount {
        Amount::zero()
    }

    fn get_account(&self, _account: &Account) -> Option<AccountInfo> {
        None
    }
}
