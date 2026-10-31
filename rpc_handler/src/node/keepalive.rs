use crate::RpcCommandHandler;
use rsnano_rpc_messages::{HostWithPortArgs, StartedResponse};

impl RpcCommandHandler {
    pub(crate) fn keepalive(&self, args: HostWithPortArgs) -> anyhow::Result<StartedResponse> {
        self.node
            .keepalive_publisher
            .keepalive_or_connect_background(args.address, args.port.into());
        Ok(StartedResponse::new(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_rpc_command_requires_control, test_rpc_command_with_node};
    use alloc::sync::Arc;
    use rsnano_core::utils::Peer;
    use rsnano_node::Node;
    use rsnano_rpc_messages::RpcCommand;

    #[test]
    fn keepalive() {
        let node = Arc::new(Node::new_null());
        let keepalive_tracker = node.keepalive_publisher.track_keepalives();
        let cmd = RpcCommand::keepalive("foobar.com", 123);

        let result: StartedResponse = test_rpc_command_with_node(cmd, node);
        assert_eq!(result, StartedResponse::new(true));

        let keepalives = keepalive_tracker.output();
        assert_eq!(keepalives, [Peer::new("foobar.com", 123)]);
    }

    #[test]
    fn keepalive_fails_without_rpc_control_enabled() {
        test_rpc_command_requires_control(RpcCommand::keepalive("foobar.com", 123));
    }
}
