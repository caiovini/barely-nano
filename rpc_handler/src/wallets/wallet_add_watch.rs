use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{SuccessResponse, WalletAddWatchArgs};

impl RpcCommandHandler {
    pub(crate) fn wallet_add_watch(
        &self,
        args: WalletAddWatchArgs,
    ) -> anyhow::Result<SuccessResponse> {
        self.node
            .wallets
            .insert_watch(&args.wallet, &args.accounts)?;
        Ok(SuccessResponse::new())
    }
}
