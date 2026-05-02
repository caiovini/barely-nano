use super::Account;
use crate::serialize_32_byte_string;
use crate::u256_struct;
use rsnano_nullable_random::NullableRng;
use rand::Rng;

u256_struct!(Blake2Hash);
serialize_32_byte_string!(Blake2Hash);

pub type BlockHash = Blake2Hash;

impl Blake2Hash {
    pub fn random() -> Self {
        let mut rng = NullableRng::rng();
        BlockHash::from_bytes(rng.random())
    }
}

impl From<&Account> for Blake2Hash {
    fn from(account: &Account) -> Self {
        Self::from_bytes(*account.as_bytes())
    }
}

impl From<Account> for Blake2Hash {
    fn from(account: Account) -> Self {
        Self::from_bytes(*account.as_bytes())
    }
}

use blake2::digest::{Update, VariableOutput};
 
pub struct Blake2HashBuilder {
    hasher: blake2::Blake2bVar,
}
 
impl Default for Blake2HashBuilder {
    fn default() -> Self {
        Self {
            hasher: blake2::Blake2bVar::new(32).expect("invalid hash length"),
        }
    }
}
 
impl Blake2HashBuilder {
    pub fn new() -> Self {
        Default::default()
    }
 
    pub fn update(mut self, data: impl AsRef<[u8]>) -> Self {
        self.hasher.update(data.as_ref());
        self
    }
 
    pub fn build(self) -> Blake2Hash {
        let mut buffer = [0; 32];
        self.hasher.finalize_variable(&mut buffer).expect("hashing failed");
        BlockHash::from_bytes(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_serialize() {
        let serialized = serde_json::to_string_pretty(&BlockHash::from(123)).unwrap();
        assert_eq!(
            serialized,
            "\"000000000000000000000000000000000000000000000000000000000000007B\""
        );
    }

    #[test]
    fn serde_deserialize() {
        let deserialized: BlockHash = serde_json::from_str(
            "\"000000000000000000000000000000000000000000000000000000000000007B\"",
        )
        .unwrap();
        assert_eq!(deserialized, BlockHash::from(123));
    }
}
