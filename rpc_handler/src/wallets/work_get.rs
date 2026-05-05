use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{WalletWithAccountArgs, WorkResponse};

impl RpcCommandHandler {
    pub(crate) fn work_get(&self, args: WalletWithAccountArgs) -> anyhow::Result<WorkResponse> {
        let work = self
            .node
            .wallets
            .work_get2(&args.wallet, &args.account.into())?;
        Ok(WorkResponse::new(work))
    }
}
