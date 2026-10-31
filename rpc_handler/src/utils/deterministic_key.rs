use rsnano_core::{Account, PublicKey};
use rsnano_rpc_messages::{DeterministicKeyArgs, KeyPairDto};

pub fn deterministic_key(args: DeterministicKeyArgs) -> KeyPairDto {
    let private = rsnano_core::deterministic_key(&args.seed, args.index.inner());
    let public: PublicKey = (&private).try_into().unwrap();
    let account = Account::from(public);
    KeyPairDto::new(private, public, account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::RawKey;

    #[test]
    fn test_deterministic_key() {
        let result = deterministic_key(DeterministicKeyArgs::new(RawKey::zero(), 0));
        assert_eq!(
            result.private,
            RawKey::decode_hex("9F0E444C69F77A49BD0BE89DB92C38FE713E0963165CCA12FAF5712D7657120F")
                .unwrap()
        );
        assert_eq!(
            result.public,
            PublicKey::decode_hex(
                "C008B814A7D269A1FA3C6528B19201A24D797912DB9996FF02A1FF356E45552B"
            )
            .unwrap()
        );
        assert_eq!(
            result.account,
            Account::decode_account(
                "nano_3i1aq1cchnmbn9x5rsbap8b15akfh7wj7pwskuzi7ahz8oq6cobd99d4r3b7"
            )
            .unwrap()
        );
    }
}
