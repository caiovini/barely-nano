use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use rsnano_core::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HashesArgs {
    pub hashes: Vec<BlockHash>,
}

impl HashesArgs {
    pub fn new(hashes: Vec<BlockHash>) -> Self {
        Self { hashes }
    }
}
