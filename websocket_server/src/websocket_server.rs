extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::sync::Arc;
use rsnano_core::{Account, BlockHash, Vote, VoteError, BlockSideband, SavedBlock, ConfirmedElection, VoteSummary};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_websocket_messages::{MessageEnvelope, Topic, ElectionInfo, JsonSideband, JsonVoteSummary};
use serde::{Deserialize, Serialize};

use super::WebsocketListener;

pub fn vote_received(vote: &Vote, result: Result<(), VoteError>) -> MessageEnvelope {
    MessageEnvelope::new(
        Topic::Vote,
        VoteReceived {
            account: Account::from(vote.voter).encode_account(),
            signature: vote.signature.encode_hex(),
            sequence: vote.timestamp().to_string(),
            timestamp: vote.timestamp().to_string(),
            duration: vote.duration_bits().to_string(),
            blocks: vote.hashes.iter().map(|h| h.to_string()).collect(),
            vote_type: match result {
                Ok(()) => "vote",
                Err(e) => e.as_str(),
            }
            .to_string(),
        },
    )
}

#[derive(Serialize, Deserialize)]
pub struct VoteReceived {
    pub account: String,
    pub signature: String,
    pub sequence: String,
    pub timestamp: String,
    pub duration: String,
    pub blocks: Vec<String>,
    #[serde(rename = "type")]
    pub vote_type: String,
}

pub struct NodeEventProcessor {
    pub server: Arc<WebsocketListener>,
    pub ledger: Arc<Ledger>,
}

impl NodeEventProcessor {
    pub fn handle_election_started(&self, hash: &BlockHash) {
        if self.server.any_subscriber(Topic::StartedElection) {
            self.server.broadcast(&started_election(hash));
        }
    }

    pub fn handle_election_stopped(&self, hash: &BlockHash) {
        if self.server.any_subscriber(Topic::StoppedElection) {
            self.server.broadcast(&stopped_election(hash));
        }
    }

    pub fn handle_block_confirmed(&self, block: &SavedBlock, election: &ConfirmedElection) {
        let amount = self
            .ledger
            .any()
            .block_amount_for(block)
            .unwrap_or_default();

        self.server.broadcast_confirmation(block, &amount, election);
    }
}

fn started_election(hash: &BlockHash) -> MessageEnvelope {
    MessageEnvelope::new(
        Topic::StartedElection,
        StartedElection {
            hash: hash.to_string(),
        },
    )
}

#[derive(Serialize)]
struct StartedElection {
    hash: String,
}

fn stopped_election(hash: &BlockHash) -> MessageEnvelope {
    MessageEnvelope::new(
        Topic::StoppedElection,
        StoppedElection {
            hash: hash.to_string(),
        },
    )
}

#[derive(Serialize)]
struct StoppedElection {
    hash: String,
}

pub fn into_json_vote_summary(v: &VoteSummary) -> JsonVoteSummary {
    JsonVoteSummary {
        representative: Account::from(v.voter).encode_account(),
        timestamp: v.vote_created.to_string(),
        hash: v.hash.to_string(),
        weight: v.weight.to_string_dec(),
    }
}

pub fn into_election_info(value: &ConfirmedElection) -> ElectionInfo {
    ElectionInfo {
        duration: value.election_duration.as_millis().to_string(),
        time: value.election_end.to_string(),
        tally: value.tally.to_string_dec(),
        final_tally: value.final_tally.to_string_dec(),
        blocks: value.block_count.to_string(),
        voters: value.voter_count.to_string(),
        request_count: 0.to_string(),
        votes: None,
    }
}

pub fn into_json_sideband(value: &BlockSideband) -> JsonSideband {
    JsonSideband {
        height: value.height.to_string(),
        local_timestamp: value.timestamp.to_string(),
    }
}
