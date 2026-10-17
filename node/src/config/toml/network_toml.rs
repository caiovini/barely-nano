use serde::{Deserialize, Serialize};

use crate::config::NodeConfig;

#[derive(Deserialize, Serialize)]
pub struct NetworkToml {
    pub peer_reachout: Option<u64>,
    pub cached_peer_reachout: Option<u64>,
    pub duplicate_filter_size: Option<usize>,
    pub duplicate_filter_cutoff: Option<usize>,
}

impl From<&NodeConfig> for NetworkToml {
    fn from(value: &NodeConfig) -> Self {
        Self {
            peer_reachout: Some(value.network.peer_reachout.as_millis() as u64),
            cached_peer_reachout: Some(value.network.cached_peer_reachout.as_millis() as u64),
            duplicate_filter_size: Some(value.network_duplicate_filter_size),
            duplicate_filter_cutoff: Some(value.network_duplicate_filter_cutoff as usize),
        }
    }
}
