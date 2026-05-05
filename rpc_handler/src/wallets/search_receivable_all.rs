use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_node::wallets::WalletsExt;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn search_receivable_all(&self) -> SuccessResponse {
        self.node.wallets.search_receivable_all();
        SuccessResponse::new()
    }
}
