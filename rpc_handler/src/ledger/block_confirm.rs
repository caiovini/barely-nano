use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_rpc_messages::{HashRpcMessage, StartedResponse};
use anyhow::anyhow;

use crate::RpcCommandHandler;

impl RpcCommandHandler {
    pub(crate) fn block_confirm(&self, args: HashRpcMessage) -> anyhow::Result<StartedResponse> {
        let any = self.node.ledger.any();
        let block = any.get_block(&args.hash).ok_or_else(|| anyhow!(Self::BLOCK_NOT_FOUND))?;
        if !any.confirmed().block_exists_or_pruned(&args.hash) {
            // Start new confirmation for unconfirmed (or not being confirmed) block
            if !self.node.confirming_set.contains(&args.hash) {
                self.node.election_schedulers.manual.push(block);
            }
        }
        Ok(StartedResponse::new(true))
    }
}
