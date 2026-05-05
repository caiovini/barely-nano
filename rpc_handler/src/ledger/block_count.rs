use crate::RpcCommandHandler;
use rsnano_rpc_messages::BlockCountResponse;

impl RpcCommandHandler {
    pub(crate) fn block_count(&self) -> BlockCountResponse {
        let count = self.node.ledger.block_count();
        let unchecked = self.node.unchecked.len() as u64;
        let cemented = self.node.ledger.confirmed_count();
        let mut block_count = BlockCountResponse {
            count: (count as u64).into(),
            unchecked: unchecked.into(),
            cemented: (cemented as u64).into(),
            full: None,
            pruned: None,
        };

        if self.node.flags.enable_pruning {
            let full = (self.node.ledger.block_count() - self.node.ledger.pruned_count()) as u64;
            let pruned = self.node.ledger.pruned_count() as u64;

            block_count.full = Some(full.into());
            block_count.pruned = Some(pruned.into())
        }

        block_count
    }
}
