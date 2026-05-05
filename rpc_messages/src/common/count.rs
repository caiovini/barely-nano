use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use super::primitives::RpcU64;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CountArgs {
    pub count: Option<RpcU64>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CountResponse {
    pub count: RpcU64,
}

impl CountResponse {
    pub fn new(count: u64) -> Self {
        Self {
            count: count.into(),
        }
    }
}
