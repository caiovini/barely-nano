use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use crate::{RpcCommand, RpcF64, RpcU64};
use rsnano_core::{BlockHash, WorkNonce};
use serde::{Deserialize, Serialize};

impl RpcCommand {
    pub fn work_validate(args: impl Into<WorkValidateArgs>) -> Self {
        Self::WorkValidate(args.into())
    }
}

#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub struct WorkValidateArgs {
    pub hash: BlockHash,
    pub work: Option<WorkNonce>,
    pub multiplier: Option<RpcF64>,
    pub difficulty: Option<WorkNonce>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkValidateResponse {
    pub valid: Option<String>,
    pub valid_all: String,
    pub valid_receive: String,
    pub difficulty: RpcU64,
    pub multiplier: RpcF64,
}
