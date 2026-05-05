use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{SuccessResponse, WorkSetArgs};

impl RpcCommandHandler {
    pub(crate) fn work_set(&self, args: WorkSetArgs) -> anyhow::Result<SuccessResponse> {
        self.node
            .wallets
            .work_set(&args.wallet, &args.account.into(), args.work)?;
        Ok(SuccessResponse::new())
    }
}
