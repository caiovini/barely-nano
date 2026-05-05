use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn work_peers_clear(&self) -> SuccessResponse {
        self.node.work_factory.clear_peers();
        SuccessResponse::new()
    }
}
