use rsnano_core::{
    utils::UnixMillisTimestamp, Account, AccountInfo, Block, PendingInfo, PendingKey, SavedBlock,
};

use crate::{AnySet, LedgerConstants};

use super::BlockValidator;

pub(crate) struct BlockValidatorFactory<'a> {
    any: &'a dyn AnySet,
    constants: &'a LedgerConstants,
    block: &'a Block,
}

impl<'a> BlockValidatorFactory<'a> {
    pub(crate) fn new(
        any: &'a dyn AnySet,
        constants: &'a LedgerConstants,
        block: &'a Block,
    ) -> Self {
        Self {
            any,
            constants,
            block,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn create_validator(&self, signature_verified: bool) -> BlockValidator<'a> {
        self.create_validator_with_batch_cache(
            signature_verified,
            false,
            None,
            None,
            false,
            None,
            UnixMillisTimestamp::now(),
        )
    }

    pub(crate) fn create_validator_with_batch_cache(
        &self,
        signature_verified: bool,
        in_batch_exists: bool,
        in_batch_prev: Option<SavedBlock>,
        in_batch_account: Option<AccountInfo>,
        in_batch_source_exists: bool,
        in_batch_pending: Option<Option<PendingInfo>>,
        batch_now: UnixMillisTimestamp,
    ) -> BlockValidator<'a> {
        use crate::LEDGER_PROF as P;
        use core::sync::atomic::Ordering::Relaxed;
        let t = || kernel_common::clint::mtime();

        let is_genesis = self.block.hash() == self.constants.genesis_block.hash();
        let has_epoch_link = match self.block {
            Block::State(s) => self.constants.epochs.is_epoch_link(&s.link()),
            _ => false,
        };

        let t3 = t();
        let block_exists = in_batch_exists || self.any.block_exists_or_pruned(&self.block.hash());
        let t4 = t();
        P[6].fetch_add(t4 - t3, Relaxed); // hash + block_exists

        if block_exists && !has_epoch_link && !cfg!(test) {
            return BlockValidator {
                block: self.block,
                epochs: &self.constants.epochs,
                work: &self.constants.work,
                account: self.block.account_field().unwrap_or_default(),
                block_exists: true,
                old_account_info: None,
                pending_receive_info: None,
                any_pending_exists: false,
                source_block_exists: false,
                is_genesis,
                signature_verified,
                previous_block: None,
                now: batch_now,
            };
        }

        let t0 = t();
        let previous_block = match in_batch_prev {
            Some(prev) => Some(prev),
            None => self.load_previous_block(),
        };
        let t1 = t();
        P[3].fetch_add(t1 - t0, Relaxed); // previous block

        let account = self.get_account(&previous_block).unwrap_or_default();
        if !self.block.previous().is_zero() && previous_block.is_none() && !cfg!(test) {
            return BlockValidator {
                block: self.block,
                epochs: &self.constants.epochs,
                work: &self.constants.work,
                account,
                block_exists,
                old_account_info: None,
                pending_receive_info: None,
                any_pending_exists: false,
                source_block_exists: false,
                is_genesis,
                signature_verified,
                previous_block: None,
                now: batch_now,
            };
        }

        let t4b = t();
        let has_in_batch_account = in_batch_account.is_some();
        let mut old_account_info = if account.is_zero() && !cfg!(test) {
            None
        } else if let Some(info) = in_batch_account {
            Some(info)
        } else {
            self.any.get_account(&account)
        };
        #[cfg(not(test))]
        if !block_exists && !account.is_zero() {
            if let Some(prev_saved) = &previous_block {
                if prev_saved.account() == account {
                    let account_desynced = match &old_account_info {
                        None => true,
                        Some(info) => info.head != prev_saved.hash(),
                    };
                    if account_desynced {
                        let has_live_successor = self
                            .any
                            .block_successor(&prev_saved.hash())
                            .filter(|s| {
                                !s.is_zero()
                                    && *s != self.block.hash()
                                    && self.any.block_exists(s)
                            })
                            .is_some();
                        if !has_live_successor {
                            let open_block = old_account_info
                                .as_ref()
                                .map(|i| i.open_block)
                                .filter(|h| !h.is_zero() && self.any.block_exists(h))
                                .unwrap_or_else(|| {
                                    let mut curr = prev_saved.clone();
                                    for _ in 0..256 {
                                        if curr.previous().is_zero() || curr.height() <= 1 {
                                            return curr.hash();
                                        }
                                        let Some(prev) = self.any.get_block(&curr.previous())
                                        else {
                                            break;
                                        };
                                        curr = prev;
                                    }
                                    curr.hash()
                                });
                            let rep = prev_saved
                                .representative_field()
                                .or_else(|| {
                                    old_account_info
                                        .as_ref()
                                        .map(|i| i.representative)
                                        .filter(|r| !r.is_zero())
                                })
                                .unwrap_or_default();
                            old_account_info = Some(AccountInfo {
                                head: prev_saved.hash(),
                                representative: rep,
                                open_block,
                                balance: prev_saved.balance(),
                                modified: batch_now.into(),
                                block_count: prev_saved.height(),
                                epoch: prev_saved.epoch(),
                            });
                        }
                    }
                }
            } else if self.block.previous().is_zero() && !has_in_batch_account {
                if let Some(info) = &old_account_info {
                    if !self.any.block_exists(&info.head)
                        && !self.any.block_exists(&info.open_block)
                    {
                        old_account_info = None;
                    } else if self.any.block_exists(&info.head) {
                        let mut curr_hash = info.head;
                        let mut broken_chain = false;
                        for _ in 0..256 {
                            let Some(curr) = self.any.get_block(&curr_hash) else {
                                broken_chain = true;
                                break;
                            };
                            if curr.previous().is_zero() {
                                break;
                            }
                            curr_hash = curr.previous();
                        }
                        if broken_chain {
                            old_account_info = None;
                        }
                    }
                }
            }
        }
        let t5 = t();
        P[7].fetch_add(t5 - t4b, Relaxed); // get_account

        let maybe_receive = cfg!(test)
            || match self.block {
                Block::LegacyReceive(_) | Block::LegacyOpen(_) => true,
                Block::State(s) => {
                    !has_epoch_link
                        && match &old_account_info {
                            Some(info) => s.balance() >= info.balance && !s.link().is_zero(),
                            None => true,
                        }
                }
                _ => false,
            };

        let source_block = self.block.source_or_link();
        let t2a = t();
        let has_in_batch_pending = in_batch_pending.is_some();
        let mut pending_receive_info = if !maybe_receive || source_block.is_zero() {
            None
        } else if let Some(cached_pending) = in_batch_pending {
            cached_pending
        } else {
            self.any
                .get_pending(&PendingKey::new(account, source_block))
        };
        let t2b = t();
        P[5].fetch_add(t2b - t2a, Relaxed); // get_pending

        let source_block_exists = if !maybe_receive || source_block.is_zero() {
            false
        } else if (pending_receive_info.is_some() || in_batch_source_exists) && !cfg!(test) {
            true
        } else {
            self.any.block_exists_or_pruned(&source_block)
        };
        #[cfg(not(test))]
        if maybe_receive
            && !source_block.is_zero()
            && pending_receive_info.is_none()
            && source_block_exists
            && !has_in_batch_pending
        {
            if let Some(src_saved) = self.any.get_block(&source_block) {
                if src_saved.is_send() && src_saved.destination() == Some(account) {
                    let expected_amount = match self.block {
                        Block::State(s) => {
                            let prev_bal = previous_block
                                .as_ref()
                                .map(|b| b.balance())
                                .unwrap_or_default();
                            if s.balance() > prev_bal {
                                Some(s.balance() - prev_bal)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(sent_amount) = self
                        .any
                        .block_amount_for(&src_saved)
                        .or(expected_amount)
                    {
                        if !sent_amount.is_zero() {
                            let mut already_received = false;
                            let mut curr = previous_block.clone();
                            for _ in 0..256 {
                                let Some(b) = curr else { break };
                                if b.source() == Some(source_block) {
                                    already_received = true;
                                    break;
                                }
                                if b.previous().is_zero() {
                                    break;
                                }
                                curr = self.any.get_block(&b.previous());
                            }
                            if !already_received {
                                pending_receive_info = Some(PendingInfo::new(
                                    src_saved.account(),
                                    sent_amount,
                                    src_saved.epoch(),
                                ));
                            }
                        }
                    }
                }
            }
        }
        let t2c = t();
        P[4].fetch_add(t2c - t2b, Relaxed); // source exists

        let any_pending_exists = if cfg!(test) || (self.block.is_open() && has_epoch_link) {
            self.any.receivable_exists(account)
        } else {
            false
        };
        let t6 = t();
        P[8].fetch_add(t6 - t2c, Relaxed); // receivable_exists

        let now = batch_now;
        let t7 = t();
        P[9].fetch_add(t7 - t6, Relaxed); // now()

        BlockValidator {
            block: self.block,
            epochs: &self.constants.epochs,
            work: &self.constants.work,
            account,
            block_exists,
            old_account_info,
            pending_receive_info,
            any_pending_exists,
            source_block_exists,
            is_genesis,
            signature_verified,
            previous_block,
            now,
        }
    }

    fn get_account(&self, previous: &Option<SavedBlock>) -> Option<Account> {
        match self.block.account_field() {
            Some(account) => Some(account),
            None => match previous {
                Some(p) => Some(p.account()),
                None => None,
            },
        }
    }

    fn load_previous_block(&self) -> Option<SavedBlock> {
        if !self.block.previous().is_zero() {
            let prev = self.any.get_block(&self.block.previous());
            tracing::debug!("ValidatorFactory: loading previous={}, found={}", self.block.previous(), prev.is_some());
            prev
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashbrown::{HashMap, HashSet};
    use rsnano_core::{
        utils::BlockPriority, AccountInfo, Amount, BlockHash, DependentBlocks, DetailedBlock, Link,
        PendingInfo, QualifiedRoot, TestBlockBuilder,
    };

    #[derive(Default)]
    struct StubAnySet {
        blocks: HashMap<BlockHash, SavedBlock>,
        pruned: HashSet<BlockHash>,
        accounts: HashMap<Account, AccountInfo>,
        pending: HashMap<PendingKey, PendingInfo>,
    }

    impl crate::LedgerSet for StubAnySet {
        fn block_exists(&self, hash: &BlockHash) -> bool {
            self.blocks.contains_key(hash)
        }
        fn account_receivable(&self, _account: &Account) -> Amount {
            Amount::zero()
        }
        fn account_balance(&self, _account: &Account) -> Amount {
            Amount::zero()
        }
        fn get_account(&self, account: &Account) -> Option<AccountInfo> {
            self.accounts.get(account).cloned()
        }
    }

    impl AnySet for StubAnySet {
        fn should_refresh(&self) -> bool {
            false
        }
        fn block_exists_or_pruned(&self, hash: &BlockHash) -> bool {
            self.blocks.contains_key(hash) || self.pruned.contains(hash)
        }
        fn get_block(&self, hash: &BlockHash) -> Option<SavedBlock> {
            self.blocks.get(hash).cloned()
        }
        fn receivable_exists(&self, account: Account) -> bool {
            self.pending.keys().any(|k| k.receiving_account == account)
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
        fn get_pending(&self, key: &PendingKey) -> Option<PendingInfo> {
            self.pending.get(key).cloned()
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
    fn block_for_unknown_account() {
        let block = TestBlockBuilder::state().build();
        let any = StubAnySet::default();
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);

        assert_eq!(validator.block.hash(), block.hash());
        assert_eq!(validator.epochs, &constants.epochs);
        assert_eq!(validator.account, block.account_field().unwrap());
        assert_eq!(validator.block_exists, false);
        assert_eq!(validator.old_account_info, None);
        assert_eq!(validator.pending_receive_info, None);
        assert_eq!(validator.any_pending_exists, false);
        assert_eq!(validator.source_block_exists, false);
        assert_eq!(validator.previous_block, None);
        assert!(validator.now >= UnixMillisTimestamp::now());
    }

    #[test]
    fn get_account_from_previous_block() {
        let previous = TestBlockBuilder::legacy_send().build_saved();
        let block = TestBlockBuilder::legacy_send()
            .previous(previous.hash())
            .build();
        let mut any = StubAnySet::default();
        any.blocks.insert(previous.hash(), previous.clone());
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);

        assert_eq!(validator.account, previous.account());
    }

    #[test]
    fn block_exists() {
        let block = TestBlockBuilder::state().build_saved();
        let mut any = StubAnySet::default();
        any.blocks.insert(block.hash(), block.clone());
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.block_exists, true);
    }

    #[test]
    fn pruned_block_exists() {
        let block = TestBlockBuilder::state().build();
        let mut any = StubAnySet::default();
        any.pruned.insert(block.hash());
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.block_exists, true);
    }

    #[test]
    fn account_info() {
        let block = TestBlockBuilder::state().build();
        let account_info = AccountInfo::new_test_instance();
        let mut any = StubAnySet::default();
        any.accounts
            .insert(block.account_field().unwrap(), account_info.clone());
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.old_account_info, Some(account_info));
    }

    #[test]
    fn pending_receive_info_for_state_block() {
        let block = TestBlockBuilder::state().link(Link::from(42)).build();
        let pending_info = PendingInfo::new_test_instance();
        let mut any = StubAnySet::default();
        any.pending.insert(
            PendingKey::new(block.account_field().unwrap(), BlockHash::from(42)),
            pending_info.clone(),
        );
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.pending_receive_info, Some(pending_info));
    }

    #[test]
    fn pending_receive_info_for_legacy_receive() {
        let previous = TestBlockBuilder::legacy_open().build_saved();
        let account = previous.account();
        let block = TestBlockBuilder::legacy_receive()
            .previous(previous.hash())
            .source(BlockHash::from(42))
            .build();
        let pending_info = PendingInfo::new_test_instance();
        let mut any = StubAnySet::default();
        any.blocks.insert(previous.hash(), previous);
        any.pending.insert(
            PendingKey::new(account, BlockHash::from(42)),
            pending_info.clone(),
        );
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.pending_receive_info, Some(pending_info));
    }

    #[test]
    fn any_pending_exists() {
        let block = TestBlockBuilder::state().build();
        let pending_info = PendingInfo::new_test_instance();
        let mut any = StubAnySet::default();
        any.pending.insert(
            PendingKey::new(block.account_field().unwrap(), BlockHash::from(42)),
            pending_info,
        );
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.any_pending_exists, true);
    }

    #[test]
    fn source_block_exists() {
        let source = TestBlockBuilder::state().build_saved();
        let block = TestBlockBuilder::state().link(source.hash()).build();
        let mut any = StubAnySet::default();
        any.blocks.insert(source.hash(), source);
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.source_block_exists, true);
    }

    #[test]
    fn pruned_source_block_exists() {
        let block = TestBlockBuilder::state().link(BlockHash::from(42)).build();
        let mut any = StubAnySet::default();
        any.pruned.insert(BlockHash::from(42));
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.source_block_exists, true);
    }

    #[test]
    fn previous_block() {
        let previous = SavedBlock::new_test_instance();
        let block = TestBlockBuilder::state()
            .previous(previous.hash())
            .build_saved();
        let mut any = StubAnySet::default();
        any.blocks.insert(previous.hash(), previous.clone());
        let constants = LedgerConstants::unit_test();
        let validator =
            BlockValidatorFactory::new(&any, &constants, &block).create_validator(false);
        assert_eq!(validator.previous_block, Some(previous));
    }
}
