use rsnano_core::{
    Account, AccountInfo, Amount, BlockHash, ConfirmationHeightInfo, PendingInfo, PendingKey,
    SavedBlock,
};
use rsnano_ledger_store::BtreeStore;

use super::{AnyReceivableIterator, LedgerSet};

pub trait ConfirmedSet: LedgerSet {
    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock>;
    fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool;
    fn get_conf_info(&self, account: &Account) -> Option<ConfirmationHeightInfo>;
}

/// Only blocks that are confirmed.
pub struct OwningConfirmedSet<'a> {
    store: &'a BtreeStore,
}

impl<'a> OwningConfirmedSet<'a> {
    pub fn new(store: &'a BtreeStore) -> Self {
        Self { store }
    }

    fn borrowing_set(&'a self) -> BorrowingConfirmedSet<'a> {
        BorrowingConfirmedSet {
            store: self.store,
            tx: &(),
        }
    }

    fn first_receivable_lower_bound(
        &self,
        _account: Account,
        _send_hash: BlockHash,
    ) -> Option<(PendingKey, PendingInfo)> {
        // Btree doesn't have range iterators yet
        None
    }

    /// Returns the next receivable entry for an account greater than or equal to 'account'
    pub fn receivable_lower_bound(
        &'a self,
        account: Account,
    ) -> ConfirmedReceivableIterator<'a>
    {
        ConfirmedReceivableIterator {
            set: self,
            requested_account: account,
            actual_account: None,
            next_hash: Some(BlockHash::zero()),
        }
    }
}

impl<'a> LedgerSet for OwningConfirmedSet<'a> {
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

impl<'a> ConfirmedSet for OwningConfirmedSet<'a> {
    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock> {
        self.borrowing_set().get_block(hash)
    }

    fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool {
        self.borrowing_set().block_exists_or_pruned(hash)
    }

    fn get_conf_info(&self, account: &Account) -> Option<ConfirmationHeightInfo> {
        self.borrowing_set().get_conf_info(account)
    }
}

/// Only blocks that are confirmed.
pub struct BorrowingConfirmedSet<'a> {
    store: &'a BtreeStore,
    pub tx: &'a (),
}

impl<'a> BorrowingConfirmedSet<'a> {
    pub fn new(store: &'a BtreeStore, tx: &'a ()) -> Self {
        Self { store, tx }
    }

    fn account_receivable_upper_bound(
        &self,
        account: Account,
        hash: BlockHash,
    ) -> AnyReceivableIterator {
        AnyReceivableIterator::new(
            &self.store.pending,
            account,
            Some(account),
            hash.inc(),
        )
    }

    fn account_head(&self, account: &Account) -> Option<BlockHash> {
        let info = self.store.confirmation_height.get(account)?;
        Some(info.frontier)
    }
}

impl<'a> LedgerSet for BorrowingConfirmedSet<'a> {
    fn block_exists(&self, hash: &BlockHash) -> bool {
        self.get_block(hash).is_some()
    }

    fn account_receivable(&self, account: &Account) -> Amount {
        let mut result = Amount::zero();

        for (key, info) in self.account_receivable_upper_bound(*account, BlockHash::zero()) {
            if self.block_exists_or_pruned(&key.send_block_hash) {
                result += info.amount;
            }
        }

        result
    }

    fn account_balance(&self, account: &Account) -> Amount {
        let Some(head) = self.account_head(account) else {
            return Amount::zero();
        };

        self.get_block(&head)
            .map(|b| b.balance())
            .unwrap_or_default()
    }

    fn get_account(&self, _account: &Account) -> Option<AccountInfo> {
        None
    }
}

impl<'a> ConfirmedSet for BorrowingConfirmedSet<'a> {
    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock> {
        if hash.is_zero() {
            return None;
        }
        let block = self.store.block.get(hash)?;

        let conf_info = self
            .store
            .confirmation_height
            .get(&block.account())?;

        if block.height() <= conf_info.height {
            Some(block)
        } else {
            None
        }
    }

    fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool {
        if hash.is_zero() {
            return false;
        }
        if self.store.pruned.exists(hash) {
            true
        } else {
            self.block_exists(hash)
        }
    }

    fn get_conf_info(&self, account: &Account) -> Option<ConfirmationHeightInfo> {
        self.store.confirmation_height.get(account)
    }
}

pub struct ConfirmedReceivableIterator<'a> {
    pub set: &'a OwningConfirmedSet<'a>,
    pub requested_account: Account,
    pub actual_account: Option<Account>,
    pub next_hash: Option<BlockHash>,
}

impl<'a> Iterator for ConfirmedReceivableIterator<'a> {
    type Item = (PendingKey, PendingInfo);

    fn next(&mut self) -> Option<Self::Item> {
        let hash = self.next_hash?;
        let account = self.actual_account.unwrap_or(self.requested_account);
        let (key, info) = self.set.first_receivable_lower_bound(account, hash)?;
        match self.actual_account {
            Some(account) => {
                if key.receiving_account == account {
                    self.next_hash = key.send_block_hash.inc();
                    Some((key.clone(), info.clone()))
                } else {
                    None
                }
            }
            None => {
                self.actual_account = Some(key.receiving_account);
                self.next_hash = key.send_block_hash.inc();
                Some((key.clone(), info.clone()))
            }
        }
    }
}
