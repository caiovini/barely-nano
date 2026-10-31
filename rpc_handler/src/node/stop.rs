use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn stop(&self) -> SuccessResponse {
        SuccessResponse::new()
    }
}
