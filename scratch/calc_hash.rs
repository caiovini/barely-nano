use rsnano_core::{Block, Networks};
use rsnano_ledger::{Ledger, LedgerConstants};
use rsnano_work::WorkThresholds;

fn main() {
    let constants = LedgerConstants::new(WorkThresholds::publish_full().clone(), Networks::NanoLiveNetwork);
    let genesis = &constants.genesis_block;
    println!("Genesis Hash: {}", genesis.hash());
}
