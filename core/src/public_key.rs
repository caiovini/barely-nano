use crate::{serialize_32_byte_string, u256_struct, Account, RawKey, Signature};

u256_struct!(PublicKey);
serialize_32_byte_string!(PublicKey);

impl PublicKey {
    /// IV for Key encryption
    pub fn initialization_vector(&self) -> [u8; 16] {
        self.0[..16].try_into().unwrap()
    }

    pub fn as_account(&self) -> Account {
        self.into()
    }

    pub fn verify(&self, message: &[u8], signature: &Signature) -> anyhow::Result<()> {
        if tools::ed25519::verify(self.as_bytes(), message, signature.as_bytes()) {
            Ok(())
        } else {
            let signature_bytes = signature.as_bytes();
            tracing::error!("Signature verification failed!");
            tracing::error!("Msg: {:X?}", message);
            tracing::error!("Expected R: {:X?}", &signature_bytes[0..32]);
            tracing::error!("A: {:X?}", self.as_bytes());
            tracing::error!("S: {:X?}", &signature_bytes[32..64]);
            Err(anyhow!("bad signature"))
        }
    }
}

impl TryFrom<&RawKey> for PublicKey {
    type Error = anyhow::Error;
    fn try_from(prv: &RawKey) -> Result<Self, Self::Error> {
        let pub_bytes = tools::ed25519::derive_public_key(prv.as_bytes());
        Ok(PublicKey::from_bytes(pub_bytes))
    }
}
