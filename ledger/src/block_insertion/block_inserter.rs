
use rsnano_core::{
    Account, AccountInfo, Amount, Block, BlockSideband, PendingInfo, PendingKey, SavedBlock,
};

use crate::Ledger;

#[derive(Debug, PartialEq)]
pub(crate) struct BlockInsertInstructions {
    pub account: Account,
    pub old_account_info: AccountInfo,
    pub set_account_info: AccountInfo,
    pub delete_pending: Option<PendingKey>,
    pub insert_pending: Option<(PendingKey, PendingInfo)>,
    pub set_sideband: BlockSideband,
    pub is_epoch_block: bool,
}

/// Inserts a new block into the ledger
pub(crate) struct BlockInserter<'a> {
    ledger: &'a Ledger,
    block: &'a Block,
    instructions: &'a BlockInsertInstructions,
}

impl<'a> BlockInserter<'a> {
    pub(crate) fn new(
        ledger: &'a Ledger,
        _txn: &'a mut (),
        block: &'a Block,
        instructions: &'a BlockInsertInstructions,
    ) -> Self {
        Self {
            ledger,
            block,
            instructions,
        }
    }

    pub(crate) fn insert(&mut self) -> Option<SavedBlock> {
        if self.account_changed_since_validation() {
            return None;
        }

        let sideband = self.instructions.set_sideband.clone();
        let saved_block = SavedBlock::new(self.block.clone(), sideband);
        
        tracing::debug!(
            "BlockInserter: inserting block {} for account {}",
            saved_block.hash(),
            saved_block.account().encode_account()
        );

        self.ledger.store.block.put(&saved_block);
        
        if !saved_block.previous().is_zero() {
            self.ledger.store.successors.put(
                &saved_block.previous(),
                &saved_block.hash(),
            );
        }
        
        self.update_account();
        self.delete_old_pending_info();
        self.insert_new_pending_info();
        self.update_representative_cache();

        Some(saved_block)
    }

    fn account_changed_since_validation(&mut self) -> bool {
        let account_info = self.get_current_account_info();
        let account_changed_since_validation =
            account_info.head != self.instructions.old_account_info.head;
        account_changed_since_validation
    }

    fn get_current_account_info(&mut self) -> AccountInfo {
        let account_info = self
            .ledger
            .store
            .account
            .get(&self.instructions.account)
            .unwrap_or_default();
        account_info
    }

    fn update_account(&mut self) {
        self.ledger.update_account(
            &mut (),
            &self.instructions.account,
            &self.instructions.old_account_info,
            &self.instructions.set_account_info,
        );
    }

    fn delete_old_pending_info(&mut self) {
        if let Some(key) = &self.instructions.delete_pending {
            self.ledger.store.pending.del(key);
        }
    }

    fn insert_new_pending_info(&mut self) {
        if let Some((key, info)) = &self.instructions.insert_pending {
            self.ledger.store.pending.put(key, info);
        }
    }

    fn update_representative_cache(&mut self) {
        if !self.instructions.old_account_info.head.is_zero() {
            // Move existing representation & add in amount delta
            self.ledger.rep_weights_updater.representation_add_dual(
                &mut (),
                self.instructions.old_account_info.representative,
                Amount::zero().wrapping_sub(self.instructions.old_account_info.balance),
                self.instructions.set_account_info.representative,
                self.instructions.set_account_info.balance,
            );
        } else {
            // Add in amount delta only
            self.ledger.rep_weights_updater.representation_add(
                &mut (),
                self.instructions.set_account_info.representative,
                self.instructions.set_account_info.balance,
            );
        }
    }
}
