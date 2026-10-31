use crate::RpcCommandHandler;
use rsnano_rpc_messages::WorkPeersResponse;

impl RpcCommandHandler {
    pub(crate) fn work_peers(&self) -> WorkPeersResponse {
        WorkPeersResponse {
            work_peers: self.node.work_factory.peers(),
        }
    }
}
