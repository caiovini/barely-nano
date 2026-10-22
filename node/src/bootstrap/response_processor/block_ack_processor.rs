use alloc::boxed::Box;
use alloc::sync::Arc;
use spin::Mutex;

use rsnano_messages::BlocksAckPayload;
use rsnano_network::ChannelId;
use rsnano_stats::{DetailType, Direction, StatType, Stats};
use rsnano_ledger::{Ledger, LedgerSet};

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue, BlockSource},
    bootstrap::state::{BootstrapState, PriorityDownResult, RunningQuery, VerifyResult},
};

pub(crate) struct BlockAckProcessor {
    state: Arc<Mutex<BootstrapState>>,
    stats: Arc<Stats>,
    block_processor_queue: Arc<BlockProcessorQueue>,
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
        response: &BlocksAckPayload,
        channel_id: ChannelId,
    ) -> bool {
        self.stats
            .inc(StatType::BootstrapProcess, DetailType::Blocks);

        let result = query.verify_blocks(response);
        match result {
            VerifyResult::Ok => {
                self.process_valid_blocks(query, response, channel_id);
                true
            }
            VerifyResult::NothingNew => {
                self.process_empty_response(query);
                true
            }
            VerifyResult::Invalid => {
                self.stats
                    .inc(StatType::BootstrapVerifyBlocks, DetailType::Invalid);
                false
            }
        }
    }

    fn process_valid_blocks(&self, query: &RunningQuery, response: &BlocksAckPayload, channel_id: ChannelId) {
        self.stats
            .inc(StatType::BootstrapVerifyBlocks, DetailType::Ok);
        self.stats.add_dir(
            StatType::Bootstrap,
            DetailType::Blocks,
            Direction::In,
            response.blocks().len() as u64,
        );

        let mut blocks = response.blocks().clone();

        // Avoid re-processing the block we already have
        assert!(blocks.len() >= 1);
        let start_hash = match query.query_type {
            crate::bootstrap::state::QueryType::BlocksByHash => query.start.into(),
            _ => rsnano_core::BlockHash::zero(),
        };

        if blocks.front().unwrap().hash() == start_hash {
            blocks.pop_front();
        }

        if blocks.is_empty() {
            self.process_empty_response(query);
            return;
        }

        let any = self.ledger.any();

        // 1. Drop blocks that are already known in the ledger
        while let Some(b) = blocks.front() {
            if any.block_exists(&b.hash()) {
                blocks.pop_front();
            } else {
                break;
            }
        }

        if blocks.is_empty() {
            self.process_empty_response(query);
            return;
        }

        let first = blocks.front().unwrap();
        let target_account = first.account_field().unwrap_or(query.account);

        // 2. If the first block is missing its dependency, block the account immediately
        // and do not flood the BlockProcessorQueue with doomed cascading blocks.
        if first.previous().is_zero() {
            let source = first.source_or_link();
            if !source.is_zero() && !any.block_exists(&source) {
                if !target_account.is_zero() {
                    let now = rsnano_nullable_clock::Timestamp::now();
                    let mut guard = self.state.lock();
                    guard.candidate_accounts.block(target_account, source, now);
                    self.stats.inc(StatType::BootstrapAccountSets, DetailType::Block);
                }
                return;
            }
        } else if !any.block_exists(&first.previous()) {
            if !target_account.is_zero() {
                let now = rsnano_nullable_clock::Timestamp::now();
                let mut guard = self.state.lock();
                guard.candidate_accounts.block(target_account, first.previous(), now);
                self.stats.inc(StatType::BootstrapAccountSets, DetailType::Block);
            }
            return;
        }

        while let Some(block) = blocks.pop_front() {
            if blocks.is_empty() {
                // It's the last block submitted for this account chain, reset timestamp to allow more requests
                let stats = self.stats.clone();
                let state = self.state.clone();
                let account = query.account;
                self.block_processor_queue
                    .push(BlockContext::new_with_callback(
                        block,
                        BlockSource::Bootstrap,
                        channel_id,
                        Box::new(move |res| {
                            if res.is_ok() {
                                stats.inc(StatType::Bootstrap, DetailType::TimestampReset);
                                let mut guard = state.lock();
                                guard.candidate_accounts.reset_last_request(&account);
                            }
                        }),
                    ));
            } else {
                self.block_processor_queue.push(BlockContext::new(
                    block,
                    BlockSource::Bootstrap,
                    channel_id,
                ));
            }
        }
    }

    fn process_empty_response(&self, query: &RunningQuery) {
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
        }
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
        let ok = processor.process(&query, &response);
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
        let ok = processor.process(&query, &response);
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
