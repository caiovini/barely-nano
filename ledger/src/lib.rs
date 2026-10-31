#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate strum_macros;

mod block_cementer;
mod block_insertion;
mod block_rollback;
mod dependent_blocks_finder;
mod ledger;
mod ledger_builder;
mod ledger_constants;
mod ledger_sets;
mod rep_weight_cache;
mod rep_weights_updater;
mod representative_block_finder;
mod vote_verifier;

pub(crate) use block_rollback::BlockRollbackPerformer;
pub use block_rollback::RollbackError;
pub use dependent_blocks_finder::*;
pub use ledger::*;
pub use ledger_builder::*;
pub use ledger_constants::{
    LedgerConstants, DEV_GENESIS_ACCOUNT, DEV_GENESIS_BLOCK, DEV_GENESIS_HASH, DEV_GENESIS_PUB_KEY,
};
pub use ledger_sets::*;
pub use rep_weight_cache::*;
pub use rep_weights_updater::*;
pub(crate) use representative_block_finder::RepresentativeBlockFinder;

#[inline]
pub fn block_may_exist(hash: &rsnano_core::BlockHash) -> bool {
    rsnano_ledger_store::BtreeBlockStore::may_exist_global(hash)
}
