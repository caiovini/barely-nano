use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn populate_backlog(&self) -> SuccessResponse {
        self.node.backlog_scan.lock().trigger();
        SuccessResponse::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::{test_rpc_command, test_rpc_command_requires_control};
    use rsnano_rpc_messages::{RpcCommand, SuccessResponse};

    #[test]
    fn populate_backlog_command() {
        let result: SuccessResponse = test_rpc_command(RpcCommand::PopulateBacklog);
        assert_eq!(result, SuccessResponse::new());
    }

    #[test]
    fn populate_backlog_requires_control() {
        test_rpc_command_requires_control(RpcCommand::PopulateBacklog);
    }
}
