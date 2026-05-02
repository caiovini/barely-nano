
use rsnano_core::{Account, PrivateKey, PublicKey, Signature, BlockHash};
use rsnano_core::utils::RawKey;

#[test]
fn verify_genesis() {
    let pub_key_hex = "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0";
    let pub_key = PublicKey::decode_hex(pub_key_hex).unwrap();
    let account = Account::decode_account("xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo").unwrap();
    assert_eq!(pub_key.as_bytes(), account.as_bytes());
    
    let signature_hex = "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02";
    let signature = Signature::decode_hex(signature_hex).unwrap();
    
    // For genesis, hash == pub_key
    let hash_bytes = pub_key.as_bytes();
    
    pub_key.verify(hash_bytes, &signature).expect("Genesis signature verification failed!");
}

#[test]
fn test_key_derivation() {
    let priv_key_hex = "34F0A37AAD20F4A260F0A5B3CB3D7FB50673212263E58A380BC10474BB039CE4";
    let priv_key = PrivateKey::from_hex_str(priv_key_hex).unwrap();
    let pub_key = priv_key.public_key();
    let expected_pub_key_hex = "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0";
    assert_eq!(pub_key.encode_hex(), expected_pub_key_hex);
}
