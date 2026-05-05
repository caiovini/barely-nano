use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn populate_backlog(&self) -> SuccessResponse {
        self.node.backlog_scan.trigger();
        SuccessResponse::new()
    }
}
