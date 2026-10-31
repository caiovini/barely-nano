use serde::{Deserialize, Serialize};

use crate::consensus::election_schedulers::OptimisticSchedulerConfig;

#[derive(Deserialize, Serialize)]
pub struct OptimisticSchedulerToml {
    pub enable: Option<bool>,
    pub gap_threshold: Option<u64>,
    pub max_size: Option<usize>,
    pub optimistic_limit_percentage: Option<usize>,
}

impl Default for OptimisticSchedulerToml {
    fn default() -> Self {
        let config = OptimisticSchedulerConfig::new();
        Self {
            enable: Some(true),
            gap_threshold: Some(config.gap_threshold),
            max_size: Some(config.max_size),
            optimistic_limit_percentage: Some(config.optimistic_limit_percentage),
        }
    }
}

impl OptimisticSchedulerConfig {
    pub fn merge_toml(&mut self, toml: &OptimisticSchedulerToml) {
        if let Some(gap_threshold) = toml.gap_threshold {
            self.gap_threshold = gap_threshold;
        }
        if let Some(max_size) = toml.max_size {
            self.max_size = max_size;
        }
        if let Some(optimistic_limit_percentage) = toml.optimistic_limit_percentage {
            self.optimistic_limit_percentage = optimistic_limit_percentage;
        }
    }
}
