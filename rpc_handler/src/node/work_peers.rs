use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::WorkPeersResponse;

impl RpcCommandHandler {
    pub(crate) fn work_peers(&self) -> WorkPeersResponse {
        WorkPeersResponse {
            work_peers: self.node.work_factory.peers(),
        }
    }
}
