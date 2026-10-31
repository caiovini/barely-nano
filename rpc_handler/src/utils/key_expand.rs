use anyhow::anyhow;
use rsnano_core::{Account, PublicKey};
use rsnano_rpc_messages::{KeyExpandArgs, KeyPairDto};

pub fn key_expand(args: KeyExpandArgs) -> anyhow::Result<KeyPairDto> {
    let public: PublicKey = (&args.key)
        .try_into()
        .map_err(|_| anyhow!("Bad private key"))?;
    let account = Account::from(public);
    Ok(KeyPairDto::new(args.key, public, account))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::RawKey;

    #[test]
    fn test_key_expand() {
        let key = RawKey::decode_hex(
            "781186FB9EF17DB6E3D1056550D9FAE5D5BBADA6A6BC370E4CBB938B1DC71DA3",
        )
        .unwrap();
        let result = key_expand(KeyExpandArgs::new(key)).unwrap();
        assert_eq!(result.private, key);
        assert_eq!(
            result.public,
            PublicKey::decode_hex(
                "3068BB1CA04525BB0E416C485FE6A67FD52540227D267CC8B6E8DA958A7FA039"
            )
            .unwrap()
        );
        assert_eq!(
            result.account,
            Account::decode_account(
                "nano_1e5aqegc1jb7qe964u4adzmcezyo6o146zb8hm6dft8tkp79za3sxwjym5rx"
            )
            .unwrap()
        );
    }
}
