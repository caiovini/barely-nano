use rsnano_core::{Account, PrivateKey};
use rsnano_rpc_messages::KeyPairDto;

pub(crate) fn key_create() -> KeyPairDto {
    let keypair = PrivateKey::new();
    let private = keypair.raw_key();
    let public = keypair.public_key();
    let account = Account::from(public);
    KeyPairDto::new(private, public, account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::PublicKey;

    #[test]
    fn test_key_create() {
        let result = key_create();
        let expected_pub: PublicKey = (&result.private).try_into().unwrap();
        assert_eq!(result.public, expected_pub);
        assert_eq!(result.account, Account::from(expected_pub));
    }
}
