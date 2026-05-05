use rsnano_ledger::Ledger;
use rsnano_core::Account;

fn main() {
    let ledger = Ledger::new_null();
    let genesis = ledger.genesis();
    println!("Genesis Hash: {}", genesis.hash());
    println!("Genesis Account: {}", genesis.account().encode_account());
    println!("Genesis Account Hex: {}", genesis.account());
}
