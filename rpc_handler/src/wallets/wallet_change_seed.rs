use crate::RpcCommandHandler;
use rsnano_rpc_messages::{WalletChangeSeedArgs, WalletChangeSeedResponse};

impl RpcCommandHandler {
    pub(crate) fn wallet_change_seed(
        &self,
        args: WalletChangeSeedArgs,
    ) -> WalletChangeSeedResponse {
        if let Some(spend_acc) = args.spend_public_key {
            self.node
                .wallets
                .set_spend_public_key(&args.wallet, Some(spend_acc.into()))
                .unwrap();
        }
        let (restored_count, last_restored_account) = self
            .node
            .wallets
            .change_seed(
                args.wallet,
                &args.seed,
                args.count.unwrap_or_default().inner(),
            )
            .unwrap();
        WalletChangeSeedResponse::new(last_restored_account, restored_count)
    }
}
