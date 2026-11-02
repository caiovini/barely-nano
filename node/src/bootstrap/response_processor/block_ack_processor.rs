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
        let result = query.verify_blocks(&response);
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

        // Avoid re-processing the block we already have
        assert!(blocks.len() >= 1);
        if blocks.front().unwrap().hash() == query.start.into() {
            blocks.pop_front();
        }
        if blocks.is_empty() {
            self.process_empty_response(query, channel_id);
            return;
        }

        let account = query.account;
        let mut batch = alloc::vec::Vec::with_capacity(blocks.len());
        while let Some(block) = blocks.pop_front() {
            if blocks.is_empty() {
                // It's the last block submitted for this account chain, reset timestamp to allow more requests
                let stats = self.stats.clone();
                let state = self.state.clone();
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
                                    let mut guard = state.lock();
                                    if is_full_batch
                                        || guard
                                            .candidate_accounts
                                            .count_by_dependency_account(&account)
                                            > 0
                                    {
                                        guard.candidate_accounts.reset_last_request(&account);
                                    } else {
                                        guard.candidate_accounts.priority_erase(&account);
                                    }
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
