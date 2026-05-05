use alloc::borrow::ToOwned;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::string::ToString;
use crate::{AddressWithPortArgs, RpcCommand};

impl RpcCommand {
    pub fn work_peer_add(args: AddressWithPortArgs) -> Self {
        Self::WorkPeerAdd(args)
    }
}
