use core::ops::{RangeBounds};
use alloc::vec::Vec;

use rsnano_core::{
    block_priority, utils::BlockPriority, Account, AccountInfo, Amount, Block, BlockHash,
    DependentBlocks, DetailedBlock, PendingInfo, PendingKey, PublicKey, QualifiedRoot, Root,
    SavedBlock,
};
use rsnano_ledger_store::{BtreeStore, BtreePendingStore};

use super::{BorrowingConfirmedSet, ConfirmedSet, LedgerSet};
use crate::{DependentBlocksFinder, LedgerConstants, RepresentativeBlockFinder};

pub trait AnySet: LedgerSet {
    fn should_refresh(&self) -> bool;
    fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool;
    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock>;
    fn receivable_exists(&self, account: Account) -> bool;
    fn confirmed(&self) -> BorrowingConfirmedSet;

    fn block_balance(&self, hash: &BlockHash) -> Option<Amount> {
        if hash.is_zero() {
            return None;
        }

        self.get_block(hash).map(|b| b.balance())
    }

    fn dependent_blocks(&self, block: &SavedBlock) -> DependentBlocks;
    fn dependents_confirmed(&self, block: &SavedBlock) -> bool;
    fn dependents_confirmed_for_unsaved_block(&self, block: &Block) -> bool;
    fn block_successor(&self, hash: &BlockHash) -> Option<BlockHash>;
    fn block_successor_by_qualified_root(&self, root: &QualifiedRoot) -> Option<BlockHash>;

    fn block_priority(&self, block: &SavedBlock) -> BlockPriority;

    fn previous_block(&self, block: &SavedBlock) -> Option<SavedBlock>;
    fn get_pending(&self, key: &PendingKey) -> Option<PendingInfo>;
    fn account_head(&self, account: &Account) -> Option<BlockHash>;
    fn block_account(&self, hash: &BlockHash) -> Option<Account>;

    fn representative_block_hash(&self, hash: &BlockHash) -> BlockHash;

    fn find_receive_block_by_send_hash(
        &self,
        destination: &Account,
        send_block_hash: &BlockHash,
    ) -> Option<SavedBlock>;

    fn linked_account(&self, block: &SavedBlock) -> Option<Account>;

    fn block_amount(&self, hash: &BlockHash) -> Option<Amount>;
    fn block_amount_for(&self, block: &SavedBlock) -> Option<Amount>;
    fn detailed_block(&self, hash: &BlockHash) -> Option<DetailedBlock>;

    fn receivable_upper_bound(&self, account: Account) -> AnyReceivableIterator;
    fn receivable_lower_bound(&self, account: Account) -> AnyReceivableIterator;
    fn account_receivable_upper_bound(
        &self,
        account: Account,
        hash: BlockHash,
    ) -> AnyReceivableIterator;

    fn get_final_vote(&self, root: &QualifiedRoot) -> Option<BlockHash>;
}

/// All blocks - either confirmed or unconfirmed
pub struct OwningAnySet<'a> {
    store: &'a BtreeStore,
    constants: &'a LedgerConstants,
}

impl<'a> OwningAnySet<'a> {
    pub(crate) fn new(store: &'a BtreeStore, constants: &'a LedgerConstants) -> Self {
        Self {
            store,
            constants,
        }
    }

    fn borrowing_set(&'a self) -> BorrowingAnySet<'a> {
        BorrowingAnySet {
            store: self.store,
            tx: &(),
            constants: self.constants,
        }
    }

    pub fn accounts_range(
        &self,
        _range: impl RangeBounds<Account> + 'static,
    ) -> Vec<(Account, AccountInfo)> {
        // Btree doesn't have range iterators yet
        Vec::new()
    }

    pub fn iter_accounts(&self) -> Vec<(Account, AccountInfo)> {
        Vec::new()
    }

    pub fn random_blocks(&self, count: usize) -> Vec<SavedBlock> {
        // Mock
        Vec::with_capacity(count)
    }

    pub fn latest_root(&self, account: &Account) -> Root {
        match self.get_account(account) {
            Some(info) => info.head.into(),
            None => account.into(),
        }
    }

    pub fn weight_exact(&self, representative: PublicKey) -> Amount {
        self.store
            .rep_weight
            .get(&(), &representative)
            .unwrap_or_default()
    }

    pub fn refresh_if_needed(&mut self) {
    }
}

impl<'a> LedgerSet for OwningAnySet<'a> {
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

impl<'a> AnySet for OwningAnySet<'a> {
    fn should_refresh(&self) -> bool {
        false
    }
    fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool {
        self.borrowing_set().block_exists_or_pruned(hash)
    }

    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock> {
        self.borrowing_set().get_block(hash)
    }

    fn confirmed(&self) -> BorrowingConfirmedSet {
        BorrowingConfirmedSet::new(self.store, &())
    }

    fn dependent_blocks(&self, block: &SavedBlock) -> DependentBlocks {
        self.borrowing_set().dependent_blocks(block)
    }

    fn dependents_confirmed(&self, block: &SavedBlock) -> bool {
        self.borrowing_set().dependents_confirmed(block)
    }

    fn dependents_confirmed_for_unsaved_block(&self, block: &Block) -> bool {
        self.borrowing_set()
            .dependents_confirmed_for_unsaved_block(block)
    }

    fn block_successor(&self, hash: &BlockHash) -> Option<BlockHash> {
        self.borrowing_set().block_successor(hash)
    }

    fn block_successor_by_qualified_root(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        self.borrowing_set().block_successor_by_qualified_root(root)
    }

    fn block_priority(&self, block: &SavedBlock) -> BlockPriority {
        self.borrowing_set().block_priority(block)
    }

    fn previous_block(&self, block: &SavedBlock) -> Option<SavedBlock> {
        self.borrowing_set().previous_block(block)
    }

    fn receivable_exists(&self, account: Account) -> bool {
        self.borrowing_set().receivable_exists(account)
    }

    fn get_pending(&self, key: &PendingKey) -> Option<PendingInfo> {
        self.borrowing_set().get_pending(key)
    }

    fn account_head(&self, account: &Account) -> Option<BlockHash> {
        self.borrowing_set().account_head(account)
    }

    fn block_account(&self, hash: &BlockHash) -> Option<Account> {
        self.borrowing_set().block_account(hash)
    }

    fn representative_block_hash(&self, hash: &BlockHash) -> BlockHash {
        self.borrowing_set().representative_block_hash(hash)
    }

    fn find_receive_block_by_send_hash(
        &self,
        destination: &Account,
        send_block_hash: &BlockHash,
    ) -> Option<SavedBlock> {
        self.borrowing_set()
            .find_receive_block_by_send_hash(destination, send_block_hash)
    }

    fn linked_account(&self, block: &SavedBlock) -> Option<Account> {
        self.borrowing_set().linked_account(block)
    }

    fn block_amount(&self, hash: &BlockHash) -> Option<Amount> {
        self.borrowing_set().block_amount(hash)
    }

    fn block_amount_for(&self, block: &SavedBlock) -> Option<Amount> {
        self.borrowing_set().block_amount_for(block)
    }

    fn detailed_block(&self, hash: &BlockHash) -> Option<DetailedBlock> {
        self.borrowing_set().detailed_block(hash)
    }

    fn receivable_upper_bound(&self, account: Account) -> AnyReceivableIterator {
        self.borrowing_set().receivable_upper_bound(account)
    }

    fn receivable_lower_bound(&self, account: Account) -> AnyReceivableIterator {
        self.borrowing_set().receivable_lower_bound(account)
    }

    fn account_receivable_upper_bound(
        &self,
        account: Account,
        hash: BlockHash,
    ) -> AnyReceivableIterator {
        self.borrowing_set().account_receivable_upper_bound(account, hash)
    }

    fn get_final_vote(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        self.borrowing_set().get_final_vote(root)
    }
}

pub(crate) struct BorrowingAnySet<'a> {
    pub constants: &'a LedgerConstants,
    pub store: &'a BtreeStore,
    pub tx: &'a (),
}

impl<'a> BorrowingAnySet<'a> {
    fn dependent_blocks_for_unsaved_block(&self, block: &Block) -> DependentBlocks {
        DependentBlocksFinder::new(self, &self.constants)
            .find_dependent_blocks_for_unsaved_block(block)
    }
}

impl<'a> LedgerSet for BorrowingAnySet<'a> {
    fn block_exists(&self, hash: &BlockHash) -> bool {
        if hash.is_zero() {
            return false;
        }
        self.store.block.exists(hash)
    }

    fn account_receivable(&self, account: &Account) -> Amount {
        let mut result = Amount::zero();

        for (_, info) in self.account_receivable_upper_bound(*account, BlockHash::zero()) {
            result += info.amount;
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

    fn get_account(&self, account: &Account) -> Option<AccountInfo> {
        self.store.account.get(account)
    }
}

impl<'a> AnySet for BorrowingAnySet<'a> {
    fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock> {
        if hash.is_zero() {
            return None;
        }
        self.store.block.get(hash)
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

    fn receivable_exists(&self, account: Account) -> bool {
        self.account_receivable_upper_bound(account, BlockHash::zero())
            .next()
            .is_some()
    }

    fn confirmed(&self) -> BorrowingConfirmedSet {
        BorrowingConfirmedSet::new(self.store, self.tx)
    }

    fn dependents_confirmed_for_unsaved_block(&self, block: &Block) -> bool {
        self.dependent_blocks_for_unsaved_block(block)
            .iter()
            .all(|hash| self.confirmed().block_exists_or_pruned(hash))
    }

    fn dependents_confirmed(&self, block: &SavedBlock) -> bool {
        self.dependent_blocks(block)
            .iter()
            .all(|hash| self.confirmed().block_exists_or_pruned(hash))
    }

    fn dependent_blocks(&self, block: &SavedBlock) -> DependentBlocks {
        DependentBlocksFinder::new(self, self.constants).find_dependent_blocks(block)
    }

    fn should_refresh(&self) -> bool {
        false
    }

    fn block_successor(&self, hash: &BlockHash) -> Option<BlockHash> {
        self.block_successor_by_qualified_root(&QualifiedRoot::new(hash.into(), *hash))
    }

    fn block_successor_by_qualified_root(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        if !root.previous.is_zero() {
            self.store.successors.get(&root.previous)
        } else {
            self.get_account(&root.root.into()).map(|i| i.open_block)
        }
    }

    fn block_priority(&self, block: &SavedBlock) -> BlockPriority {
        let previous_block = self.previous_block(block);
        block_priority(block, previous_block.as_ref())
    }

    fn previous_block(&self, block: &SavedBlock) -> Option<SavedBlock> {
        if block.previous().is_zero() {
            None
        } else {
            self.get_block(&block.previous())
        }
    }

    fn get_pending(&self, key: &PendingKey) -> Option<PendingInfo> {
        self.store.pending.get(key)
    }

    fn account_head(&self, account: &Account) -> Option<BlockHash> {
        self.get_account(account).map(|i| i.head)
    }

    fn block_account(&self, hash: &BlockHash) -> Option<Account> {
        self.get_block(hash).map(|b| b.account())
    }

    /// Returns the latest block with representative information
    fn representative_block_hash(&self, hash: &BlockHash) -> BlockHash {
        let hash = RepresentativeBlockFinder::new(self.store).find_rep_block(*hash);
        debug_assert!(hash.is_zero() || self.store.block.exists(&hash));
        hash
    }

    fn find_receive_block_by_send_hash(
        &self,
        destination: &Account,
        send_block_hash: &BlockHash,
    ) -> Option<SavedBlock> {
        let info = self.confirmed().get_conf_info(destination)?;
        let mut possible_receive_block = self.get_block(&info.frontier);

        while let Some(current) = possible_receive_block {
            if current.is_receive() && Some(*send_block_hash) == current.source() {
                return Some(current);
            }

            possible_receive_block = self.get_block(&current.previous());
        }

        None
    }

    fn linked_account(&self, block: &SavedBlock) -> Option<Account> {
        if block.is_send() {
            Some(block.destination_or_link())
        } else if block.is_receive() {
            self.block_account(&block.source_or_link())
        } else {
            None
        }
    }

    fn block_amount(&self, hash: &BlockHash) -> Option<Amount> {
        let block = self.get_block(hash)?;
        self.block_amount_for(&block)
    }

    fn block_amount_for(&self, block: &SavedBlock) -> Option<Amount> {
        let block_balance = block.balance();
        if block.previous().is_zero() {
            Some(block_balance)
        } else {
            let previous_balance = self.block_balance(&block.previous())?;
            if block_balance > previous_balance {
                Some(block_balance - previous_balance)
            } else {
                Some(previous_balance - block_balance)
            }
        }
    }

    fn detailed_block(&self, hash: &BlockHash) -> Option<DetailedBlock> {
        let block = self.get_block(hash)?;
        let amount = self.block_amount_for(&block);
        let confirmed = self.confirmed().block_exists_or_pruned(hash);
        Some(DetailedBlock {
            block,
            amount,
            confirmed,
        })
    }

    fn receivable_upper_bound(&self, account: Account) -> AnyReceivableIterator {
        match account.inc() {
            None => AnyReceivableIterator::new(
                &self.store.pending,
                Default::default(),
                None,
                None,
            ),
            Some(account) => AnyReceivableIterator::new(
                &self.store.pending,
                account,
                None,
                Some(BlockHash::zero()),
            ),
        }
    }

    fn receivable_lower_bound(&self, account: Account) -> AnyReceivableIterator {
        AnyReceivableIterator::new(
            &self.store.pending,
            account,
            None,
            Some(BlockHash::zero()),
        )
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

    fn get_final_vote(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        self.store.final_vote.get(&(), root)
    }
}

pub struct AnyReceivableIterator {
    returned_account: Option<Account>,
    items: Vec<(PendingKey, PendingInfo)>,
    index: usize,
}

impl AnyReceivableIterator {
    pub fn new(
        pending: &BtreePendingStore,
        _requested_account: Account,
        returned_account: Option<Account>,
        _next_hash: Option<BlockHash>,
    ) -> Self {
        // Btree doesn't have range iterators yet, returning empty for now
        let items = pending.iter(); 
        Self {
            returned_account,
            items,
            index: 0,
        }
    }
}

impl Iterator for AnyReceivableIterator {
    type Item = (PendingKey, PendingInfo);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.items.len() {
            return None;
        }
        let (key, info) = &self.items[self.index];
        self.index += 1;
        
        match self.returned_account {
            Some(acc) => {
                if acc == key.receiving_account {
                    Some((key.clone(), info.clone()))
                } else {
                    None
                }
            }
            None => {
                self.returned_account = Some(key.receiving_account);
                Some((key.clone(), info.clone()))
            }
        }
    }
}
