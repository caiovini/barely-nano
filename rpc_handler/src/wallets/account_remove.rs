use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{RemovedDto, WalletWithAccountArgs};

impl RpcCommandHandler {
    pub(crate) fn account_remove(&self, args: WalletWithAccountArgs) -> anyhow::Result<RemovedDto> {
        self.node
            .wallets
            .remove_key(&args.wallet, &args.account.into())?;
        Ok(RemovedDto::new(true))
    }
}
