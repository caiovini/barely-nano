use crate::{AnySet, LedgerConstants};
use rsnano_core::{Block, BlockBase, BlockHash, DependentBlocks, SavedBlock, StateBlock};

/// Finds all dependent blocks for a given block.
/// There can be at most two dependencies per block, namely "previous" and "link/source".
pub struct DependentBlocksFinder<'a, T>
where
    T: AnySet,
{
    any: &'a T,
    constants: &'a LedgerConstants,
}

impl<'a, T> DependentBlocksFinder<'a, T>
where
    T: AnySet,
{
    pub fn new(any: &'a T, constants: &'a LedgerConstants) -> Self {
        Self { any, constants }
    }

    pub fn find_dependent_blocks(&self, block: &SavedBlock) -> DependentBlocks {
        block.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
    }

    pub fn find_dependent_blocks_for_unsaved_block(&self, block: &Block) -> DependentBlocks {
        match block {
            Block::LegacySend(b) => b.dependent_blocks(),
            Block::LegacyChange(b) => b.dependent_blocks(),
            Block::LegacyReceive(b) => b.dependent_blocks(),
            Block::LegacyOpen(b) => b.dependent_blocks(&self.constants.genesis_account),
            // a ledger lookup is needed if it is a state block!
            Block::State(state) => {
                let linked_block = if self.is_receive_or_change(state) {
                    state.link().into()
                } else {
                    BlockHash::zero()
                };
                DependentBlocks::new(block.previous(), linked_block)
            }
        }
    }

    fn is_receive_or_change(&self, state: &StateBlock) -> bool {
        !self.constants.epochs.is_epoch_link(&state.link()) && !self.is_send(state)
    }

    // This function is used in place of block.is_send() as it is tolerant to the block not having the sideband information loaded
    // This is needed for instance in vote generation on forks which have not yet had sideband information attached
    fn is_send(&self, block: &StateBlock) -> bool {
        if block.previous().is_zero() {
            return false;
        }

        let previous_balance = self
            .any
            .block_balance(&block.previous())
            .unwrap_or_default();
        block.balance() < previous_balance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LedgerSet;
    use hashbrown::HashMap;
    use rsnano_core::{
        epoch_v1_link, utils::BlockPriority, Account, AccountInfo, Amount, DetailedBlock,
        PendingInfo, PendingKey, QualifiedRoot, TestBlockBuilder,
    };

    #[derive(Default)]
    struct StubBalanceAnySet {
        balances: HashMap<BlockHash, Amount>,
    }

    impl LedgerSet for StubBalanceAnySet {
        fn block_exists(&self, hash: &BlockHash) -> bool {
            self.balances.contains_key(hash)
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

    impl AnySet for StubBalanceAnySet {
        fn should_refresh(&self) -> bool {
            false
        }
        fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool {
            self.balances.contains_key(hash)
        }
        fn get_block(&self, _hash: &BlockHash) -> Option<SavedBlock> {
            None
        }
        fn block_balance(&self, hash: &BlockHash) -> Option<Amount> {
            self.balances.get(hash).cloned()
        }
        fn receivable_exists(&self, _account: Account) -> bool {
            false
        }
        fn confirmed(&self) -> crate::BorrowingConfirmedSet<'_> {
            unimplemented!()
        }
        fn dependent_blocks(&self, _block: &SavedBlock) -> DependentBlocks {
            unimplemented!()
        }
        fn dependents_confirmed(&self, _block: &SavedBlock) -> bool {
            unimplemented!()
        }
        fn dependents_confirmed_for_unsaved_block(&self, _block: &Block) -> bool {
            unimplemented!()
        }
        fn block_successor(&self, _hash: &BlockHash) -> Option<BlockHash> {
            unimplemented!()
        }
        fn block_successor_by_qualified_root(&self, _root: &QualifiedRoot) -> Option<BlockHash> {
            unimplemented!()
        }
        fn block_priority(&self, _block: &SavedBlock) -> BlockPriority {
            unimplemented!()
        }
        fn previous_block(&self, _block: &SavedBlock) -> Option<SavedBlock> {
            unimplemented!()
        }
        fn get_pending(&self, _key: &PendingKey) -> Option<PendingInfo> {
            unimplemented!()
        }
        fn account_head(&self, _account: &Account) -> Option<BlockHash> {
            unimplemented!()
        }
        fn block_account(&self, _hash: &BlockHash) -> Option<Account> {
            unimplemented!()
        }
        fn representative_block_hash(&self, _hash: &BlockHash) -> BlockHash {
            unimplemented!()
        }
        fn find_receive_block_by_send_hash(
            &self,
            _destination: &Account,
            _send_block_hash: &BlockHash,
        ) -> Option<SavedBlock> {
            unimplemented!()
        }
        fn linked_account(&self, _block: &SavedBlock) -> Option<Account> {
            unimplemented!()
        }
        fn block_amount(&self, _hash: &BlockHash) -> Option<Amount> {
            unimplemented!()
        }
        fn block_amount_for(&self, _block: &SavedBlock) -> Option<Amount> {
            unimplemented!()
        }
        fn detailed_block(&self, _hash: &BlockHash) -> Option<DetailedBlock> {
            unimplemented!()
        }
        fn receivable_upper_bound(&self, _account: Account) -> crate::AnyReceivableIterator {
            unimplemented!()
        }
        fn receivable_lower_bound(&self, _account: Account) -> crate::AnyReceivableIterator {
            unimplemented!()
        }
        fn account_receivable_upper_bound(
            &self,
            _account: Account,
            _hash: BlockHash,
        ) -> crate::AnyReceivableIterator {
            unimplemented!()
        }
        fn get_final_vote(&self, _root: &QualifiedRoot) -> Option<BlockHash> {
            unimplemented!()
        }
    }

    #[test]
    fn unsaved_state_send_depends_only_on_previous() {
        let prev_hash = BlockHash::from(10);
        let mut any = StubBalanceAnySet::default();
        any.balances.insert(prev_hash, Amount::raw(1000));
        let constants = LedgerConstants::unit_test();
        let finder = DependentBlocksFinder::new(&any, &constants);

        let send_block = TestBlockBuilder::state()
            .previous(prev_hash)
            .balance(Amount::raw(600))
            .link(BlockHash::from(99))
            .build();

        assert_eq!(
            finder.find_dependent_blocks_for_unsaved_block(&send_block),
            DependentBlocks::new(prev_hash, BlockHash::zero())
        );
    }

    #[test]
    fn unsaved_state_receive_depends_on_previous_and_source() {
        let prev_hash = BlockHash::from(10);
        let source_hash = BlockHash::from(99);
        let mut any = StubBalanceAnySet::default();
        any.balances.insert(prev_hash, Amount::raw(1000));
        let constants = LedgerConstants::unit_test();
        let finder = DependentBlocksFinder::new(&any, &constants);

        let receive_block = TestBlockBuilder::state()
            .previous(prev_hash)
            .balance(Amount::raw(1500))
            .link(source_hash)
            .build();

        assert_eq!(
            finder.find_dependent_blocks_for_unsaved_block(&receive_block),
            DependentBlocks::new(prev_hash, source_hash)
        );
    }

    #[test]
    fn unsaved_state_epoch_depends_only_on_previous() {
        let prev_hash = BlockHash::from(10);
        let mut any = StubBalanceAnySet::default();
        any.balances.insert(prev_hash, Amount::raw(1000));
        let constants = LedgerConstants::unit_test();
        let finder = DependentBlocksFinder::new(&any, &constants);

        let epoch_block = TestBlockBuilder::state()
            .previous(prev_hash)
            .balance(Amount::raw(1000))
            .link(epoch_v1_link())
            .build();

        assert_eq!(
            finder.find_dependent_blocks_for_unsaved_block(&epoch_block),
            DependentBlocks::new(prev_hash, BlockHash::zero())
        );
    }
}
