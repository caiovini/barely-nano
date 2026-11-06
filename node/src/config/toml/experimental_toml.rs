use alloc::string::ToString;
use alloc::string::String;
use alloc::vec::Vec;
use crate::config::NodeConfig;
use rsnano_core::{utils::Peer, Account};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct ExperimentalToml {
    pub enable_pruning: Option<bool>,
    pub max_pruning_age: Option<u64>,
    pub max_pruning_depth: Option<u64>,
    pub secondary_work_peers: Option<Vec<String>>,
    pub pruning_exempt_accounts: Option<Vec<String>>,
}

impl NodeConfig {
    pub fn merge_experimental_toml(&mut self, toml: &ExperimentalToml) {
        if let Some(max_pruning_age) = toml.max_pruning_age {
            self.max_pruning_age_s = max_pruning_age as i64;
        }
        if let Some(max_pruning_depth) = toml.max_pruning_depth {
            self.max_pruning_depth = max_pruning_depth;
        }
        if let Some(secondary_work_peers) = &toml.secondary_work_peers {
            self.secondary_work_peers = secondary_work_peers
                .iter()
                .map(|string| string.parse::<Peer>().expect("Invalid secondary work peer"))
                .collect();
        }
        if let Some(exempt_accounts) = &toml.pruning_exempt_accounts {
            self.pruning_exempt_accounts = exempt_accounts
                .iter()
                .filter_map(|s| Account::decode_account(s).ok())
                .collect();
        }
    }
}

impl From<&NodeConfig> for ExperimentalToml {
    fn from(config: &NodeConfig) -> Self {
        Self {
            enable_pruning: None,
            secondary_work_peers: Some(
                config
                    .secondary_work_peers
                    .iter()
                    .map(|peer| peer.to_string())
                    .collect(),
            ),
            max_pruning_age: Some(config.max_pruning_age_s as u64),
            max_pruning_depth: Some(config.max_pruning_depth),
            pruning_exempt_accounts: if config.pruning_exempt_accounts.is_empty() {
                None
            } else {
                Some(
                    config
                        .pruning_exempt_accounts
                        .iter()
                        .map(|a| a.encode_account())
                        .collect(),
                )
            },
        }
    }
}
