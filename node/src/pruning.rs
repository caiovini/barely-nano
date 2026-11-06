use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::boxed::Box;
use core::sync::atomic::{ AtomicBool, Ordering };
use core::time::Duration;
use hashbrown::HashSet;

use tracing::info;

use rsnano_core::{utils::UnixTimestamp, Account, BlockHash};
use rsnano_ledger::Ledger;
use rsnano_stats::{DetailType, StatType, Stats};

use crate::{
    config::NodeConfig,
    utils::{ThreadPool, ThreadPoolImpl},
    wallets::Wallets,
};

pub struct LedgerPruning {
    config: NodeConfig,
    ledger: Arc<Ledger>,
    wallets: Arc<Wallets>,
    stopped: AtomicBool,
    workers: Arc<dyn ThreadPool>,
    stats: Arc<Stats>,
}

impl LedgerPruning {
    pub fn new(
        config: NodeConfig,
        ledger: Arc<Ledger>,
        wallets: Arc<Wallets>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            config,
            ledger,
            wallets,
            workers: Arc::new(ThreadPoolImpl::create(1, "Pruning")),
            stats,
            stopped: AtomicBool::new(false),
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.workers.stop();
    }

    pub fn build_exempt_accounts(&self) -> HashSet<Account> {
        let mut exempt: HashSet<Account> = self
            .config
            .pruning_exempt_accounts
            .iter()
            .copied()
            .collect();
        self.wallets.collect_pruning_exempt_accounts(&mut exempt);
        exempt
    }

    pub fn ledger_pruning(&self, batch_size: u64, bootstrap_weight_reached: bool) {
        self.stats.inc(StatType::Pruning, DetailType::LedgerPruning);

        let max_depth = if self.config.max_pruning_depth != 0 {
            self.config.max_pruning_depth
        } else {
            u64::MAX
        };
        let cutoff_time: UnixTimestamp = if bootstrap_weight_reached {
            UnixTimestamp::now() - Duration::from_secs(self.config.max_pruning_age_s as u64)
        } else {
            UnixTimestamp::MAX
        };
        let exempt_accounts = self.build_exempt_accounts();
        let mut pruned_count = 0;
        let mut transaction_write_count = 0;
        let mut last_account = Account::from(1); // 0 Burn account is never opened. So it can be used to break loop
        let mut pruning_targets = VecDeque::new();
        let mut target_finished = false;
        while (transaction_write_count != 0 || !target_finished)
            && !self.stopped.load(Ordering::SeqCst)
        {
            // Search pruning targets
            while pruning_targets.len() < batch_size as usize
                && !target_finished
                && !self.stopped.load(Ordering::SeqCst)
            {
                self.stats
                    .inc(StatType::Pruning, DetailType::CollectTargets);
                target_finished = self.collect_ledger_pruning_targets_with_exempt(
                    &mut pruning_targets,
                    &mut last_account,
                    batch_size * 2,
                    max_depth,
                    cutoff_time,
                    &exempt_accounts,
                );
            }
            // Pruning write operation
            transaction_write_count = self
                .ledger
                .prune_batch(&mut pruning_targets, batch_size as usize);
            pruned_count += transaction_write_count;
        }

        info!("Recently pruned blocks: {}", pruned_count);
    }

    pub fn collect_ledger_pruning_targets(
        &self,
        pruning_targets: &mut VecDeque<BlockHash>,
        last_account: &mut Account,
        batch_read_size: u64,
        max_depth: u64,
        cutoff_time: UnixTimestamp,
    ) -> bool {
        let exempt_accounts = self.build_exempt_accounts();
        self.collect_ledger_pruning_targets_with_exempt(
            pruning_targets,
            last_account,
            batch_read_size,
            max_depth,
            cutoff_time,
            &exempt_accounts,
        )
    }

    pub fn collect_ledger_pruning_targets_with_exempt(
        &self,
        pruning_targets: &mut VecDeque<BlockHash>,
        last_account: &mut Account,
        batch_read_size: u64,
        max_depth: u64,
        cutoff_time: UnixTimestamp,
        exempt_accounts: &HashSet<Account>,
    ) -> bool {
        let mut read_operations = 0;
        let mut finish_transaction = false;
        let tx = self.ledger.store.tx_begin_read();
        let mut it = self
            .ledger
            .store
            .confirmation_height
            .iter_range(&tx, *last_account..);

        while let Some((account, info)) = it.next() {
            read_operations += 1;
            if exempt_accounts.contains(&account) {
                if read_operations >= batch_read_size {
                    *last_account = account.inc_or_max();
                    finish_transaction = true;
                    break;
                }
                continue;
            }
            let mut hash = info.frontier;
            let mut depth = 0;
            while !hash.is_zero() && depth < max_depth {
                if let Some(block) = self.ledger.store.block.get(&hash) {
                    if block.timestamp() > cutoff_time.into() || depth == 0 {
                        hash = block.previous();
                    } else {
                        break;
                    }
                } else {
                    assert!(depth != 0);
                    hash = BlockHash::zero();
                }
                depth += 1;
                if depth % 256 == 0 {
                    kernel_common::watchdog::kick();
                }
            }
            if !hash.is_zero() {
                pruning_targets.push_back(hash);
            }
            read_operations += depth;
            if read_operations >= batch_read_size {
                *last_account = account.inc_or_max();
                finish_transaction = true;
                break;
            }
        }

        !finish_transaction || last_account.is_zero()
    }
}

pub trait LedgerPruningExt {
    fn start(&self);
    fn ongoing_ledger_pruning(&self);
}

impl LedgerPruningExt for Arc<LedgerPruning> {
    fn start(&self) {
        let self_w = Arc::downgrade(self);
        self.workers.post(Box::new(move || {
            if let Some(self_l) = self_w.upgrade() {
                self_l.ongoing_ledger_pruning();
            }
        }));
    }

    fn ongoing_ledger_pruning(&self) {
        let bootstrap_weight_reached =
            self.ledger.block_count() as u64 >= self.ledger.bootstrap_weight_max_blocks();
        self.ledger_pruning(2 * 1024, bootstrap_weight_reached);
        let ledger_pruning_interval = if bootstrap_weight_reached {
            Duration::from_secs(self.config.max_pruning_age_s as u64)
        } else {
            Duration::from_secs(core::cmp::min(self.config.max_pruning_age_s as u64, 15 * 60))
        };
        let node_w = Arc::downgrade(self);
        self.workers.post_delayed(
            ledger_pruning_interval,
            Box::new(move || {
                if let Some(node) = node_w.upgrade() {
                    node.ongoing_ledger_pruning()
                }
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_common::storage::{
        bpm::BufferPool, btree::BTree, init_wallet_storage, StorageContext, SHARDS, SHARD_COUNT,
        WALLET_SHARD,
    };
    use rsnano_core::{
        Amount, Block, BlockDetails, BlockSideband, ConfirmationHeightInfo, Epoch, Link,
        PrivateKey, RawKey, SavedBlock, StateBlockArgs, WalletId, WorkNonce,
    };
    use spin::Mutex;

    fn make_saved_block(
        key: &PrivateKey,
        previous: BlockHash,
        balance: Amount,
        height: u64,
        timestamp: u64,
    ) -> SavedBlock {
        let block: Block = StateBlockArgs {
            key,
            previous,
            representative: key.public_key(),
            balance,
            link: Link::zero(),
            work: WorkNonce::from(1),
        }
        .into();
        let sideband = BlockSideband {
            height,
            timestamp: timestamp.into(),
            details: BlockDetails::new(Epoch::Epoch2, true, false, false),
            ..BlockSideband::new_test_instance_for(&block)
        };
        SavedBlock::new(block, sideband)
    }

    #[test]
    fn merchant_and_stealth_accounts_exempt_from_pruning() {
        let _storage_guard = crate::wallets::TEST_STORAGE_LOCK.lock();
        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(8)));
        for i in 0..SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 64, 1 + i * 64);
            *SHARDS[i].lock() = Some(BTree::new(bpm));
        }
        *WALLET_SHARD.lock() = None;
        init_wallet_storage(storage.clone());

        let wallets = Arc::new(Wallets::new_null());
        let ledger = wallets.ledger.clone();
        ledger.enable_pruning();

        // 1. Non-exempt global account with 2 confirmed blocks
        let global_key = PrivateKey::from_bytes(&[0x11; 32]);
        let global_acc = global_key.account();
        let global_b1 = make_saved_block(&global_key, BlockHash::zero(), Amount::nano(10), 1, 100);
        let global_b2 = make_saved_block(&global_key, global_b1.hash(), Amount::nano(5), 2, 100);
        ledger.store.block.put(&global_b1);
        ledger.store.block.put(&global_b2);
        ledger.store.confirmation_height.put(
            &global_acc,
            &ConfirmationHeightInfo::new(2, global_b2.hash()),
        );

        // 2. Merchant treasury account configured in `pruning_exempt_accounts` with 2 confirmed blocks
        let treasury_key = PrivateKey::from_bytes(&[0x22; 32]);
        let treasury_acc = treasury_key.account();
        let treasury_b1 =
            make_saved_block(&treasury_key, BlockHash::zero(), Amount::nano(100), 1, 100);
        let treasury_b2 =
            make_saved_block(&treasury_key, treasury_b1.hash(), Amount::nano(90), 2, 100);
        ledger.store.block.put(&treasury_b1);
        ledger.store.block.put(&treasury_b2);
        ledger.store.confirmation_height.put(
            &treasury_acc,
            &ConfirmationHeightInfo::new(2, treasury_b2.hash()),
        );

        // 3. Stateless stealth invoice account (derived via POS wallet `next_ephemeral_account`)
        let cold_spend_raw = RawKey::from_bytes([0x33; 32]);
        let cold_spend_priv = PrivateKey::from(cold_spend_raw);
        let cold_spend_pub = cold_spend_priv.public_key();
        let scan_key = RawKey::from_bytes([0x44; 32]);

        let pos_wallet_id = WalletId::from(0x99);
        wallets.create(pos_wallet_id);
        {
            let guard = wallets.mutex.lock();
            let wallet = guard.get(&pos_wallet_id).unwrap();
            *wallet.seed.lock() = scan_key;
        }
        wallets
            .set_spend_public_key(&pos_wallet_id, Some(cold_spend_pub))
            .unwrap();

        let stealth_pub = wallets
            .next_ephemeral_account(&pos_wallet_id, Some(0))
            .unwrap();
        let stealth_acc = stealth_pub.as_account();
        let stealth_priv = PrivateKey::from_stealth(&cold_spend_raw, &scan_key, 0);
        assert_eq!(stealth_priv.account(), stealth_acc);

        let stealth_b1 =
            make_saved_block(&stealth_priv, BlockHash::zero(), Amount::nano(25), 1, 100);
        let stealth_b2 =
            make_saved_block(&stealth_priv, stealth_b1.hash(), Amount::nano(0), 2, 100);
        ledger.store.block.put(&stealth_b1);
        ledger.store.block.put(&stealth_b2);
        ledger.store.confirmation_height.put(
            &stealth_acc,
            &ConfirmationHeightInfo::new(2, stealth_b2.hash()),
        );

        // Configure pruning with max_pruning_depth = 1 and treasury_acc in pruning_exempt_accounts
        let mut config = NodeConfig::default();
        config.max_pruning_depth = 1;
        config.max_pruning_age_s = 0;
        config.pruning_exempt_accounts.push(treasury_acc);

        let stats = Arc::new(Stats::default());
        let pruning = LedgerPruning::new(config, ledger.clone(), wallets.clone(), stats);
        pruning.ledger_pruning(64, true);

        // Global account: block 1 (ancestor) was pruned; block 2 (frontier) remains
        assert!(ledger.store.block.get(&global_b1.hash()).is_none());
        assert!(ledger.store.pruned.exists(&global_b1.hash()));
        assert!(ledger.store.block.get(&global_b2.hash()).is_some());

        // Merchant treasury account: both block 1 and block 2 remain unpruned!
        assert!(ledger.store.block.get(&treasury_b1.hash()).is_some());
        assert!(!ledger.store.pruned.exists(&treasury_b1.hash()));
        assert!(ledger.store.block.get(&treasury_b2.hash()).is_some());

        // Stateless stealth invoice account: both block 1 and block 2 remain unpruned!
        assert!(ledger.store.block.get(&stealth_b1.hash()).is_some());
        assert!(!ledger.store.pruned.exists(&stealth_b1.hash()));
        assert!(ledger.store.block.get(&stealth_b2.hash()).is_some());

        *WALLET_SHARD.lock() = None;
        for i in 0..SHARD_COUNT {
            *SHARDS[i].lock() = None;
        }
    }
}
