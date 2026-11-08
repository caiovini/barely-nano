use alloc::boxed::Box;
use alloc::sync::Arc;
use spin::Mutex;

use rsnano_messages::BlocksAckPayload;
use rsnano_network::ChannelId;
use rsnano_stats::{DetailType, Direction, StatType, Stats};
use rsnano_ledger::Ledger;

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue, BlockSource},
    bootstrap::state::{BootstrapState, PriorityDownResult, RunningQuery, VerifyResult},
};

pub(crate) struct BlockAckProcessor {
    state: Arc<Mutex<BootstrapState>>,
    stats: Arc<Stats>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    #[allow(dead_code)]
    ledger: Arc<Ledger>,
}

impl BlockAckProcessor {
    pub(crate) fn new(
        state: Arc<Mutex<BootstrapState>>,
        stats: Arc<Stats>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            state,
            stats,
            block_processor_queue,
            ledger,
        }
    }

    pub fn process(
        &self,
        query: &RunningQuery,
        response: BlocksAckPayload,
        channel_id: ChannelId,
    ) -> bool {
        self.stats
            .inc(StatType::BootstrapProcess, DetailType::Blocks);

        let t_proc = kernel_common::clint::mtime();
        let mut result = query.verify_blocks(&response);
        #[cfg(not(test))]
        if result == VerifyResult::NothingNew
            && query.query_type == crate::bootstrap::state::QueryType::BlocksByHash
            && response.blocks().len() == 1
            && !query.hash.is_zero()
            && !self.ledger.store.block.exists(&query.hash)
        {
            result = VerifyResult::Ok;
        }
        #[cfg(not(test))]
        Self::diag_log(query, &response, &result);
        let ret = match result {
            VerifyResult::Ok => {
                self.process_valid_blocks(query, response, channel_id);
                true
            }
            VerifyResult::NothingNew => {
                self.process_empty_response(query, channel_id);
                true
            }
            VerifyResult::Invalid => {
                self.stats
                    .inc(StatType::BootstrapVerifyBlocks, DetailType::Invalid);
                #[cfg(not(test))]
                if !query.account.is_zero() {
                    let mut guard = self.state.lock();
                    guard.candidate_accounts.priority_down(&query.account);
                    guard.candidate_accounts.reset_last_request(&query.account);
                }
                false
            }
        };
        self.stats.add(
            StatType::BootstrapVerifyBlocks,
            DetailType::ProcessMs,
            (kernel_common::clint::mtime() - t_proc) * 1000 / kernel_common::clint::FREQ,
        );
        ret
    }

    /// Diagnostics: logs the first replies that carry no usable new blocks (empty / start-only /
    /// invalid), so a stuck account can be traced to its peer replies. Samples 1 in 50.
    #[cfg(not(test))]
    fn diag_log(query: &RunningQuery, response: &BlocksAckPayload, result: &VerifyResult) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        use core::sync::atomic::{AtomicUsize, Ordering};
        static LOGGED: AtomicUsize = AtomicUsize::new(0);
        let kind = match result {
            VerifyResult::Ok => return,
            VerifyResult::NothingNew => "nothing_new",
            VerifyResult::Invalid => "invalid",
        };
        if LOGGED.fetch_add(1, Ordering::Relaxed) % 50 != 0 {
            return;
        }
        let qtype = if query.query_type == crate::bootstrap::state::QueryType::BlocksByHash {
            "by_hash"
        } else {
            "by_account"
        };
        let first_hash = response.blocks().front().map(|b| b.hash());
        tracing::debug!(
            "[blkack] {} type={} account={} start={} req_count={} resp_len={} first={:?}",
            kind,
            qtype,
            query.account.encode_account(),
            query.hash,
            query.count,
            response.blocks().len(),
            first_hash
        );
    }

    fn process_valid_blocks(&self, query: &RunningQuery, response: BlocksAckPayload, channel_id: ChannelId) {
        self.stats
            .inc(StatType::BootstrapVerifyBlocks, DetailType::Ok);
        let response_len = response.blocks().len();
        self.stats.add_dir(
            StatType::Bootstrap,
            DetailType::Blocks,
            Direction::In,
            response_len as u64,
        );

        let is_full_batch = response_len >= query.count as usize;
        let mut blocks = response.into_blocks();

        #[cfg(test)]
        let start_exists = true;
        #[cfg(not(test))]
        let start_exists = query.hash.is_zero() || self.ledger.store.block.exists(&query.hash);

        // Avoid re-processing the block we already have
        assert!(blocks.len() >= 1);
        #[cfg(not(test))]
        let mut popped_start_balance = None;
        if start_exists && blocks.front().unwrap().hash() == query.start.into() {
            let front = blocks.pop_front().unwrap();
            #[cfg(not(test))]
            {
                popped_start_balance = front.balance_field();
            }
        }
        if blocks.is_empty() {
            #[cfg(not(test))]
            if !query.account.is_zero() && !query.hash.is_zero() {
                let mut guard = self.state.lock();
                if guard.candidate_accounts.gap_target(&query.account) == Some(query.hash) {
                    guard.candidate_accounts.clear_gap_target(&query.account);
                }
            }
            self.process_empty_response(query, channel_id);
            return;
        }

        let account = query.account;
        #[cfg(not(test))]
        let mut splice_successor: Option<(rsnano_core::BlockHash, rsnano_core::BlockHash)> = None;
        #[cfg(not(test))]
        let mut stopped_at_missing_source = false;
        #[cfg(not(test))]
        let forward_cursor;
        #[cfg(not(test))]
        if !start_exists {
            let first_prev = blocks.front().unwrap().previous();
            if !first_prev.is_zero() && !self.ledger.store.block.exists(&first_prev) {
                if !account.is_zero() {
                    let has_existing_after = blocks
                        .iter()
                        .skip(1)
                        .any(|b| self.ledger.store.block.exists(&b.hash()));
                    let stepped_back = if has_existing_after || blocks.len() == 1 {
                        let mut guard = self.state.lock();
                        guard.candidate_accounts.set_gap_target(account, first_prev)
                    } else {
                        false
                    };
                    if !stepped_back {
                        let forward_start = self.ledger.find_forward_gap_start(&account, false);
                        let mut guard = self.state.lock();
                        if !forward_start.is_zero() {
                            guard
                                .candidate_accounts
                                .set_gap_target(account, forward_start);
                        } else {
                            guard.candidate_accounts.clear_gap_target(&account);
                        }
                        guard.candidate_accounts.reset_last_request(&account);
                        guard.candidate_accounts.priority_set_safe(&account);
                    } else {
                        let mut guard = self.state.lock();
                        guard.candidate_accounts.reset_last_request(&account);
                        guard.candidate_accounts.priority_set_safe(&account);
                    }
                }
                return;
            } else {
                if !account.is_zero() {
                    let mut guard = self.state.lock();
                    guard.candidate_accounts.clear_gap_target(&account);
                }
                let first_prev_balance = if !first_prev.is_zero() {
                    self.ledger.store.block.get(&first_prev).map(|b| b.balance())
                } else {
                    Some(rsnano_core::Amount::zero())
                };
                let first_missing_source =
                    self.block_missing_source(blocks.front().unwrap(), first_prev_balance);
                let mut prev_balance = blocks.front().unwrap().balance_field();
                let mut keep_len = 1usize;
                let mut last_missing_hash = if first_missing_source {
                    rsnano_core::BlockHash::zero()
                } else {
                    blocks.front().unwrap().hash()
                };
                if !first_missing_source {
                    for b in blocks.iter().skip(1) {
                        let h = b.hash();
                        if self.ledger.store.block.exists(&h) {
                            splice_successor = Some((last_missing_hash, h));
                            break;
                        }
                        if self.block_missing_source(b, prev_balance) {
                            stopped_at_missing_source = true;
                            break;
                        }
                        last_missing_hash = h;
                        prev_balance = b.balance_field();
                        keep_len += 1;
                    }
                }
                while blocks.len() > keep_len {
                    blocks.pop_back();
                }
                forward_cursor = last_missing_hash;
            }
        } else {
            let mut filtered = alloc::collections::VecDeque::with_capacity(blocks.len());
            let mut last_missing_hash = rsnano_core::BlockHash::zero();
            let mut prev_existing_hash = query.hash;
            let mut existing_deps = alloc::vec::Vec::new();
            let mut prev_balance = popped_start_balance;
            while let Some(b) = blocks.pop_front() {
                let h = b.hash();
                let b_bal = b.balance_field();
                if self.ledger.store.block.exists(&h) {
                    if !prev_existing_hash.is_zero() {
                        if self.ledger.store.successors.get(&prev_existing_hash) != Some(h) {
                            self.ledger.store.successors.put(&prev_existing_hash, &h);
                        }
                    }
                    prev_existing_hash = h;
                    if !last_missing_hash.is_zero() && splice_successor.is_none() {
                        splice_successor = Some((last_missing_hash, h));
                    }
                    existing_deps.push(h);
                    prev_balance = b_bal;
                } else if self.block_missing_source(&b, prev_balance) {
                    if filtered.is_empty() {
                        filtered.push_back(b);
                    } else {
                        stopped_at_missing_source = true;
                    }
                    break;
                } else {
                    prev_existing_hash = rsnano_core::BlockHash::zero();
                    last_missing_hash = h;
                    prev_balance = b_bal;
                    filtered.push_back(b);
                }
            }
            if !existing_deps.is_empty() {
                let mut guard = self.state.lock();
                for h in existing_deps {
                    if guard.candidate_accounts.has_dependency(&h) {
                        let unblocked = guard.candidate_accounts.unblock_hash(&h);
                        if unblocked > 0 {
                            self.stats.add(
                                StatType::BootstrapAccountSets,
                                DetailType::PriorityUnblocked,
                                unblocked as u64,
                            );
                        }
                    }
                }
            }
            forward_cursor = last_missing_hash;
            blocks = filtered;
            if blocks.is_empty() {
                if is_full_batch
                    && !account.is_zero()
                    && !prev_existing_hash.is_zero()
                    && prev_existing_hash != query.hash
                {
                    let advanced = self
                        .ledger
                        .advance_existing_chain(prev_existing_hash, 16384);
                    let next_target = if !advanced.is_zero() {
                        advanced
                    } else {
                        prev_existing_hash
                    };
                    let (head, _) = self.ledger.get_healed_account_frontiers(&account);
                    if next_target != head {
                        let mut guard = self.state.lock();
                        guard
                            .candidate_accounts
                            .set_forward_gap_target(account, next_target);
                        guard.candidate_accounts.reset_last_request(&account);
                        if guard
                            .candidate_accounts
                            .count_by_dependency_account(&account)
                            > 0
                        {
                            guard.candidate_accounts.priority_set_safe(&account);
                        }
                        return;
                    }
                }
                if !account.is_zero() {
                    let mut guard = self.state.lock();
                    guard.candidate_accounts.clear_gap_target(&account);
                }
                self.process_empty_response(query, channel_id);
                return;
            }
        }

        let mut batch = alloc::vec::Vec::with_capacity(blocks.len());
        while let Some(block) = blocks.pop_front() {
            if blocks.is_empty() {
                // It's the last block submitted for this account chain, reset timestamp to allow more requests
                let stats = self.stats.clone();
                let state = self.state.clone();
                #[cfg(not(test))]
                let ledger = self.ledger.clone();
                batch.push(
                    BlockContext::new_with_callback(
                        block,
                        BlockSource::Bootstrap,
                        channel_id,
                        Box::new(move |res| {
                            stats.inc(StatType::Bootstrap, DetailType::TimestampReset);
                            #[cfg(test)]
                            {
                                let _ = (res, is_full_batch);
                                let mut guard = state.lock();
                                guard.candidate_accounts.reset_last_request(&account);
                            }
                            #[cfg(not(test))]
                            match res {
                                Ok(()) => {
                                    if let Some((last_missing, existing_succ)) = splice_successor {
                                        ledger.store.successors.put(&last_missing, &existing_succ);
                                    }
                                    let mut guard = state.lock();
                                    if splice_successor.is_some() {
                                        guard.candidate_accounts.clear_gap_target(&account);
                                    } else if (is_full_batch || stopped_at_missing_source)
                                        && !forward_cursor.is_zero()
                                        && !account.is_zero()
                                    {
                                        guard
                                            .candidate_accounts
                                            .set_forward_gap_target(account, forward_cursor);
                                    } else {
                                        guard.candidate_accounts.clear_gap_target(&account);
                                    }
                                    let has_deps = guard
                                        .candidate_accounts
                                        .count_by_dependency_account(&account)
                                        > 0;
                                    if is_full_batch || stopped_at_missing_source || has_deps {
                                        guard.candidate_accounts.reset_last_request(&account);
                                        if has_deps {
                                            guard.candidate_accounts.priority_set_safe(&account);
                                        }
                                    } else {
                                        guard.candidate_accounts.priority_erase(&account);
                                    }
                                }
                                Err(rsnano_ledger::BlockError::GapPrevious) => {
                                    let mut guard = state.lock();
                                    guard.candidate_accounts.priority_down(&account);
                                    guard.candidate_accounts.reset_last_request(&account);
                                }
                                Err(rsnano_ledger::BlockError::Old) => {
                                    let mut guard = state.lock();
                                    guard.candidate_accounts.priority_down(&account);
                                    guard.candidate_accounts.reset_last_request(&account);
                                }
                                Err(rsnano_ledger::BlockError::GapSource)
                                | Err(rsnano_ledger::BlockError::GapEpochOpenPending) => {}
                                Err(_) => {
                                    let mut guard = state.lock();
                                    guard.candidate_accounts.priority_down(&account);
                                    guard.candidate_accounts.reset_last_request(&account);
                                }
                            }
                        }),
                    )
                    .with_account(account),
                );
            } else {
                batch.push(
                    BlockContext::new(
                        block,
                        BlockSource::Bootstrap,
                        channel_id,
                    )
                    .with_account(account),
                );
            }
        }
        self.block_processor_queue.push_batch(batch);
    }

    fn process_empty_response(&self, query: &RunningQuery, _channel_id: ChannelId) {
        self.stats
            .inc(StatType::BootstrapVerifyBlocks, DetailType::NothingNew);

        {
            let mut guard = self.state.lock();
            match guard.candidate_accounts.priority_down(&query.account) {
                PriorityDownResult::Deprioritized => {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::Deprioritize);
                }
                PriorityDownResult::Erased => {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::Deprioritize);
                    self.stats.inc(
                        StatType::BootstrapAccountSets,
                        DetailType::PriorityEraseThreshold,
                    );
                }
                PriorityDownResult::AccountNotFound => {
                    self.stats.inc(
                        StatType::BootstrapAccountSets,
                        DetailType::DeprioritizeFailed,
                    );
                }
                PriorityDownResult::InvalidAccount => {}
            }

            guard.candidate_accounts.reset_last_request(&query.account);
        }
    }

    #[cfg(not(test))]
    fn block_missing_source(
        &self,
        block: &rsnano_core::Block,
        prev_balance: Option<rsnano_core::Amount>,
    ) -> bool {
        let source: rsnano_core::BlockHash = match block {
            rsnano_core::Block::LegacyReceive(r) => r.source(),
            rsnano_core::Block::LegacyOpen(o) => o.source(),
            rsnano_core::Block::LegacySend(_) | rsnano_core::Block::LegacyChange(_) => {
                return false;
            }
            rsnano_core::Block::State(s) => {
                if s.link().is_zero() || self.ledger.constants.epochs.is_epoch_link(&s.link()) {
                    return false;
                }
                if block.previous().is_zero() {
                    s.link().into()
                } else {
                    let prev_bal = prev_balance.or_else(|| {
                        self.ledger
                            .store
                            .block
                            .get(&block.previous())
                            .map(|pb| pb.balance())
                    });
                    if let Some(prev_bal) = prev_bal {
                        if s.balance() >= prev_bal {
                            s.link().into()
                        } else {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
            }
        };
        !source.is_zero() && !self.ledger.store.block.exists(&source)
    }
}

#[cfg(test)]
mod tests {
    use rsnano_core::Account;

    use crate::bootstrap::state::QueryType;

    use super::*;

    #[test]
    fn response_doesnt_match_query() {
        let state = Arc::new(Mutex::new(BootstrapState::default()));
        let stats = Arc::new(Stats::default());
        let block_processor_queue = Arc::new(BlockProcessorQueue::default());
        let ledger = Arc::new(Ledger::new_null());
        let processor = BlockAckProcessor::new(state, stats.clone(), block_processor_queue, ledger);

        let query = RunningQuery::new_test_instance();
        let response = BlocksAckPayload::new_test_instance();
        let ok = processor.process(&query, response, ChannelId::from(1));
        assert!(!ok);
        assert_eq!(
            stats.count(
                StatType::BootstrapProcess,
                DetailType::Blocks,
                Direction::In
            ),
            1
        );
        assert_eq!(
            stats.count(
                StatType::BootstrapVerifyBlocks,
                DetailType::Invalid,
                Direction::In
            ),
            1
        );
    }

    #[test]
    fn handle_empty_response() {
        let state = Arc::new(Mutex::new(BootstrapState::default()));
        let stats = Arc::new(Stats::default());
        let block_processor_queue = Arc::new(BlockProcessorQueue::default());
        let ledger = Arc::new(Ledger::new_null());
        let processor = BlockAckProcessor::new(state, stats.clone(), block_processor_queue, ledger);

        let account = Account::from(42);

        let query = RunningQuery {
            query_type: QueryType::BlocksByAccount,
            account,
            ..RunningQuery::new_test_instance()
        };

        let response = BlocksAckPayload::empty();
        let ok = processor.process(&query, response, ChannelId::from(1));
        assert!(ok);
        assert_eq!(
            stats.count(
                StatType::BootstrapProcess,
                DetailType::Blocks,
                Direction::In
            ),
            1
        );
        assert_eq!(
            stats.count(
                StatType::BootstrapVerifyBlocks,
                DetailType::NothingNew,
                Direction::In
            ),
            1
        );
    }
}
