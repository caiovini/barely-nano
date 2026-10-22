use alloc::sync::Arc;
use alloc::collections::BTreeSet;
use spin::Mutex;

use rsnano_core::{Account, Block, BlockType, SavedBlock};
use rsnano_ledger::{AnySet, BlockError, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{DetailType, StatType, Stats};

use super::state::BootstrapState;
use crate::block_processing::{BlockSource, ProcessedResult};

/// Inspects a processed block and adjusts the bootstrap state accordingly
pub(super) struct BlockInspector {
    state: Arc<Mutex<BootstrapState>>,
    ledger: Arc<Ledger>,
    stats: Arc<Stats>,
    clock: Arc<SteadyClock>,
}

impl BlockInspector {
    pub(super) fn new(
        state: Arc<Mutex<BootstrapState>>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            state,
            ledger,
            stats,
            clock,
        }
    }

    pub fn inspect(&self, batch: &[ProcessedResult]) {
        let mut state = self.state.lock();
        let any = self.ledger.any();
        let mut failed_accounts = BTreeSet::new();
        
        for result in batch {
            let account = self.get_account(&any, &result.block, &result.saved_block);
            let is_cascading = failed_accounts.contains(&account);
            
            if result.status.is_err() {
                failed_accounts.insert(account);
            }
            
            self.inspect_block(&mut state, result, &account, is_cascading);
        }

        if state.candidate_accounts.blocked_len() > 0 {
            let unblocked = state.candidate_accounts.unblock_fulfilled(&self.ledger);
            if unblocked > 0 {
                self.stats.add(
                    StatType::BootstrapAccountSets,
                    DetailType::PriorityUnblocked,
                    unblocked as u64,
                );
            }
        }
    }

    fn get_account(
        &self,
        any: &dyn AnySet,
        block: &Block,
        saved_block: &Option<SavedBlock>,
    ) -> Account {
        match saved_block {
            Some(b) => b.account(),
            None => block
                .account_field()
                .unwrap_or_else(|| any.block_account(&block.previous()).unwrap_or_default()),
        }
    }

    /// Inspects a block that has been processed by the block processor
    /// - Marks an account as blocked if the result code is gap source as there is no reason request additional blocks for this account until the dependency is resolved
    /// - Marks an account as forwarded if it has been recently referenced by a block that has been inserted.
    fn inspect_block(
        &self,
        state: &mut BootstrapState,
        result: &ProcessedResult,
        account: &Account,
        is_cascading: bool,
    ) {
        if is_cascading {
            return;
        }

        let hash = result.block.hash();

        match result.status {
            Ok(()) => {
                // Reward peer for providing a valid block
                state.scoring.reward(result.channel_id, 10);

                // Progress blocks from live traffic don't need further bootstrapping
                if result.source != BlockSource::Live {
                    let saved_block = result.saved_block.clone().unwrap();
                    let account = saved_block.account();
                    // If we've inserted any block in to an account, unmark it as blocked
                    if state.candidate_accounts.unblock(account, None) {
                        self.stats
                            .inc(StatType::BootstrapAccountSets, DetailType::Unblock);
                        self.stats.inc(
                            StatType::BootstrapAccountSets,
                            DetailType::PriorityUnblocked,
                        );
                    } else {
                        self.stats
                            .inc(StatType::BootstrapAccountSets, DetailType::UnblockFailed);
                    }

                    // Unblock any accounts waiting for this specific block hash
                    let unblocked = state.candidate_accounts.unblock_hash(&hash);
                    if unblocked > 0 {
                        self.stats.add(
                            StatType::BootstrapAccountSets,
                            DetailType::PriorityUnblocked,
                            unblocked as u64,
                        );
                    }

                    let boost = super::state::Priority::new(1024.0);
                    let _ = state.candidate_accounts.priority_up_with(&account, boost);

                    if saved_block.is_send() {
                        if let Some(destination) = saved_block.destination() {
                            if !destination.is_zero() {
                                let _ = state.candidate_accounts.unblock(destination, Some(hash));
                                if state.candidate_accounts.priority_set_initial(&destination) {
                                    self.stats
                                        .inc(StatType::BootstrapAccountSets, DetailType::PriorityInsert);
                                }
                            }
                        }
                    }
                }
            }
            Err(BlockError::GapSource) => {

                // Prevent malicious live traffic from filling up the blocked set
                if result.source == BlockSource::Bootstrap {
                    let source = result.block.source_or_link();

                    if !account.is_zero() && !source.is_zero() {
                        // Mark account as blocked because it is missing the source block
                        let blocked =
                            state
                                .candidate_accounts
                                .block(*account, source, self.clock.now());
                        if blocked {
                            self.stats.inc(
                                StatType::BootstrapAccountSets,
                                DetailType::PriorityEraseBlock,
                            );
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::Block);
                        } else {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::BlockFailed);
                        }
                    }
                }
            }
            Err(BlockError::GapPrevious) => {
                // Prevent live traffic from evicting accounts from the priority list
                if result.source == BlockSource::Live
                    && !state.candidate_accounts.priority_half_full()
                    && !state.candidate_accounts.blocked_half_full()
                {
                    if result.block.block_type() == BlockType::State {
                        let account = result.block.account_field().unwrap();
                        if state.candidate_accounts.priority_set_initial(&account) {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::PriorityInsert);
                        } else {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::PrioritizeFailed);
                        }
                    }
                }
            }
            Err(BlockError::GapEpochOpenPending) => {
                // Epoch open blocks for accounts that don't have any pending blocks yet
                if state.candidate_accounts.priority_erase(account) {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::PriorityErase);
                }
            }
            Err(BlockError::BadSignature) => {
                // Severe punishment for invalid data
                state.scoring.punish(result.channel_id, 500);

                if state.candidate_accounts.priority_erase(account) {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::PriorityErase);
                }
            }
            Err(BlockError::Fork) => {
                // Fork means our local ledger disagrees. Do not punish the peer, as our ledger might be the one that is wrong.
                if state.candidate_accounts.priority_erase(account) {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::PriorityErase);
                }
            }
            Err(BlockError::Old) => {
                // Ignore old blocks, don't erase from priority
            }
            _ => {
                // Any other error (BalanceMismatch, InsufficientWork, etc.) might just be local state inconsistencies.
                // Apply a mild de-prioritization rather than a ban.
                state.scoring.punish(result.channel_id, 10);

                if state.candidate_accounts.priority_erase(account) {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::PriorityErase);
                }
            }
        }
    }
}
