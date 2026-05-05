use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use rsnano_core::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockDto {
    pub block: BlockHash,
}

impl BlockDto {
    pub fn new(block: BlockHash) -> Self {
        Self { block }
    }
}
