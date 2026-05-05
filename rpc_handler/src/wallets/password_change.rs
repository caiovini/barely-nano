use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{ChangedResponse, WalletWithPasswordArgs};

impl RpcCommandHandler {
    pub(crate) fn password_change(
        &self,
        args: WalletWithPasswordArgs,
    ) -> anyhow::Result<ChangedResponse> {
        self.node.wallets.rekey(&args.wallet, args.password)?;
        Ok(ChangedResponse::new(true))
    }
}
