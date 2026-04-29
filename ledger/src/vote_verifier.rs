use alloc::collections::VecDeque;

use rsnano_core::{BlockHash, Root};
use rsnano_ledger_store::BtreeStore;

use crate::{AnySet, BorrowingAnySet, LedgerConstants, OwningAnySet};

/// Verifies whether a vote (or a final vote) can be generated for a given block
pub(crate) struct VoteVerifier<'a> {
    pub constants: &'a LedgerConstants,
    pub store: &'a BtreeStore,
}

impl<'a> VoteVerifier<'a> {
    pub fn verify_votes(
        &self,
        candidates: VecDeque<(Root, BlockHash)>,
        is_final: bool,
    ) -> VecDeque<(Root, BlockHash)> {
        let mut verified = VecDeque::new();

        if is_final {
            for (root, hash) in &candidates {
                if self.should_vote_final(root, hash) {
                    verified.push_back((*root, *hash));
                }
            }
        } else {
            let any = OwningAnySet::new(self.store, self.constants);
            for (root, hash) in &candidates {
                if self.should_vote_non_final(&any, root, hash) {
                    verified.push_back((*root, *hash));
                }
            }
        };

        verified
    }

    fn should_vote_non_final(&self, any: &impl AnySet, root: &Root, hash: &BlockHash) -> bool {
        let Some(block) = any.get_block(hash) else {
            return false;
        };
        debug_assert!(block.root() == *root);
        any.dependents_confirmed(&block)
    }

    fn should_vote_final(&self, root: &Root, hash: &BlockHash) -> bool {
        let any = BorrowingAnySet {
            constants: &self.constants,
            store: self.store,
            tx: &(),
        };
        let Some(block) = any.get_block(hash) else {
            return false;
        };
        debug_assert!(block.root() == *root);
        if any.dependents_confirmed(&block) {
            self.store.final_vote.put(&block.qualified_root(), hash);
            true
        } else {
            false
        }
    }
}
