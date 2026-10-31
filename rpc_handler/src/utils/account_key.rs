use rsnano_rpc_messages::{AccountArg, KeyResponse};

pub(crate) fn account_key(args: AccountArg) -> KeyResponse {
    KeyResponse::new(args.account.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::Account;

    #[test]
    fn test_account_key() {
        let result = account_key(AccountArg::new(Account::zero()));
        assert_eq!(result.key, Account::zero().into());
    }
}
