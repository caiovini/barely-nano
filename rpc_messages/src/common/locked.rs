use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use super::primitives::RpcBoolNumber;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LockedResponse {
    pub locked: RpcBoolNumber,
}

impl LockedResponse {
    pub fn new(locked: bool) -> Self {
        Self {
            locked: locked.into(),
        }
    }
}
