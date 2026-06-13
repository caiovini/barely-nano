use alloc::sync::Arc;
use spin::Mutex;
use hashbrown::HashMap;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::string::String;

use tracing::info;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, Amount, BlockHash, KeyDerivationFunction, Networks,
    PendingKey, PrivateKey, PublicKey, Root, SavedBlock, WalletId, WorkNonce,
};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_work::WorkThresholds;

use super::{Wallet, WalletRepresentatives};
use crate::{
    block_processing::{BlockProcessorQueue, BlockSource},
    cementation::ConfirmingSet,
    config::{NetworkParams, NodeConfig},
    representatives::OnlineReps,
    transport::MessageFlooder,
    utils::{ThreadPool, ThreadPoolImpl},
    work::WorkFactory,
};

#[allow(dead_code)]
pub struct Wallets {
    pub mutex: Mutex<HashMap<WalletId, Arc<Wallet>>>,
    node_config: NodeConfig,
    ledger: Arc<Ledger>,
    work_factory: Arc<WorkFactory>,
    work_thresholds: WorkThresholds,
    network_params: NetworkParams,
    pub delayed_work: Mutex<HashMap<Account, Root>>,
    workers: Arc<dyn ThreadPool>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    pub wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    online_reps: Arc<Mutex<OnlineReps>>,
    kdf: KeyDerivationFunction,
    confirming_set: Arc<ConfirmingSet>,
}

impl Wallets {
    pub fn new(
        ledger: Arc<Ledger>,
        node_config: &NodeConfig,
        work: WorkThresholds,
        work_factory: Arc<WorkFactory>,
        network_params: NetworkParams,
        workers: Arc<dyn ThreadPool>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        online_reps: Arc<Mutex<OnlineReps>>,
        confirming_set: Arc<ConfirmingSet>,
        _message_flooder: MessageFlooder,
        current_network: Networks,
    ) -> Self {
        let kdf = KeyDerivationFunction::new(Self::kdf_work_for(current_network));

        Self {
            mutex: Mutex::new(HashMap::new()),
            node_config: node_config.clone(),
            ledger: Arc::clone(&ledger),
            work_factory,
            work_thresholds: work.clone(),
            network_params,
            delayed_work: Mutex::new(HashMap::new()),
            workers,
            block_processor_queue,
            wallet_reps: Arc::new(Mutex::new(WalletRepresentatives::new(
                node_config.vote_minimum,
                ledger.rep_weights.clone(),
            ))),
            online_reps,
            kdf: kdf.clone(),
            confirming_set,
        }
    }

    fn kdf_work_for(network: Networks) -> u32 {
        match network {
            Networks::NanoDevNetwork => 8,
            _ => 1024 * 64,
        }
    }

    pub fn start(&self) {}
    pub fn stop(&self) {}

    pub fn set_start_election_callback(&self, _callback: Box<dyn Fn(SavedBlock) + Send + Sync>) {}

    pub fn initialize(&mut self) -> anyhow::Result<()> {
        info!("Wallets initialized (stub)");
        Ok(())
    }

    pub fn voting_enabled(&self) -> bool {
        self.node_config.enable_voting && self.voting_reps_count() > 0
    }

    pub fn voting_reps_count(&self) -> u64 {
        self.wallet_reps.lock().voting_reps()
    }

    pub fn wallet_ids(&self) -> Vec<WalletId> {
        self.mutex.lock().keys().cloned().collect()
    }

    pub fn foreach_representative<F>(&self, mut action: F)
    where
        F: FnMut(&PrivateKey),
    {
        let guard = self.mutex.lock();
        for wallet in guard.values() {
            let keys = wallet.keys.lock();
            for prv in keys.values() {
                action(prv);
            }
        }
    }

    pub fn exists(&self, pub_key: &PublicKey) -> bool {
        let guard = self.mutex.lock();
        for wallet in guard.values() {
            if wallet.exists(pub_key) {
                return true;
            }
        }
        false
    }

    pub fn compute_reps(&self) {}

    pub fn reload(&self) {}
    
    pub fn fetch(&self, wallet_id: &WalletId, account: &Account) -> anyhow::Result<PrivateKey> {
        let guard = self.mutex.lock();
        let wallet = guard.get(wallet_id).ok_or_else(|| anyhow!("Wallet not found"))?;
        wallet.get_key(&account.into()).ok_or_else(|| anyhow!("Key not found"))
    }

    pub fn create(&self, id: WalletId) {
        let mut guard = self.mutex.lock();
        guard.insert(id, Arc::new(Wallet::new(
            self.ledger.clone(),
            self.work_thresholds.clone(),
            rsnano_core::PublicKey::zero(),
        ).unwrap()));
    }

    pub fn change_seed(&self, _id: WalletId, _seed: &rsnano_core::RawKey, _offset: u32) -> anyhow::Result<(u32, Account)> {
        // Seed-based generation is not fully implemented in memory wallet yet.
        Ok((0, Account::zero()))
    }

    pub fn new_null() -> Self {
        let node_config = NodeConfig::default();
        let ledger = Arc::new(Ledger::new_null());
        Self::new(
            ledger.clone(),
            &node_config,
            WorkThresholds::publish_dev().clone(),
            Arc::new(WorkFactory::new_null()),
            NetworkParams::new(rsnano_core::Networks::NanoDevNetwork),
            Arc::new(ThreadPoolImpl::create(1, "null")),
            Arc::new(BlockProcessorQueue::new_null()),
            Arc::new(Mutex::new(OnlineReps::new_test_instance())),
            Arc::new(ConfirmingSet::new_null()),
            MessageFlooder::new_null(),
            rsnano_core::Networks::NanoDevNetwork,
        )
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WalletsError {
    Generic,
    WalletNotFound,
    WalletLocked,
    InvalidPassword,
}

impl ContainerInfoProvider for Wallets {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::new()
    }
}

pub trait WalletsExt {
    fn insert_adhoc2(&self, wallet_id: &WalletId, key: &[u8; 32], generate_work: bool) -> anyhow::Result<PublicKey>;
    fn send_sync(
        &self,
        wallet_id: WalletId,
        source: Account,
        destination: Account,
        amount: Amount,
        work: WorkNonce,
        generate_work: bool,
        id: Option<String>,
    ) -> BlockHash;
    fn receive_sync(
        &self,
        wallet_id: WalletId,
        block: BlockHash,
        representative: PublicKey,
        amount: Amount,
        account: Account,
        work: WorkNonce,
        generate_work: bool,
    ) -> Result<SavedBlock, WalletsError>;
    fn get_representative(&self, wallet_id: WalletId) -> anyhow::Result<PublicKey>;
}

impl WalletsExt for Wallets {
    fn insert_adhoc2(&self, wallet_id: &WalletId, key: &[u8; 32], _generate_work: bool) -> anyhow::Result<PublicKey> {
        let guard = self.mutex.lock();
        let wallet = guard.get(wallet_id).ok_or_else(|| anyhow!("Wallet not found"))?;
        let priv_key = PrivateKey::from_bytes(key);
        Ok(wallet.insert_key(priv_key))
    }

    fn send_sync(
        &self,
        wallet_id: WalletId,
        source: Account,
        destination: Account,
        amount: Amount,
        work: WorkNonce,
        _generate_work: bool,
        _id: Option<String>,
    ) -> BlockHash {
        let guard = self.mutex.lock();
        let Some(wallet) = guard.get(&wallet_id) else {
            return BlockHash::zero();
        };

        if !wallet.exists(&source.into()) {
            return BlockHash::zero();
        }

        let any = self.ledger.any();
        let balance = any.account_balance(&source);
        if balance < amount {
            return BlockHash::zero();
        }

        let Some(info) = any.get_account(&source) else {
            return BlockHash::zero();
        };
        
        match wallet.create_block(
            &source.into(),
            info.head,
            info.representative,
            balance - amount,
            destination.into(),
            work
        ) {
            Ok(block) => {
                let arc_block = Arc::new(block);
                match self.block_processor_queue.push_blocking(arc_block, BlockSource::Local) {
                    Ok(Ok(saved)) => saved.hash(),
                    _ => BlockHash::zero(),
                }
            }
            Err(_) => BlockHash::zero(),
        }
    }

    fn receive_sync(
        &self,
        wallet_id: WalletId,
        send_hash: BlockHash,
        representative: PublicKey,
        _amount: Amount,
        account: Account,
        work: WorkNonce,
        _generate_work: bool,
    ) -> Result<SavedBlock, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = guard.get(&wallet_id).ok_or(WalletsError::WalletNotFound)?;

        if !wallet.exists(&account.into()) {
            return Err(WalletsError::Generic);
        }

        let any = self.ledger.any();
        let pending = any.get_pending(&PendingKey::new(account, send_hash))
            .ok_or(WalletsError::Generic)?;

        let (previous, balance, current_representative) = if let Some(info) = any.get_account(&account) {
            (info.head, info.balance + pending.amount, info.representative)
        } else {
            (BlockHash::zero(), pending.amount, representative)
        };

        let block = wallet.create_block(
            &account.into(),
            previous,
            current_representative,
            balance,
            send_hash.into(),
            work
        ).map_err(|_| WalletsError::Generic)?;

        let arc_block = Arc::new(block);

        self.block_processor_queue.push_blocking(arc_block, BlockSource::Local)
            .map_err(|_| WalletsError::Generic)?
            .map_err(|_| WalletsError::Generic)
    }

    fn get_representative(&self, wallet_id: WalletId) -> anyhow::Result<PublicKey> {
        let guard = self.mutex.lock();
        let wallet = guard.get(&wallet_id).ok_or_else(|| anyhow!("Wallet not found"))?;
        Ok(wallet.representatives.lock().iter().next().cloned().unwrap_or(PublicKey::zero()))
    }
}
