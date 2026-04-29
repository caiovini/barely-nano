use super::{NetworkParams, NodeConfig, NodeRpcConfig};
use rsnano_core::Networks;

#[derive(Debug, PartialEq)]
pub struct DaemonConfig {
    pub rpc_enable: bool,
    pub rpc: NodeRpcConfig,
    pub node: NodeConfig,
}

impl DaemonConfig {
    pub fn new(network_params: &NetworkParams, parallelism: usize) -> Self {
        Self {
            rpc_enable: false,
            node: NodeConfig::new(
                Some(network_params.network.default_node_port),
                network_params,
                parallelism,
            ),
            rpc: NodeRpcConfig::new(),
        }
    }

    pub fn new2(network: Networks, parallelism: usize) -> Self {
        Self {
            rpc_enable: false,
            node: NodeConfig::default_for(network, parallelism),
            rpc: NodeRpcConfig::new(),
        }
    }
}
