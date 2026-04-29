use crate::RawKey;

/// Key derivation function
#[derive(Clone)]
pub struct KeyDerivationFunction {
    _kdf_work: u32,
}

impl KeyDerivationFunction {
    pub fn new(kdf_work: u32) -> Self {
        Self { _kdf_work: kdf_work }
    }

    pub fn hash_password(&self, _password: &str, _salt: &[u8; 32]) -> RawKey {
        // Stub for no_std
        RawKey::from_bytes([0u8; 32])
    }
}
