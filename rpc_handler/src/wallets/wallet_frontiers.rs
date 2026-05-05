use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_ledger::AnySet;
use rsnano_rpc_messages::{FrontiersResponse, WalletRpcMessage};
use hashbrown::HashMap;

impl RpcCommandHandler {
    pub(crate) fn wallet_frontiers(
        &self,
        args: WalletRpcMessage,
    ) -> anyhow::Result<FrontiersResponse> {
        let any = self.node.ledger.any();
        let accounts = self.node.wallets.get_accounts_of_wallet(&args.wallet)?;
        let mut frontiers = HashMap::new();

        for account in accounts {
            if let Some(block_hash) = any.account_head(&account) {
                frontiers.insert(account, block_hash);
            }
        }
        Ok(FrontiersResponse::new(frontiers))
    }
}
