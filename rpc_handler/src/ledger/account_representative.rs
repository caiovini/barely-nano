use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{AccountArg, AccountRepresentativeDto};

impl RpcCommandHandler {
    pub(crate) fn account_representative(
        &self,
        args: AccountArg,
    ) -> anyhow::Result<AccountRepresentativeDto> {
        let any = self.node.ledger.any();
        let account_info = self.load_account(&any, &args.account)?;
        Ok(AccountRepresentativeDto::new(
            account_info.representative.as_account(),
        ))
    }
}
