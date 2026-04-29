use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::Ordering;

use rsnano_core::{BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_stats::{DetailType, Direction, StatType, Stats};
use rsnano_ledger_store::BtreeStore;

use crate::LedgerConstants;

/// Cements Blocks in the ledger
pub(crate) struct BlockCementer<'a> {
    constants: &'a LedgerConstants,
    store: &'a BtreeStore,
    stats: &'a Stats,
}

impl<'a> BlockCementer<'a> {
    pub(crate) fn new(
        store: &'a BtreeStore,
        constants: &'a LedgerConstants,
        stats: &'a Stats,
    ) -> Self {
        Self {
            store,
            constants,
            stats,
        }
    }

    pub(crate) fn confirm(
        &self,
        target_hash: BlockHash,
        max_blocks: usize,
    ) -> Vec<SavedBlock> {
        let mut result = Vec::new();

        let mut stack = VecDeque::new();
        stack.push_back(target_hash);
        while let Some(&hash) = stack.back() {
            let Some(block) = self.store.block.get(&hash) else {
                stack.pop_back();
                continue;
            };

            let dependents =
                block.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account);
            for dependent in dependents.iter() {
                if !dependent.is_zero() && !self.is_confirmed(dependent) {
                    self.stats.inc(
                        StatType::ConfirmationHeight,
                        DetailType::DependentUnconfirmed,
                    );

                    stack.push_back(*dependent);

                    if stack.len() > max_blocks {
                        stack.pop_front();
                    }
                }
            }

            if stack.back() == Some(&hash) {
                stack.pop_back();
                if !self.is_confirmed(&hash) {
                    let conf_height = ConfirmationHeightInfo::new(block.height(), block.hash());

                    self.store
                        .confirmation_height
                        .put(&block.account(), &conf_height);
                    self.store
                        .cache
                        .confirmed_count
                        .fetch_add(1, Ordering::SeqCst);

                    self.stats.add_dir(
                        StatType::ConfirmationHeight,
                        DetailType::BlocksConfirmed,
                        Direction::In,
                        1,
                    );

                    result.push(block);
                }
            }

            if result.len() >= max_blocks {
                break;
            }
        }
        result
    }

    fn is_confirmed(&self, hash: &BlockHash) -> bool {
        if self.store.pruned.exists(hash) {
            return true;
        }
        let Some(block) = self.store.block.get(hash) else {
            return false;
        };
        let Some(info) = self.store.confirmation_height.get(&block.account()) else {
            return false;
        };

        block.height() <= info.height
    }
}
