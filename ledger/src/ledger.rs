use crate::{
    block_cementer::BlockCementer,
    block_insertion::{BlockInserter, BlockValidatorFactory},
    vote_verifier::VoteVerifier,
    AnySet, BlockRollbackPerformer, GenerateCacheFlags, LedgerConstants, LedgerSet, OwningAnySet,
    OwningConfirmedSet, OwningUnconfirmedSet, RepWeightCache, RepWeightsUpdater, RollbackError,
};
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use rsnano_core::{
    utils::SocketAddrV6, Account, AccountInfo, Amount, Block, BlockHash, ConfirmationHeightInfo,
    Epoch, Link, PendingInfo, PendingKey, PublicKey, QualifiedRoot, Root, SavedBlock,
};
use rsnano_ledger_store::BtreeStore;
use rsnano_nullable_clock::Timestamp;
use rsnano_stats::{DetailType, StatType, Stats};

#[derive(PartialEq, Eq, Debug, Clone, Copy, EnumCount, EnumIter, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BlockError {
    /// Signature was bad, forged or transmission error
    BadSignature,
    /// Already seen and was valid
    Old,
    /// Malicious attempt to spend a negative amount
    NegativeSpend,
    /// Malicious fork based on previous
    Fork,
    /// Source block doesn't exist, has already been received, or requires an account upgrade (epoch blocks)
    Unreceivable,
    /// Block marked as previous is unknown
    GapPrevious,
    /// Block marked as source is unknown
    GapSource,
    /// Block marked as pending blocks required for epoch open block are unknown
    GapEpochOpenPending,
    /// Block attempts to open the burn account
    OpenedBurnAccount,
    /// Balance and amount delta don't match
    BalanceMismatch,
    /// Representative is changed when it is not allowed
    RepresentativeMismatch,
    /// This block cannot follow the previous block
    BlockPosition,
    /// Insufficient work for this block, even though it passed the minimal validation
    InsufficientWork,
    /// The account got updated while this block was processed. This block is ether old or a fork.
    Conflict,
}

impl BlockError {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockError::BadSignature => "Bad signature",
            BlockError::Old => "Old",
            BlockError::NegativeSpend => "Negative spend",
            BlockError::Fork => "Fork",
            BlockError::Unreceivable => "Unreceivable",
            BlockError::GapPrevious => "Gap previous",
            BlockError::GapSource => "Gap source",
            BlockError::GapEpochOpenPending => "Gap epoch open pendign",
            BlockError::OpenedBurnAccount => "Opened burn account",
            BlockError::BalanceMismatch => "Balance mismatch",
            BlockError::RepresentativeMismatch => "Representative mismatch",
            BlockError::BlockPosition => "Block position",
            BlockError::InsufficientWork => "Insufficient work",
            BlockError::Conflict => "Conflict",
        }
    }
}

impl From<BlockError> for DetailType {
    fn from(value: BlockError) -> Self {
        match value {
            BlockError::BadSignature => Self::BadSignature,
            BlockError::Old => Self::Old,
            BlockError::NegativeSpend => Self::NegativeSpend,
            BlockError::Fork => Self::Fork,
            BlockError::Unreceivable => Self::Unreceivable,
            BlockError::GapPrevious => Self::GapPrevious,
            BlockError::GapSource => Self::GapSource,
            BlockError::GapEpochOpenPending => Self::GapEpochOpenPending,
            BlockError::OpenedBurnAccount => Self::OpenedBurnAccount,
            BlockError::BalanceMismatch => Self::BalanceMismatch,
            BlockError::RepresentativeMismatch => Self::RepresentativeMismatch,
            BlockError::BlockPosition => Self::BlockPosition,
            BlockError::InsufficientWork => Self::InsufficientWork,
            BlockError::Conflict => Self::Conflict,
        }
    }
}

pub struct Ledger {
    pub store: BtreeStore,
    pub rep_weights_updater: RepWeightsUpdater,
    pub rep_weights: Arc<RepWeightCache>,
    pub constants: LedgerConstants,
    pruning: AtomicBool,
    pub(crate) stats: Arc<Stats>,
}

pub struct NullLedgerBuilder {
    min_rep_weight: Amount,
}

impl NullLedgerBuilder {
    fn new() -> Self {
        Self {
            min_rep_weight: Amount::zero(),
        }
    }

    pub fn block(self, _block: &SavedBlock) -> Self {
        // Mock for now
        self
    }

    pub fn blocks<'a>(self, _blocks: impl IntoIterator<Item = &'a SavedBlock>) -> Self {
        self
    }

    pub fn peers(self, _peers: impl IntoIterator<Item = (SocketAddrV6, Timestamp)>) -> Self {
        self
    }

    pub fn confirmation_height(self, _account: &Account, _info: &ConfirmationHeightInfo) -> Self {
        self
    }

    pub fn account_info(self, _account: &Account, _info: &AccountInfo) -> Self {
        self
    }

    pub fn pending(self, _key: &PendingKey, _info: &PendingInfo) -> Self {
        self
    }

    pub fn pruned(self, _hash: &BlockHash) -> Self {
        self
    }

    pub fn finish(self) -> Ledger {
        Ledger::new(
            BtreeStore::new(),
            LedgerConstants::unit_test(),
            self.min_rep_weight,
            Arc::new(RepWeightCache::new()),
            Arc::new(Stats::default()),
            1,
        )
        .unwrap()
    }
}

impl Ledger {
    pub fn new_null() -> Self {
        Self::new(
            BtreeStore::new(),
            LedgerConstants::unit_test(),
            Amount::zero(),
            Arc::new(RepWeightCache::new()),
            Arc::new(Stats::default()),
            1,
        )
        .unwrap()
    }

    pub fn store_vendor(&self) -> &'static str {
        "B-tree"
    }

    pub fn version(&self) -> u32 {
        1
    }

    pub fn pruned_count(&self) -> usize {
        self.store.cache.pruned_count.load(Ordering::SeqCst) as usize
    }

    pub fn genesis(&self) -> &Block {
        &self.constants.genesis_block
    }

    pub fn new_null_builder() -> NullLedgerBuilder {
        NullLedgerBuilder::new()
    }

    pub(crate) fn new(
        mut store: BtreeStore,
        constants: LedgerConstants,
        min_rep_weight: Amount,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
        thread_count: usize,
    ) -> anyhow::Result<Self> {
        store.cache = rep_weights.ledger_cache.clone();

        let rep_weights_updater =
            RepWeightsUpdater::new(store.rep_weight.clone(), min_rep_weight, &rep_weights);

        let mut ledger = Self {
            rep_weights,
            rep_weights_updater,
            store,
            constants,
            pruning: AtomicBool::new(false),
            stats,
        };

        ledger.initialize(thread_count, &GenerateCacheFlags::new())?;

        Ok(ledger)
    }

    fn initialize(
        &mut self,
        _thread_count: usize,
        _generate_cache: &GenerateCacheFlags,
    ) -> anyhow::Result<()> {
        kernel_common::watchdog::kick();
        kernel_common::uart::print(b"[ledger] Initializing ledger...\n");
        let genesis = self.constants.genesis_block.clone();
        let genesis_account = self.constants.genesis_block.account();

        for attempt in 0..2 {
            kernel_common::watchdog::kick();
            self.store.generate_cache();

            // 1. Ensure genesis block exists
            let exists = self.any().block_exists_or_pruned(&genesis.hash());
            kernel_common::uart::print(b"[ledger] Checked genesis block. Exists=");
            if exists {
                kernel_common::uart::print(b"true\n");
            } else {
                kernel_common::uart::print(b"false\n");
            }

            if !exists {
                kernel_common::uart::print(b"[ledger] Genesis block not found, inserting (attempt ");
                kernel_common::uart::print_usize(attempt);
                kernel_common::uart::print(b")...\n");
                
                if let Err(e) = self.process_one(&genesis) {
                    tracing::error!("Failed to process genesis block: {:?}", e);
                    kernel_common::uart::print(b"[ledger] FAILED to process genesis block!\n");
                    if attempt == 0 {
                        tracing::error!("FATAL: Ledger corruption detected during genesis insertion. Wiping database...");
                        kernel_common::uart::print(b"[ledger] FATAL: Corruption during genesis insertion. Wiping database...\n");
                        self.store.wipe();
                        continue;
                    } else {
                        return Err(anyhow::anyhow!("Could not insert genesis: {:?}", e));
                    }
                } else {
                    tracing::debug!("Genesis block processed successfully.");
                    kernel_common::uart::print(b"[ledger] Genesis block processed successfully.\n");
                    // Diagnostic: Verify we can retrieve it immediately
                    if let Some(_found) = self.any().get_block(&genesis.hash()) {
                        tracing::debug!("VERIFIED: Genesis block is retrievable from store.");
                        kernel_common::uart::print(b"[ledger] VERIFIED: Genesis block is retrievable from store.\n");
                    } else {
                        tracing::error!("CRITICAL: Genesis block NOT FOUND in store immediately after processing!");
                        kernel_common::uart::print(b"[ledger] CRITICAL: Genesis block NOT FOUND in store immediately after processing!\n");
                    }
                }
                self.confirm(genesis.hash());
                let _ = self.store.flush();
            }

            // 2. Ensure account info exists and is consistent
            let info = self.any().get_account(&genesis_account);
            tracing::debug!(
                "Ledger: initial lookup of genesis account: found={}",
                info.is_some()
            );
            match info {
                Some(info) => {
                    // Check if head block exists
                    if !self.any().block_exists_or_pruned(&info.head) {
                        tracing::warn!(
                            "Ledger inconsistency: Head block {} NOT found for genesis account. Resetting to genesis...",
                            info.head
                        );
                        kernel_common::uart::print(b"[ledger] Ledger inconsistency: Head block NOT found for genesis account. Resetting...\n");
                        // Reset account info to genesis
                        let mut new_info = info.clone();
                        new_info.head = genesis.hash();
                        new_info.balance = self.constants.genesis_amount;
                        new_info.block_count = 1;
                        self.store.account.put(&genesis_account, &new_info);
                        self.store.flush().expect("Could not flush during recovery");
                        tracing::debug!("Ledger: Genesis account reset to consistent state.");
                        kernel_common::uart::print(b"[ledger] Genesis account reset to consistent state.\n");
                        continue;
                    }
                }
                None => {
                    tracing::warn!(
                        "Genesis account info NOT found for account {}! Recreating from genesis block...",
                        genesis_account.encode_account()
                    );
                    kernel_common::uart::print(b"[ledger] Ledger inconsistency: Genesis account info NOT found. Recreating...\n");
                    let mut info = AccountInfo::default();
                    info.head = genesis.hash();
                    info.open_block = genesis.hash();
                    info.balance = self.constants.genesis_amount;
                    info.block_count = 1;
                    self.store.account.put(&genesis_account, &info);
                    self.store.flush().expect("Could not flush during recovery");
                    tracing::debug!("Ledger: Genesis account info recreated successfully.");
                    kernel_common::uart::print(b"[ledger] Genesis account info recreated successfully.\n");
                    continue;
                }
            }

            // If we reached here, basic integrity is confirmed
            if attempt > 0 {
                tracing::debug!("Ledger successfully recovered and initialized.");
                kernel_common::uart::print(b"[ledger] Ledger successfully recovered and initialized.\n");
            }
            break;
        }

        kernel_common::uart::print(b"[ledger] Fetching genesis account info...\n");
        let _info = self.any().get_account(&genesis_account).unwrap();
        kernel_common::uart::print(b"[ledger] Fetching genesis account info done.\n");

        kernel_common::uart::print(b"[ledger] Iterating accounts...\n");
        let _accounts = self.any().iter_accounts();
        kernel_common::uart::print(b"[ledger] Iterating accounts done.\n");

        kernel_common::uart::print(b"[ledger] Tracing block successors...\n");
        let mut current = genesis.hash();
        let mut count = 0;
        while !current.is_zero() && count < 10 {
            kernel_common::watchdog::kick();
            if let Some(_block) = self.any().get_block(&current) {
                kernel_common::uart::print(b"[ledger]   Block successor: ");
                kernel_common::uart::print_usize(count);
                kernel_common::uart::print(b"\n");
                current = self.any().block_successor(&current).unwrap_or_default();
            } else {
                kernel_common::uart::print(b"[ledger]   Block successor NOT FOUND.\n");
                break;
            }
            count += 1;
        }
        kernel_common::uart::print(b"[ledger] Tracing block successors done.\n");

        Ok(())
    }

    pub fn any(&self) -> OwningAnySet<'_> {
        OwningAnySet::new(&self.store, &self.constants)
    }

    pub fn confirmed(&self) -> OwningConfirmedSet<'_> {
        OwningConfirmedSet::new(&self.store)
    }

    pub fn unconfirmed(&self) -> impl LedgerSet + use<'_> {
        OwningUnconfirmedSet::new(&self.store)
    }

    pub fn pruning_enabled(&self) -> bool {
        self.pruning.load(Ordering::SeqCst)
    }

    pub fn enable_pruning(&self) {
        self.pruning.store(true, Ordering::SeqCst);
    }

    pub fn bootstrap_weight_max_blocks(&self) -> u64 {
        self.rep_weights.bootstrap_weight_max_blocks()
    }

    pub fn work_thresholds(&self) -> &rsnano_work::WorkThresholds {
        &self.constants.work
    }

    /// Returns the cached vote weight for the given representative.
    /// If the weight is below the cache limit it returns 0.
    /// During bootstrap it returns the preconfigured bootstrap weights.
    pub fn weight(&self, rep: &PublicKey) -> Amount {
        self.rep_weights.weight(rep)
    }

    pub fn verify_votes(
        &self,
        candidates: VecDeque<(Root, BlockHash)>,
        is_final: bool,
    ) -> VecDeque<(Root, BlockHash)> {
        let verifier = VoteVerifier {
            constants: &self.constants,
            store: &self.store,
        };
        verifier.verify_votes(candidates, is_final)
    }

    pub fn backlog_count(&self) -> usize {
        self.block_count().saturating_sub(self.confirmed_count())
    }

    pub fn account_count(&self) -> usize {
        self.store.cache.account_count.load(Ordering::SeqCst) as usize
    }

    pub fn block_count(&self) -> usize {
        self.store.cache.block_count.load(Ordering::SeqCst) as usize
    }

    pub fn confirmed_count(&self) -> usize {
        self.store.cache.confirmed_count.load(Ordering::SeqCst) as usize
    }

    pub fn is_epoch_link(&self, link: &Link) -> bool {
        self.constants.epochs.is_epoch_link(link)
    }

    pub fn epoch_signer(&self, link: &Link) -> Option<Account> {
        self.constants.epochs.epoch_signer(link)
    }

    pub fn epoch_link(&self, epoch: Epoch) -> Option<Link> {
        self.constants.epochs.link(epoch).cloned()
    }

    pub(crate) fn update_account(
        &self,
        _txn: &mut (),
        account: &Account,
        old_info: &AccountInfo,
        new_info: &AccountInfo,
    ) {
        if !new_info.head.is_zero() {
            if old_info.head.is_zero() && new_info.open_block == new_info.head {
                self.store
                    .cache
                    .account_count
                    .fetch_add(1, Ordering::SeqCst);
            }
            if !old_info.head.is_zero() && old_info.epoch != new_info.epoch {
                self.store.account.del(account);
            }
            self.store.account.put(account, new_info);
        } else {
            self.store.account.del(account);
            self.store
                .cache
                .account_count
                .fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn prune_batch(&self, targets: &mut VecDeque<BlockHash>, batch_size: usize) -> usize {
        let mut transaction_write_count = 0;
        if !targets.is_empty() {
            while !targets.is_empty() && transaction_write_count < batch_size {
                let pruning_hash = targets.front().unwrap();
                let account_pruned_count = self.pruning_action(pruning_hash, batch_size as u64);
                transaction_write_count += account_pruned_count as usize;
                targets.pop_front();
            }
        }
        transaction_write_count
    }

    pub fn prune_one(&self, target: &BlockHash, batch_size: usize) -> usize {
        self.pruning_action(target, batch_size as u64) as usize
    }

    fn pruning_action(&self, hash: &BlockHash, _batch_size: u64) -> u64 {
        self.stats.inc(StatType::Pruning, DetailType::PruningTarget);
        let mut pruned_count = 0;
        let mut hash = *hash;
        let genesis_hash = self.constants.genesis_block.hash();
        let any = OwningAnySet::new(&self.store, &self.constants);

        while !hash.is_zero() && hash != genesis_hash {
            if let Some(block) = any.get_block(&hash) {
                self.store.block.del(&hash);
                self.store.pruned.put(&hash);
                hash = block.previous();
                pruned_count += 1;
                self.store.cache.pruned_count.fetch_add(1, Ordering::SeqCst);
            } else if self.store.pruned.exists(&hash) {
                hash = BlockHash::zero();
            } else {
                break;
            }
        }

        self.stats
            .add(StatType::Pruning, DetailType::PrunedCount, pruned_count);

        pruned_count
    }

    pub fn roll_back(&self, block: &BlockHash) -> Result<usize, RollbackError> {
        let result = self.roll_back_batch(&[*block], usize::MAX, |_| true);
        let rolled_back = result.0[0].rolled_back.len();
        result.0[0]
            .error
            .as_ref()
            .map_or(Ok(rolled_back), |e| Err(e.clone()))
    }

    pub fn roll_back_batch<'a, T, F>(
        &self,
        targets: T,
        max_rollbacks: usize,
        mut can_roll_back: F,
    ) -> RollbackResults
    where
        T: IntoIterator<Item = &'a BlockHash>,
        F: FnMut(&BlockHash) -> bool,
    {
        self.stats
            .inc(StatType::BoundedBacklog, DetailType::PerformingRollbacks);

        let mut rolled_back_count = 0;
        let mut results = RollbackResults(Vec::new());

        for hash in targets {
            if !can_roll_back(hash) {
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: QualifiedRoot::ZERO,
                    rolled_back: Vec::new(),
                    error: Some(RollbackError::Rejected),
                });
                continue;
            }

            if let Some(block) = self.store.block.get(hash) {
                let (rollback_list, error) = self.roll_back_with_tx(&block.hash());
                rolled_back_count += rollback_list.len();
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: block.qualified_root(),
                    rolled_back: rollback_list,
                    error,
                });

                if rolled_back_count >= max_rollbacks {
                    break;
                }
            } else {
                results.push(RollbackResult {
                    target_hash: *hash,
                    target_root: QualifiedRoot::ZERO,
                    rolled_back: Vec::new(),
                    error: Some(RollbackError::BlockNotFound),
                });
            }
        }

        results
    }

    fn roll_back_with_tx(&self, block: &BlockHash) -> (Vec<SavedBlock>, Option<RollbackError>) {
        let mut tx = ();
        let mut performer = BlockRollbackPerformer::new(self, &mut tx);
        match performer.roll_back(block) {
            Ok(()) => (performer.rolled_back, None),
            Err(e) => (performer.rolled_back, Some(e)),
        }
    }

    pub fn process_one(&self, block: &Block) -> Result<SavedBlock, BlockError> {
        let mut result = self.process_batch(core::iter::once(block));
        let mut drain = result.processed.drain(..);
        match drain.next().unwrap() {
            (Ok(_), Some(block)) => Ok(block),
            (Ok(_), None) => unreachable!(),
            (Err(e), _) => Err(e),
        }
    }

    pub fn process_batch<'a>(
        &self,
        batch: impl IntoIterator<Item = &'a Block>,
    ) -> BatchProcessResult {
        self.process_batch_with_verifications(batch.into_iter().map(|b| (b, false)))
    }

    pub fn process_batch_with_verifications<'a>(
        &self,
        batch: impl IntoIterator<Item = (&'a Block, bool)>,
    ) -> BatchProcessResult {
        let mut processed = Vec::new();

        for (block, signature_verified) in batch.into_iter() {
            let hash = block.hash();
            tracing::debug!("Ledger: validating block {}", hash);

            let any = OwningAnySet::new(&self.store, &self.constants);
            let validator =
                BlockValidatorFactory::new(&any, &self.constants, block).create_validator(signature_verified);
            let result = validator.validate();

            match result {
                Ok(instructions) => {
                    if let Some(saved_block) =
                        BlockInserter::new(self, &mut (), block, &instructions).insert()
                    {
                        self.store.cache.block_count.fetch_add(1, Ordering::SeqCst);
                        tracing::debug!(
                            "Ledger: processed block {} (Account: {}, Height: {})",
                            saved_block.hash(),
                            saved_block.account().encode_account(),
                            saved_block.height()
                        );
                        processed.push((Ok(()), Some(saved_block)));
                    } else {
                        tracing::debug!("Ledger: conflict for block {}", block.hash());
                        processed.push((Err(BlockError::Conflict), None));
                    }
                }
                Err(err) => {
                    tracing::debug!("Ledger: validation failed for block {}! Error: {:?}", hash, err);

                    tracing::debug!(
                        "Ledger: validation failed for block {}! Error: {:?}",
                        block.hash(),
                        err
                    );
                    processed.push((Err(err), None));
                }
            }
        }

        BatchProcessResult { processed }
    }

    pub fn roll_back_competitors<'a, T, F>(&self, blocks: T, mut rolled_back_callback: F)
    where
        T: IntoIterator<Item = &'a Block>,
        F: FnMut(RollbackResults),
    {
        let mut rolled_back = RollbackResults(Vec::new());
        for block in blocks {
            let rolled_back_blocks = self.rollback_competitor(block);
            if !rolled_back_blocks.is_empty() {
                rolled_back.push(RollbackResult {
                    target_hash: block.hash(),
                    target_root: block.qualified_root(),
                    rolled_back: rolled_back_blocks,
                    error: None,
                });
            }
        }
        if !rolled_back.is_empty() {
            rolled_back_callback(rolled_back);
        }
    }

    fn rollback_competitor(&self, fork_block: &Block) -> Vec<SavedBlock> {
        let mut rollback_list = Vec::new();
        let hash = fork_block.hash();
        if let Some(successor) =
            self.block_successor_by_qualified_root(&fork_block.qualified_root())
        {
            if successor != hash {
                let (list, error) = self.roll_back_with_tx(&successor);
                rollback_list = list;
                if error.is_none() {
                    self.stats.inc(StatType::Ledger, DetailType::Rollback);
                } else {
                    self.stats.inc(StatType::Ledger, DetailType::RollbackFailed);
                }
            }
        }
        rollback_list
    }

    fn block_successor_by_qualified_root(&self, root: &QualifiedRoot) -> Option<BlockHash> {
        if !root.previous.is_zero() {
            self.store.successors.get(&root.previous)
        } else {
            self.store
                .account
                .get(&root.root.into())
                .map(|i| i.open_block)
        }
    }

    pub fn confirm(&self, hash: BlockHash) -> Vec<SavedBlock> {
        self.confirm_max(hash, 1024 * 128)
    }

    fn confirm_max(&self, target_hash: BlockHash, max_blocks: usize) -> Vec<SavedBlock> {
        BlockCementer::new(&self.store, &self.constants, &self.stats)
            .confirm(target_hash, max_blocks)
    }

    pub fn confirm_batch<'a, O>(
        &self,
        batch: impl IntoIterator<Item = &'a BlockHash>,
        stopped: &AtomicBool,
        max_blocks: usize,
        cementing_observer: &mut O,
    ) where
        O: CementingObserver,
    {
        let mut confirmed = Vec::new();
        let mut blocks_confirmed = 0;

        for confirmation_root in batch.into_iter() {
            loop {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }

                if blocks_confirmed >= max_blocks {
                    blocks_confirmed = 0;
                    cementing_observer.batch_confirmed(confirmed);
                    confirmed = Vec::new();
                }

                if !self.store.block.exists(confirmation_root) {
                    break;
                }

                let added = self.confirm_max(*confirmation_root, max_blocks);

                if !added.is_empty() {
                    blocks_confirmed += added.len();
                    for block in added {
                        confirmed.push((block, *confirmation_root));
                    }
                } else {
                    break;
                }
            }
        }
    }
}

pub struct BatchProcessResult {
    pub processed: Vec<(Result<(), BlockError>, Option<SavedBlock>)>,
}

#[derive(Debug)]
pub struct RollbackResult {
    pub target_hash: BlockHash,
    pub target_root: QualifiedRoot,
    pub rolled_back: Vec<SavedBlock>,
    pub error: Option<RollbackError>,
}

#[derive(Debug, Default)]
pub struct RollbackResults(pub Vec<RollbackResult>);

impl RollbackResults {
    pub fn hashes(&self) -> Vec<BlockHash> {
        self.0
            .iter()
            .flat_map(|r| r.rolled_back.iter().map(|b| b.hash()))
            .collect()
    }

    pub fn roots(&self) -> Vec<rsnano_core::QualifiedRoot> {
        self.0.iter().map(|r| r.target_root.clone()).collect()
    }

    pub fn affected_accounts(&self) -> Vec<rsnano_core::Account> {
        self.0.iter().map(|r| r.target_root.root.into()).collect()
    }

    pub fn iter(&self) -> core::slice::Iter<'_, RollbackResult> {
        self.0.iter()
    }

    pub fn push(&mut self, result: RollbackResult) {
        self.0.push(result);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn extend(&mut self, other: RollbackResults) {
        self.0.extend(other.0);
    }
}

pub trait CementingObserver {
    fn batch_confirmed(&mut self, blocks: Vec<(SavedBlock, BlockHash)>);
}
