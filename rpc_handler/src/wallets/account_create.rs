use rsnano_node::wallets::WalletsExt;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{AccountCreateArgs, AccountResponse};

impl RpcCommandHandler {
    pub(crate) fn account_create(
        &self,
        args: AccountCreateArgs,
    ) -> anyhow::Result<AccountResponse> {
        let generate_work = args.work.unwrap_or(true.into()).inner();
        let stateless = args.stateless.unwrap_or(false.into()).inner();

        let account = if stateless {
            self.node
                .wallets
                .next_ephemeral_account(&args.wallet, args.index.map(|i| i.inner()))?
        } else {
            match args.index {
                Some(i) => {
                    self.node
                        .wallets
                        .deterministic_insert_at(&args.wallet, i.inner(), generate_work)?
                }
                None => self
                    .node
                    .wallets
                    .deterministic_insert2(&args.wallet, generate_work)?,
            }
        };

        Ok(AccountResponse::new(account.as_account()))
    }
}
