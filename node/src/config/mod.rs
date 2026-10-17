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
pub use toml::*;
pub use websocket_config::WebsocketConfig;

pub struct GlobalConfig {
    pub node_config: NodeConfig,
    pub flags: NodeFlags,
    pub network_params: NetworkParams,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            node_config: NodeConfig::default(),
            flags: NodeFlags::default(),
            network_params: NetworkParams::new(rsnano_core::Networks::NanoLiveNetwork),
        }
    }
}
