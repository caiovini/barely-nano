use rsnano_rpc_messages::{AccountResponse, KeyArg};

pub(crate) fn account_get(args: KeyArg) -> AccountResponse {
    AccountResponse::new(args.key.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::PublicKey;

    #[test]
    fn test_account_get() {
        let result = account_get(KeyArg::new(PublicKey::zero()));
        assert_eq!(result.account, PublicKey::zero().into());
    }
}
