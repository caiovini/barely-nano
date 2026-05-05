use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RpcError {
    pub error: String,
}

impl RpcError {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}
