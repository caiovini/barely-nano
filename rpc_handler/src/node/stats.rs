
use crate::RpcCommandHandler;
use rsnano_core::utils::ContainerInfo;
use rsnano_rpc_messages::{StatsArgs, StatsType, SuccessResponse};

impl RpcCommandHandler {
    pub(crate) fn stats(&self, args: StatsArgs) -> anyhow::Result<serde_json::Value> {
        match args.stats_type {
            StatsType::Counters => {
                // StatsJsonWriter is disabled in no_std for now
                self.not_implemented().map_err(|e| anyhow::anyhow!(e))
            }
            StatsType::Samples => {
                self.not_implemented().map_err(|e| anyhow::anyhow!(e))
            }
            StatsType::Database => Ok(serde_json::Value::Object(serde_json::Map::new())),
            StatsType::Objects => Ok(ContainerInfo::builder()
                .node("node", self.node.container_info())
                .finish()
                .into_json()),
        }
    }

    pub(crate) fn stats_clear(&self) -> SuccessResponse {
        self.node.stats.clear();
        SuccessResponse::new()
    }
}
