use crate::{Amount, BlockHash, MaybeSavedBlock, PublicKey, ConfirmationType};
use core::time::Duration;
use alloc::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteSummary {
    pub voter: PublicKey,
    pub hash: BlockHash,
    pub weight: Amount,
    pub vote_created: u64, // timestamp in ms
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedElection {
    pub winner: MaybeSavedBlock,
    pub tally: Amount,
    pub final_tally: Amount,
    pub block_count: u32,
    pub voter_count: u32,
    pub election_end: u64, // timestamp in ms
    pub election_duration: Duration,
    pub confirmation_type: ConfirmationType,
    pub votes: BTreeMap<PublicKey, VoteSummary>,
}
