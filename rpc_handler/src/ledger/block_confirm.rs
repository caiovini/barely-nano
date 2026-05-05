use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_rpc_messages::{HashRpcMessage, StartedResponse};

use crate::RpcCommandHandler;

impl RpcCommandHandler {
    pub(crate) fn block_confirm(&self, args: HashRpcMessage) -> anyhow::Result<StartedResponse> {
        let any = self.node.ledger.any();
        let block = self.load_block_any(&any, &args.hash)?;
        if !any.confirmed().block_exists_or_pruned(&args.hash) {
            // Start new confirmation for unconfirmed (or not being confirmed) block
            if !self.node.confirming_set.contains(&args.hash) {
                self.node.election_schedulers.manual.push(block);
            }
        }
        Ok(StartedResponse::new(true))
    }
}
