use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::borrow::ToOwned;
use rsnano_core::{Amount, BlockType, SavedBlock, ConfirmedElection};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_websocket_messages::{
    BlockConfirmed, ElectionInfo, JsonSideband, MessageEnvelope, Topic,
};

use crate::{into_election_info, into_json_sideband, into_json_vote_summary, ConfirmationOptions};

pub(super) struct ConfirmationMessageFactory<'a> {
    pub ledger: &'a Ledger,
    pub options: &'a ConfirmationOptions,
    pub block: &'a SavedBlock,
    pub amount: &'a Amount,
    pub election: &'a ConfirmedElection,
}

impl ConfirmationMessageFactory<'_> {
    pub fn create_message(&self) -> MessageEnvelope {
        MessageEnvelope::new(
            Topic::Confirmation,
            BlockConfirmed {
                account: self.block.account().encode_account(),
                amount: self.amount.to_string_dec(),
                hash: self.block.hash().to_string(),
                confirmation_type: self.confirmation_type(),
                election_info: self.election_info(),
                block: self.json_block(),
                sideband: self.sideband(),
                linked_account: self.linked_account(),
            },
        )
    }

    fn confirmation_type(&self) -> String {
        self.election.confirmation_type.as_str().to_string()
    }

    fn subtype(&self) -> String {
        if self.block.block_type() == BlockType::State {
            self.block.subtype().as_str().to_string()
        } else {
            String::new()
        }
    }

    fn election_info(&self) -> Option<ElectionInfo> {
        if self.options.include_election_info || self.options.include_election_info_with_votes {
            let mut info = into_election_info(self.election);
            if self.options.include_election_info_with_votes {
                let mut votes: Vec<_> = self.election.votes.values().cloned().collect();
                // sort by descending weight
                votes.sort_by(|a, b| b.weight.cmp(&a.weight));
                info.votes = Some(votes.iter().map(into_json_vote_summary).collect());
            }
            Some(info)
        } else {
            None
        }
    }

    fn json_block(&self) -> Option<serde_json::Value> {
        if self.options.include_block {
            let mut block_value: serde_json::Value = (**self.block).clone().into();
            let subtype = self.subtype();
            if !subtype.is_empty() {
                if let serde_json::Value::Object(o) = &mut block_value {
                    o.insert("subtype".to_string(), serde_json::Value::String(subtype));
                }
            }
            Some(block_value)
        } else {
            None
        }
    }

    fn linked_account(&self) -> Option<String> {
        if !self.options.include_block || !self.options.include_linked_account {
            return None;
        }

        let any = self.ledger.any();
        match any.linked_account(self.block) {
            Some(linked) => Some(linked.encode_account()),
            None => Some("0".to_owned()),
        }
    }

    fn sideband(&self) -> Option<JsonSideband> {
        if self.options.include_sideband_info {
            Some(into_json_sideband(self.block.sideband()))
        } else {
            None
        }
    }
}
