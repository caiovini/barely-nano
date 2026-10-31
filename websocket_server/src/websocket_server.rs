extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr, SocketAddrV6};
use rsnano_core::{Account, BlockHash, BlockSideband, SavedBlock, Vote, VoteError};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::TelemetryData;
use rsnano_node::{
    config::WebsocketConfig,
    consensus::election::{ConfirmedElection, VoteSummary},
    CompositeNodeEventHandler, Node, NodeEvent, NodeEventHandler,
};
use rsnano_websocket_messages::{
    new_block_arrived_message, ElectionInfo, JsonSideband, JsonVoteSummary, MessageEnvelope, Topic,
};
use serde::{Deserialize, Serialize};
use tracing::error;

use super::WebsocketListener;

pub fn create_websocket_server(
    config: WebsocketConfig,
    node: &Node,
    event_handlers: &mut CompositeNodeEventHandler,
) -> Option<Arc<WebsocketListener>> {
    if !config.enabled {
        return None;
    }

    let Ok(address) = config.address.parse::<IpAddr>() else {
        error!(address = config.address, "invalid websocket IP address");
        return None;
    };

    let endpoint = SocketAddr::new(address, config.port);
    let server = Arc::new(WebsocketListener::new(
        endpoint,
        node.wallets.clone(),
        node.ledger.clone(),
    ));

    event_handlers.add(NodeEventProcessor {
        server: server.clone(),
        ledger: node.ledger.clone(),
    });

    let server_w = Arc::downgrade(&server);
    node.telemetry
        .on_telemetry_processed(Box::new(move |data, peer_addr| {
            if let Some(server) = server_w.upgrade() {
                if server.any_subscriber(Topic::Telemetry) {
                    server.broadcast(&telemetry_received(data, *peer_addr));
                }
            }
        }));

    Some(server)
}

pub fn telemetry_received(data: &TelemetryData, endpoint: SocketAddrV6) -> MessageEnvelope {
    MessageEnvelope::new(
        Topic::Telemetry,
        TelemetryReceived {
            block_count: data.block_count.to_string(),
            cemented_count: data.cemented_count.to_string(),
            unchecked_count: data.unchecked_count.to_string(),
            account_count: data.account_count.to_string(),
            bandwidth_cap: data.bandwidth_cap.to_string(),
            peer_count: data.peer_count.to_string(),
            protocol_version: data.protocol_version.to_string(),
            uptime: data.uptime.to_string(),
            genesis_block: data.genesis_block.to_string(),
            major_version: data.major_version.to_string(),
            minor_version: data.minor_version.to_string(),
            patch_version: data.patch_version.to_string(),
            pre_release_version: data.pre_release_version.to_string(),
            maker: data.maker.to_string(),
            timestamp: data.timestamp.as_u64().to_string(),
            active_difficulty: format!("{:016x}", data.active_difficulty),
            node_id: data.node_id.to_string(),
            signature: data.signature.encode_hex(),
            address: endpoint.ip().to_string(),
            port: endpoint.port().to_string(),
        },
    )
}

#[derive(Serialize, Deserialize)]
pub struct TelemetryReceived {
    pub block_count: String,
    pub cemented_count: String,
    pub unchecked_count: String,
    pub account_count: String,
    pub bandwidth_cap: String,
    pub peer_count: String,
    pub protocol_version: String,
    pub uptime: String,
    pub genesis_block: String,
    pub major_version: String,
    pub minor_version: String,
    pub patch_version: String,
    pub pre_release_version: String,
    pub maker: String,
    pub timestamp: String,
    pub active_difficulty: String,
    pub node_id: String,
    pub signature: String,
    pub address: String,
    pub port: String,
}

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

impl NodeEventHandler for NodeEventProcessor {
    fn handle(&mut self, event: &NodeEvent) {
        match event {
            NodeEvent::ElectionStarted(hash) => {
                if self.server.any_subscriber(Topic::StartedElection) {
                    self.server.broadcast(&started_election(hash));
                }
            }
            NodeEvent::ElectionStopped(hash) => {
                if self.server.any_subscriber(Topic::StoppedElection) {
                    self.server.broadcast(&stopped_election(hash));
                }
            }
            NodeEvent::BlockConfirmed(block, election) => {
                let amount = self
                    .ledger
                    .any()
                    .block_amount_for(block)
                    .unwrap_or_default();

                self.server.broadcast_confirmation(block, &amount, election);
            }
            NodeEvent::VoteProcessed(vote, vote_code) => {
                if self.server.any_subscriber(Topic::Vote) {
                    self.server.broadcast(&vote_received(vote, *vote_code));
                }
            }
            NodeEvent::BlocksProcessed(results) => {
                if self.server.any_subscriber(Topic::NewUnconfirmedBlock) {
                    for result in results {
                        if result.status.is_ok() {
                            if let Some(block) = result.saved_block.as_ref() {
                                self.server.broadcast(&new_block_arrived_message(block));
                            }
                        }
                    }
                }
            }
        }
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
        time: value.election_end.millis().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::Ipv6Addr;

    #[test]
    fn vote_received_envelope() {
        let vote = Vote::new_test_instance();
        let env = vote_received(&vote, Ok(()));
        assert_eq!(env.topic, Some(Topic::Vote));
        let vr: VoteReceived = serde_json::from_value(env.message.unwrap()).unwrap();
        assert_eq!(vr.vote_type, "vote");
        assert_eq!(vr.account, Account::from(vote.voter).encode_account());

        let env_replay = vote_received(&vote, Err(VoteError::Replay));
        let vr_replay: VoteReceived = serde_json::from_value(env_replay.message.unwrap()).unwrap();
        assert_eq!(vr_replay.vote_type, "replay");
    }

    #[test]
    fn telemetry_received_envelope() {
        let mut data = TelemetryData::new();
        data.block_count = 100;
        data.cemented_count = 95;
        let endpoint = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 7075, 0, 0);
        let env = telemetry_received(&data, endpoint);
        assert_eq!(env.topic, Some(Topic::Telemetry));
        let tr: TelemetryReceived = serde_json::from_value(env.message.unwrap()).unwrap();
        assert_eq!(tr.block_count, "100");
        assert_eq!(tr.cemented_count, "95");
        assert_eq!(tr.port, "7075");
    }
}
