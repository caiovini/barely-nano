use alloc::string::ToString;
use crate::RpcCommandHandler;
use rsnano_node::telemetry::{rsnano_build_info, rsnano_version_string};
use rsnano_rpc_messages::{VersionResponse, RpcU32};

impl RpcCommandHandler {
    pub(crate) fn version(&self) -> VersionResponse {
        VersionResponse {
            rpc_version: 1.into(),
            store_version: RpcU32::from(self.node.ledger.version()),
            protocol_version: self.node.network_params.network.protocol_version.into(),
            node_vendor: rsnano_version_string(),
            store_vendor: self.node.ledger.store_vendor().to_string(),
            network: self
                .node
                .network_params
                .network
                .current_network
                .as_str()
                .to_string(),
            network_identifier: self.node.network_params.ledger.genesis_block.hash(),
            build_info: rsnano_build_info(),
        }
    }
}
