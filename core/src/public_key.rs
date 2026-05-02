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
        use blake2::digest::{Update, VariableOutput};
        use curve25519_dalek::edwards::CompressedEdwardsY;
        use curve25519_dalek::scalar::Scalar;
        use curve25519_dalek::edwards::EdwardsPoint;
 
        let public_key_bytes = self.as_bytes();
        let signature_bytes = signature.as_bytes();
        let r_bytes = &signature_bytes[0..32];
        let s_bytes = &signature_bytes[32..64];
 
        let a = CompressedEdwardsY(*public_key_bytes)
            .decompress()
            .ok_or_else(|| anyhow!("invalid public key"))?;
        
        let r_bytes_32: [u8; 32] = r_bytes.try_into().unwrap();
        let r = CompressedEdwardsY(r_bytes_32)
            .decompress()
            .ok_or_else(|| anyhow!("invalid signature R"))?;
 
        let s = Scalar::from_bytes_mod_order(s_bytes.try_into().unwrap());
 
        let mut hasher = blake2::Blake2bVar::new(64).unwrap();
        hasher.update(r_bytes);
        hasher.update(public_key_bytes);
        hasher.update(message);
        let mut k_bytes = [0u8; 64];
        hasher.finalize_variable(&mut k_bytes).unwrap();
        let k = Scalar::from_bytes_mod_order_wide(&k_bytes);
        tracing::debug!("Signature K hash: {:X?}", k_bytes);
 
        // Check if s*B == R + k*A  =>  R == s*B - k*A
        let check_r = EdwardsPoint::vartime_double_scalar_mul_basepoint(&k, &-a, &s);
        let check_r_bytes = check_r.compress();
 
        if check_r_bytes.as_bytes() == r_bytes {
            Ok(())
        } else {
            tracing::error!("Signature verification failed!");
            tracing::error!("Expected R: {:X?}", r_bytes);
            tracing::error!("Actual R:   {:X?}", check_r_bytes.as_bytes());
            Err(anyhow!("bad signature"))
        }
    }
}
 
impl TryFrom<&RawKey> for PublicKey {
    type Error = anyhow::Error;
    fn try_from(prv: &RawKey) -> Result<Self, Self::Error> {
        use blake2::digest::{Update, VariableOutput};
        use curve25519_dalek::scalar::Scalar;
        use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
 
        let mut hasher = blake2::Blake2bVar::new(64).unwrap();
        hasher.update(prv.as_bytes());
        let mut h = [0u8; 64];
        hasher.finalize_variable(&mut h).unwrap();
 
        h[0] &= 248;
        h[31] &= 63;
        h[31] |= 64;
 
        let a_scalar = Scalar::from_bytes_mod_order(h[0..32].try_into().unwrap());
        let a_point = &a_scalar * &ED25519_BASEPOINT_POINT;
        Ok(PublicKey::from_bytes(*a_point.compress().as_bytes()))
    }
}
