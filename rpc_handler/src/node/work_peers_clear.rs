use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn work_peers_clear(&self) -> SuccessResponse {
        self.node.work_factory.clear_peers();
        SuccessResponse::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::{test_rpc_command_requires_control, test_rpc_command_with_node};
    use alloc::{string::ToString, sync::Arc};
    use core::net::Ipv4Addr;
    use rsnano_core::utils::Peer;
    use rsnano_node::Node;
    use rsnano_rpc_messages::{
        AddressWithPortArgs, RpcCommand, SuccessResponse, WorkPeersResponse,
    };

    #[test]
    fn work_peers_lifecycle() {
        let node = Arc::new(Node::new_null());

        let initial: WorkPeersResponse =
            test_rpc_command_with_node(RpcCommand::WorkPeers, node.clone());
        assert!(initial.work_peers.is_empty());

        let _: SuccessResponse = test_rpc_command_with_node(
            RpcCommand::WorkPeerAdd(AddressWithPortArgs {
                address: Ipv4Addr::LOCALHOST.to_string(),
                port: 7076.into(),
            }),
            node.clone(),
        );

        let after_add: WorkPeersResponse =
            test_rpc_command_with_node(RpcCommand::WorkPeers, node.clone());
        assert_eq!(after_add.work_peers, [Peer::new("127.0.0.1", 7076)]);

        let _: SuccessResponse =
            test_rpc_command_with_node(RpcCommand::WorkPeersClear, node.clone());

        let after_clear: WorkPeersResponse =
            test_rpc_command_with_node(RpcCommand::WorkPeers, node);
        assert!(after_clear.work_peers.is_empty());
    }

    #[test]
    fn work_peers_requires_control() {
        test_rpc_command_requires_control(RpcCommand::WorkPeers);
        test_rpc_command_requires_control(RpcCommand::WorkPeersClear);
    }
}
