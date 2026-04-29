mod converters;
mod daemon_config;
mod network_constants;
mod network_params;
mod node_config;
mod node_flags;
mod node_rpc_config;
mod toml;
mod websocket_config;

pub use daemon_config::*;
pub use network_constants::*;
pub use network_params::*;
pub use node_config::*;
pub use node_flags::*;
pub use node_rpc_config::*;
pub use rsnano_core::Networks;
pub use websocket_config::WebsocketConfig;

pub struct GlobalConfig {
    pub node_config: NodeConfig,
    pub flags: NodeFlags,
    pub network_params: NetworkParams,
}
