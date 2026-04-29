#![no_std]

extern crate alloc;

#[macro_use]
extern crate num_derive;

mod account_store;
mod block_store;
mod confirmation_height_store;
mod final_vote_store;
mod online_weight_store;
mod peer_store;
mod pending_store;
mod pruned_store;
mod rep_weight_store;
mod store;
mod successor_store;
mod version_store;

pub use account_store::*;
pub use block_store::*;
pub use confirmation_height_store::*;
pub use final_vote_store::*;
pub use online_weight_store::*;
pub use peer_store::*;
pub use pending_store::*;
pub use pruned_store::*;
pub use rep_weight_store::*;
pub use store::*;
pub use successor_store::*;
pub use version_store::*;

pub const STORE_VERSION_MINIMUM: i32 = 24;
pub const STORE_VERSION_CURRENT: i32 = 10_001;
pub const FIRST_INCOMPATIBLE_STORE_VERSION: i32 = 10_000;
