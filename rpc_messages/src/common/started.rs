use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use crate::RpcBoolNumber;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StartedResponse {
    pub started: RpcBoolNumber,
}

impl StartedResponse {
    pub fn new(started: bool) -> Self {
        Self {
            started: started.into(),
        }
    }
}
