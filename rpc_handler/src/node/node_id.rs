use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::NodeIdResponse;

impl RpcCommandHandler {
    pub(crate) fn node_id(&self) -> NodeIdResponse {
        let public = self.node.node_id.public_key();
        let as_account = public.as_account();
        NodeIdResponse {
            public,
            as_account,
            node_id: public.into(),
        }
    }
}
