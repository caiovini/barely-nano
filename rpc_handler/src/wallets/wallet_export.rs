use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{JsonResponse, WalletRpcMessage};

impl RpcCommandHandler {
    pub(crate) fn wallet_export(&self, args: WalletRpcMessage) -> anyhow::Result<JsonResponse> {
        let json = self.node.wallets.serialize(args.wallet)?;
        Ok(JsonResponse::new(json))
    }
}
