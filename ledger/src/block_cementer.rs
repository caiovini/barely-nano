use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use rsnano_core::{Account, BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_stats::{DetailType, Direction, StatType, Stats};
use rsnano_ledger_store::BtreeStore;

use crate::{CementingObserver, LedgerConstants};

const MAX_CEMENT_STACK: usize = 131_072;

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
        struct CollectObserver {
            out: Vec<SavedBlock>,
        }
        impl CementingObserver for CollectObserver {
            fn batch_confirmed(&mut self, blocks: Vec<(SavedBlock, BlockHash)>) {
                self.out.extend(blocks.into_iter().map(|(b, _)| b));
            }
        }
        let stopped = AtomicBool::new(false);
        let mut obs = CollectObserver { out: Vec::new() };
        self.confirm_batch_internal([&target_hash], &stopped, max_blocks, true, &mut obs);
        obs.out
    }

    pub(crate) fn confirm_batch<'b, O>(
        &self,
        batch: impl IntoIterator<Item = &'b BlockHash>,
        stopped: &AtomicBool,
        max_blocks: usize,
        cementing_observer: &mut O,
    ) where
        O: CementingObserver,
    {
        self.confirm_batch_internal(batch, stopped, max_blocks, false, cementing_observer);
    }

    fn confirm_batch_internal<'b, O>(
        &self,
        batch: impl IntoIterator<Item = &'b BlockHash>,
        stopped: &AtomicBool,
        max_blocks: usize,
        stop_after_one_chunk: bool,
        cementing_observer: &mut O,
    ) where
        O: CementingObserver,
    {
        let mut conf_heights: hashbrown::HashMap<Account, Option<ConfirmationHeightInfo>> =
            hashbrown::HashMap::new();
        let mut visited: hashbrown::HashSet<BlockHash> = hashbrown::HashSet::new();
        let mut dirty_heights: hashbrown::HashMap<Account, ConfirmationHeightInfo> =
            hashbrown::HashMap::new();
        let mut confirmed: Vec<(SavedBlock, BlockHash)> = Vec::new();
        let mut stack: VecDeque<(SavedBlock, bool)> = VecDeque::new();
        let stack_cap = max_blocks.max(MAX_CEMENT_STACK);

        for &target_hash in batch.into_iter() {
            if stopped.load(Ordering::Relaxed) {
                break;
            }
            if visited.contains(&target_hash) {
                continue;
            }
            let Some(target_block) = self.store.block.get(&target_hash) else {
                continue;
            };
            if Self::is_block_confirmed_cached(self.store, &mut conf_heights, &target_block) {
                visited.insert(target_hash);
                continue;
            }

            stack.clear();
            stack.push_back((target_block, false));
            while !stack.is_empty() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let hash = stack.back().unwrap().0.hash();
                if visited.contains(&hash) {
                    stack.pop_back();
                    continue;
                }

                if !stack.back().unwrap().1 {
                    let dependents = {
                        let (block, dependents_checked) = stack.back_mut().unwrap();
                        *dependents_checked = true;
                        block.dependent_blocks(
                            &self.constants.epochs,
                            &self.constants.genesis_account,
                        )
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
                                    if stack.len() > stack_cap {
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

                    confirmed.push((block, target_hash));
                }

                if confirmed.len() >= max_blocks {
                    for (account, conf_height) in dirty_heights.drain() {
                        self.store
                            .confirmation_height
                            .put(&account, &conf_height);
                    }
                    cementing_observer.batch_confirmed(core::mem::take(&mut confirmed));
                    if stop_after_one_chunk {
                        return;
                    }
                }
            }
        }

        for (account, conf_height) in dirty_heights.drain() {
            self.store
                .confirmation_height
                .put(&account, &conf_height);
        }
        if !confirmed.is_empty() {
            cementing_observer.batch_confirmed(confirmed);
        }
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
