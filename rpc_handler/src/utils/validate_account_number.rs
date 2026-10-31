use rsnano_core::Account;
use rsnano_rpc_messages::{AccountCandidateArg, ValidResponse};

pub fn validate_account_number(args: AccountCandidateArg) -> ValidResponse {
    let valid = Account::decode_account(&args.account).is_ok();
    ValidResponse::new(valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn test_validate_account_number() {
        let valid = validate_account_number(AccountCandidateArg {
            account: Account::zero().encode_account(),
        });
        assert_eq!(valid.valid, true.into());

        let invalid = validate_account_number(AccountCandidateArg {
            account: String::from("nano_invalid"),
        });
        assert_eq!(invalid.valid, false.into());
    }
}
