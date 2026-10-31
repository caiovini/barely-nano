use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::Ordering;

use rsnano_core::{Account, BlockHash, ConfirmationHeightInfo, SavedBlock};
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
        let Some(target_block) = self.store.block.get(&target_hash) else {
            return Vec::new();
        };

        let mut conf_heights: hashbrown::HashMap<Account, Option<ConfirmationHeightInfo>> =
            hashbrown::HashMap::new();
        if Self::is_block_confirmed_cached(self.store, &mut conf_heights, &target_block) {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut visited = hashbrown::HashSet::new();
        let mut dirty_heights: hashbrown::HashMap<Account, ConfirmationHeightInfo> =
            hashbrown::HashMap::new();

        let mut stack: VecDeque<(SavedBlock, bool)> = VecDeque::new();
        stack.push_back((target_block, false));
        while !stack.is_empty() {
            let hash = stack.back().unwrap().0.hash();
            if visited.contains(&hash) {
                stack.pop_back();
                continue;
            }

            if !stack.back().unwrap().1 {
                let dependents = {
                    let (block, dependents_checked) = stack.back_mut().unwrap();
                    *dependents_checked = true;
                    block.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
                };

                let mut pushed = false;
                for dependent in dependents.iter() {
                    if !dependent.is_zero() && !visited.contains(dependent) {
                        if let Some(dep_block) = self.store.block.get(dependent) {
                            if !Self::is_block_confirmed_cached(
                                self.store,
                                &mut conf_heights,
                                &dep_block,
                            ) {
                                self.stats.inc(
                                    StatType::ConfirmationHeight,
                                    DetailType::DependentUnconfirmed,
                                );

                                stack.push_back((dep_block, false));
                                if stack.len() > max_blocks {
                                    stack.pop_front();
                                }
                                pushed = true;
                            } else {
                                visited.insert(*dependent);
                            }
                        } else {
                            visited.insert(*dependent);
                        }
                    }
                }

                if pushed {
                    continue;
                }
            }

            let (block, _) = stack.pop_back().unwrap();
            visited.insert(hash);
            if !Self::is_block_confirmed_cached(self.store, &mut conf_heights, &block) {
                let conf_height = ConfirmationHeightInfo::new(block.height(), hash);
                let account = block.account();

                conf_heights.insert(account, Some(conf_height.clone()));
                dirty_heights.insert(account, conf_height);
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

            if result.len() >= max_blocks {
                break;
            }
        }

        for (account, conf_height) in dirty_heights {
            self.store
                .confirmation_height
                .put(&account, &conf_height);
        }

        result
    }

    #[inline(always)]
    fn is_block_confirmed_cached(
        store: &BtreeStore,
        conf_heights: &mut hashbrown::HashMap<Account, Option<ConfirmationHeightInfo>>,
        block: &SavedBlock,
    ) -> bool {
        let account = block.account();
        let entry = conf_heights
            .entry(account)
            .or_insert_with(|| store.confirmation_height.get(&account));
        if let Some(info) = entry {
            block.height() <= info.height
        } else {
            false
        }
    }
}
