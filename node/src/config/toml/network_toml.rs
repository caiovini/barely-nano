use serde::{Deserialize, Serialize};

use crate::config::NodeConfig;

#[derive(Deserialize, Serialize)]
pub struct NetworkToml {
    pub duplicate_filter_size: Option<usize>,
    pub duplicate_filter_cutoff: Option<usize>,
}

impl From<&NodeConfig> for NetworkToml {
    fn from(value: &NodeConfig) -> Self {
        Self {
            duplicate_filter_size: Some(value.network_duplicate_filter_size),
            duplicate_filter_cutoff: Some(value.network_duplicate_filter_cutoff as usize),
        }
    }
}
