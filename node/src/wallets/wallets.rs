use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::Ordering;
use hashbrown::HashMap;
use spin::Mutex;

use tracing::info;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, Amount, BlockHash, KeyDerivationFunction, Networks, PendingKey, PrivateKey,
    PublicKey, RawKey, Root, SavedBlock, WalletId, WorkNonce,
};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_work::WorkThresholds;

use super::{KeyType, Wallet, WalletRepresentatives};
use crate::{
    block_processing::{BlockProcessorQueue, BlockSource},
    cementation::ConfirmingSet,
    config::{NetworkParams, NodeConfig},
    representatives::OnlineReps,
    transport::MessageFlooder,
    utils::{ThreadPool, ThreadPoolImpl},
    work::WorkFactory,
};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WalletsError {
    None,
    Generic,
    WalletNotFound,
    WalletLocked,
    AccountNotFound,
    InvalidPassword,
    BadPublicKey,
}

impl WalletsError {
    pub fn as_str(&self) -> &'static str {
        match self {
            WalletsError::None => "No error",
            WalletsError::Generic => "Unknown error",
            WalletsError::WalletNotFound => "Wallet not found",
            WalletsError::WalletLocked => "Wallet is locked",
            WalletsError::AccountNotFound => "Account not found",
            WalletsError::InvalidPassword => "Invalid password",
            WalletsError::BadPublicKey => "Bad public key",
        }
    }
}

impl fmt::Display for WalletsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl core::error::Error for WalletsError {}

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

    fn get_wallet<'a>(
        guard: &'a HashMap<WalletId, Arc<Wallet>>,
        wallet_id: &WalletId,
    ) -> Result<&'a Arc<Wallet>, WalletsError> {
        guard.get(wallet_id).ok_or(WalletsError::WalletNotFound)
    }

    fn random_representative(&self) -> PublicKey {
        self.node_config
            .random_representative()
            .unwrap_or(self.network_params.ledger.genesis_account.into())
    }

    pub fn start(&self) {}
    pub fn stop(&self) {}

    pub fn set_start_election_callback(&self, _callback: Box<dyn Fn(SavedBlock) + Send + Sync>) {}

    pub fn initialize(&mut self) -> anyhow::Result<()> {
        info!("Wallets initialized (in-memory)");
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

    pub fn wallet_exists(&self, wallet_id: &WalletId) -> bool {
        self.mutex.lock().contains_key(wallet_id)
    }

    pub fn destroy(&self, id: &WalletId) {
        self.mutex.lock().remove(id);
    }

    pub fn foreach_representative<F>(&self, mut action: F)
    where
        F: FnMut(&PrivateKey),
    {
        let guard = self.mutex.lock();
        for wallet in guard.values() {
            if wallet.valid_password() {
                let keys = wallet.keys.lock();
                for prv in keys.values() {
                    action(prv);
                }
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

    pub fn fetch(&self, wallet_id: &WalletId, account: &Account) -> Result<PrivateKey, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        wallet
            .get_key(&(*account).into())
            .ok_or(WalletsError::AccountNotFound)
    }

    pub fn create(&self, id: WalletId) {
        let mut guard = self.mutex.lock();
        let rep = self.random_representative();
        guard.insert(
            id,
            Arc::new(
                Wallet::new(self.ledger.clone(), self.work_thresholds.clone(), rep).unwrap(),
            ),
        );
    }

    pub fn change_seed(
        &self,
        id: WalletId,
        seed: &RawKey,
        count: u32,
    ) -> Result<(u32, Account), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        *wallet.seed.lock() = *seed;
        wallet.deterministic_index.store(0, Ordering::SeqCst);
        let mut last_account = Account::zero();
        let mut restored = 0;
        let to_generate = if count == 0 { 1 } else { count };
        for _ in 0..to_generate {
            let pk = wallet.insert_deterministic();
            last_account = pk.into();
            restored += 1;
        }
        Ok((restored, last_account))
    }

    pub fn get_accounts_of_wallet(
        &self,
        wallet_id: &WalletId,
    ) -> Result<Vec<Account>, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        let mut accounts: Vec<Account> = wallet.keys.lock().keys().map(|k| (*k).into()).collect();
        for watch_key in wallet.watch_keys.lock().iter() {
            let acc = (*watch_key).into();
            if !accounts.contains(&acc) {
                accounts.push(acc);
            }
        }
        Ok(accounts)
    }

    pub fn remove_key(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        let removed_key = wallet.keys.lock().remove(pub_key).is_some();
        let removed_watch = wallet.watch_keys.lock().remove(pub_key);
        wallet.deterministic_keys.lock().remove(pub_key);
        wallet.work_cache.lock().remove(pub_key);
        if !removed_key && !removed_watch {
            return Err(WalletsError::AccountNotFound);
        }
        Ok(())
    }

    pub fn move_accounts(
        &self,
        source_id: &WalletId,
        target_id: &WalletId,
        accounts: &[PublicKey],
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let source = Self::get_wallet(&guard, source_id)?;
        let target = Self::get_wallet(&guard, target_id)?;
        if !source.valid_password() || !target.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        {
            let src_keys = source.keys.lock();
            for pk in accounts {
                if !src_keys.contains_key(pk) {
                    return Err(WalletsError::AccountNotFound);
                }
            }
        }
        for pk in accounts {
            if let Some(prv) = source.keys.lock().remove(pk) {
                source.deterministic_keys.lock().remove(pk);
                target.keys.lock().insert(*pk, prv);
            }
        }
        Ok(())
    }

    pub fn insert_watch(
        &self,
        wallet_id: &WalletId,
        accounts: &[Account],
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        let mut watch = wallet.watch_keys.lock();
        for account in accounts {
            if account.is_zero() {
                return Err(WalletsError::BadPublicKey);
            }
            watch.insert((*account).into());
        }
        Ok(())
    }

    pub fn valid_password(&self, wallet_id: &WalletId) -> Result<bool, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        Ok(wallet.valid_password())
    }

    pub fn lock(&self, wallet_id: &WalletId) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        wallet.locked.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn rekey(
        &self,
        wallet_id: &WalletId,
        password: impl AsRef<str>,
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        *wallet.password.lock() = String::from(password.as_ref());
        Ok(())
    }

    pub fn work_get2(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
    ) -> Result<WorkNonce, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.exists(pub_key) {
            return Err(WalletsError::AccountNotFound);
        }
        Ok(wallet
            .work_cache
            .lock()
            .get(pub_key)
            .copied()
            .unwrap_or(WorkNonce::from(1)))
    }

    pub fn work_set(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
        work: WorkNonce,
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.exists(pub_key) {
            return Err(WalletsError::AccountNotFound);
        }
        wallet.work_cache.lock().insert(*pub_key, work);
        Ok(())
    }

    pub fn key_type(&self, wallet_id: WalletId, pub_key: &PublicKey) -> KeyType {
        let guard = self.mutex.lock();
        match guard.get(&wallet_id) {
            Some(wallet) => wallet.key_type(pub_key),
            None => KeyType::Unknown,
        }
    }

    pub fn deterministic_index_get(&self, wallet_id: &WalletId) -> Result<u32, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        Ok(wallet.deterministic_index.load(Ordering::SeqCst))
    }

    pub fn decrypt(&self, wallet_id: WalletId) -> Result<Vec<(PublicKey, RawKey)>, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        let mut result: Vec<(PublicKey, RawKey)> = wallet
            .keys
            .lock()
            .iter()
            .map(|(pk, prv)| (*pk, prv.raw_key()))
            .collect();
        for watch_pk in wallet.watch_keys.lock().iter() {
            if !result.iter().any(|(pk, _)| pk == watch_pk) {
                result.push((*watch_pk, RawKey::zero()));
            }
        }
        Ok(result)
    }

    pub fn serialize(&self, wallet_id: WalletId) -> Result<String, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        let rep: Account = (*wallet.representative.lock()).into();
        let seed = *wallet.seed.lock();
        let index = wallet.deterministic_index.load(Ordering::SeqCst);
        Ok(format!(
            "{{\"representative\":\"{}\",\"seed\":\"{}\",\"deterministic_index\":\"{}\"}}",
            rep.encode_account(),
            seed.encode_hex(),
            index
        ))
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

impl ContainerInfoProvider for Wallets {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::new()
    }
}

pub trait WalletsExt {
    fn insert_adhoc2(
        &self,
        wallet_id: &WalletId,
        key: &[u8; 32],
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;
    fn deterministic_insert_at(
        &self,
        wallet_id: &WalletId,
        index: u32,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;
    fn deterministic_insert2(
        &self,
        wallet_id: &WalletId,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;
    fn enter_password(&self, wallet_id: WalletId, password: &str) -> Result<(), WalletsError>;
    fn search_receivable_wallet(&self, wallet_id: WalletId) -> Result<(), WalletsError>;
    fn search_receivable_all(&self);
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
    fn get_representative(&self, wallet_id: WalletId) -> Result<PublicKey, WalletsError>;
    fn set_representative(
        &self,
        wallet_id: WalletId,
        rep: PublicKey,
        update_existing_accounts: bool,
    ) -> Result<(), WalletsError>;
}

impl WalletsExt for Wallets {
    fn insert_adhoc2(
        &self,
        wallet_id: &WalletId,
        key: &[u8; 32],
        _generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        let priv_key = PrivateKey::from_bytes(key);
        Ok(wallet.insert_key(priv_key))
    }

    fn deterministic_insert_at(
        &self,
        wallet_id: &WalletId,
        index: u32,
        _generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        Ok(wallet.insert_deterministic_at(index))
    }

    fn deterministic_insert2(
        &self,
        wallet_id: &WalletId,
        _generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        Ok(wallet.insert_deterministic())
    }

    fn enter_password(&self, wallet_id: WalletId, password: &str) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        if wallet.password.lock().as_str() == password {
            wallet.locked.store(false, Ordering::SeqCst);
            Ok(())
        } else {
            wallet.locked.store(true, Ordering::SeqCst);
            Err(WalletsError::InvalidPassword)
        }
    }

    fn search_receivable_wallet(&self, wallet_id: WalletId) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        Ok(())
    }

    fn search_receivable_all(&self) {}

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

        if !wallet.valid_password() || !wallet.exists(&source.into()) {
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
            work,
        ) {
            Ok(block) => {
                let arc_block = Arc::new(block);
                match self
                    .block_processor_queue
                    .push_blocking(arc_block, BlockSource::Local)
                {
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

        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }

        if !wallet.exists(&account.into()) {
            return Err(WalletsError::AccountNotFound);
        }

        let any = self.ledger.any();
        let pending = any
            .get_pending(&PendingKey::new(account, send_hash))
            .ok_or(WalletsError::Generic)?;

        let (previous, balance, current_representative) =
            if let Some(info) = any.get_account(&account) {
                (
                    info.head,
                    info.balance + pending.amount,
                    info.representative,
                )
            } else {
                (BlockHash::zero(), pending.amount, representative)
            };

        let block = wallet
            .create_block(
                &account.into(),
                previous,
                current_representative,
                balance,
                send_hash.into(),
                work,
            )
            .map_err(|_| WalletsError::Generic)?;

        let arc_block = Arc::new(block);

        self.block_processor_queue
            .push_blocking(arc_block, BlockSource::Local)
            .map_err(|_| WalletsError::Generic)?
            .map_err(|_| WalletsError::Generic)
    }

    fn get_representative(&self, wallet_id: WalletId) -> Result<PublicKey, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        Ok(*wallet.representative.lock())
    }

    fn set_representative(
        &self,
        wallet_id: WalletId,
        rep: PublicKey,
        _update_existing_accounts: bool,
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, &wallet_id)?;
        if !wallet.valid_password() {
            return Err(WalletsError::WalletLocked);
        }
        *wallet.representative.lock() = rep;
        Ok(())
    }
}
