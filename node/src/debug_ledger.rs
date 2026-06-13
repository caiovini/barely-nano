use rsnano_core::{Account, Amount};
use rsnano_ledger::{Ledger, AnySet};
use std::sync::Arc;

pub fn debug_ledger_accounts(ledger: Arc<Ledger>) {
    let any = ledger.any();
    let accounts = any.accounts_range(Account::zero()..);
    println!("Ledger Accounts:");
    for (acc, info) in accounts {
        let weight = any.account_weight(&acc.into());
        println!("  Account {}: weight={}, head={}, count={}", 
            acc.encode_account(), 
            weight.format_balance(0),
            info.head, 
            info.block_count
        );
    }
}
