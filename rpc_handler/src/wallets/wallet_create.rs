use crate::RpcCommandHandler;
use rsnano_core::WalletId;
use rsnano_rpc_messages::{WalletCreateArgs, WalletCreateResponse};

impl RpcCommandHandler {
    pub(crate) fn wallet_create(
        &self,
        args: WalletCreateArgs,
    ) -> anyhow::Result<WalletCreateResponse> {
        let wallet = WalletId::random();
        self.node.wallets.create(wallet);

        if let Some(spend_acc) = args.spend_public_key {
            self.node
                .wallets
                .set_spend_public_key(&wallet, Some(spend_acc.into()))?;
        }

        let last_restored_account;
        let restored_count;
        if let Some(seed) = args.seed {
            let (count, last) = self.node.wallets.change_seed(wallet, &seed, 0)?;
            last_restored_account = Some(last);
            restored_count = Some(count.into());
        } else {
            last_restored_account = None;
            restored_count = None;
        }

        Ok(WalletCreateResponse {
            wallet,
            last_restored_account,
            restored_count,
        })
    }
}
