use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_core::PublicKey;
use rsnano_rpc_messages::{AccountArg, CountResponse};

impl RpcCommandHandler {
    pub(crate) fn delegators_count(&self, args: AccountArg) -> CountResponse {
        let representative: PublicKey = args.account.into();

        let count = self
            .node
            .ledger
            .any()
            .iter_accounts()
            .filter(|(_, info)| info.representative == representative)
            .count();

        CountResponse::new(count as u64)
    }
}
