use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::AmountRpcMessage;

impl RpcCommandHandler {
    pub(crate) fn receive_minimum(&self) -> AmountRpcMessage {
        AmountRpcMessage::new(self.node.config.receive_minimum)
    }
}
