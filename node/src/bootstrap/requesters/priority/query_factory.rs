use super::pull_count_decider::PullCountDecider;
use crate::bootstrap::{
    state::candidate_accounts::PullType,
    AscPullQuerySpec,
    state::{BootstrapState, PriorityResult},
};
use alloc::sync::Arc;
use rsnano_core::{Account, BlockHash, HashOrAccount};
use rsnano_ledger::Ledger;
use rsnano_messages::{AscPullReqType, BlocksReqPayload, HashType};
use rsnano_network::Channel;
use rsnano_nullable_clock::Timestamp;
use crate::bootstrap::state::candidate_accounts::PullTypeDecider;

/// Creates a query for the next priority account
pub(super) struct QueryFactory {
    ledger: Arc<Ledger>,
    pub(crate) pull_type_decider: PullTypeDecider,
    pull_count_decider: PullCountDecider,
}

impl QueryFactory {
    pub(super) fn new(
        ledger: Arc<Ledger>,
        pull_type_decider: PullTypeDecider,
        pull_count_decider: PullCountDecider,
    ) -> Self {
        Self {
            ledger,
            pull_type_decider,
            pull_count_decider,
        }
    }

    pub fn next_priority_query(
        &mut self,
        state: &mut BootstrapState,
        channel: Arc<Channel>,
        now: Timestamp,
    ) -> Option<AscPullQuerySpec> {
        let next = state.next_priority(now);

        if next.account.is_zero() {
            return None;
        }
        let (head, confirmed_frontier) = self.get_account_info(&next.account);
        let pull_type = if next.pull_type == PullType::Safe {
            PullType::Safe
        } else {
            self.pull_type_decider.decide_pull_type()
        };

        Some(self.create_priority_query(&next, channel, pull_type, head, confirmed_frontier))
    }

    #[allow(dead_code)]
    pub(crate) fn get_account_head(&self, account: &Account) -> BlockHash {
        self.get_account_info(account).0
    }

    pub(crate) fn get_account_info(&self, account: &Account) -> (BlockHash, BlockHash) {
        self.ledger.get_healed_account_frontiers(account)
    }

    #[cfg(not(test))]
    pub(crate) fn block_exists(&self, hash: &BlockHash) -> bool {
        self.ledger.store.block.exists(hash)
    }

    #[cfg(not(test))]
    pub(crate) fn create_target_hash_query(
        &self,
        next: &PriorityResult,
        channel: Arc<Channel>,
    ) -> AscPullQuerySpec {
        let req_type = AscPullReqType::Blocks(BlocksReqPayload {
            start_type: HashType::Block,
            start: next.target_hash.into(),
            count: self.pull_count_decider.pull_count(next.priority),
        });
        AscPullQuerySpec {
            channel,
            req_type,
            hash: next.target_hash,
            account: next.account,
            cooldown_account: false,
        }
    }

    pub(crate) fn create_priority_query(
        &self,
        next: &PriorityResult,
        channel: Arc<Channel>,
        pull_type: PullType,
        head: BlockHash,
        confirmed_frontier: BlockHash,
    ) -> AscPullQuerySpec {
        let pull_start = { PullStart::new(pull_type, next.account, head, confirmed_frontier) };
        


        let req_type = AscPullReqType::Blocks(BlocksReqPayload {
            start_type: pull_start.start_type,
            start: pull_start.start,
            count: self.pull_count_decider.pull_count(next.priority),
        });

        // Only cooldown accounts that are likely to have more blocks.
        // Not throttling accounts that are probably up-to-date (fails > 0) allows us to query
        // another peer or evict them from the priority set faster without a cooldown stall.
        let cooldown_account = next.fails == 0;

        AscPullQuerySpec {
            channel,
            req_type,
            hash: pull_start.hash,
            account: next.account,
            cooldown_account,
        }
    }
}

struct PullStart {
    start: HashOrAccount,
    start_type: HashType,
    hash: BlockHash,
}

impl PullStart {
    fn new(
        pull_type: PullType,
        account: Account,
        head: BlockHash,
        confirmed_frontier: BlockHash,
    ) -> Self {
        // Check if the account picked has blocks, if it does, start the pull from the highest block
        if head.is_zero() {
            PullStart::account(account)
        } else {
            match pull_type {
                PullType::Optimistic => PullStart::block(head),
                PullType::Safe => PullStart::safe(account, confirmed_frontier),
            }
        }
    }

    fn safe(account: Account, confirmed_frontier: BlockHash) -> Self {
        if confirmed_frontier.is_zero() {
            PullStart::account(account)
        } else {
            PullStart::block(confirmed_frontier)
        }
    }

    fn account(account: Account) -> Self {
        Self {
            start: account.into(),
            start_type: HashType::Account,
            hash: BlockHash::zero(),
        }
    }

    fn block(start: BlockHash) -> Self {
        Self {
            start: start.into(),
            start_type: HashType::Block,
            hash: start,
        }
    }
}

