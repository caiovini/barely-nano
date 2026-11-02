use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::Ordering;
use hashbrown::{HashMap, HashSet};
use spin::Mutex;

use tracing::{info, warn};

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
        let persisted = self.ledger.store.wallet.load_all();
        {
            let mut guard = self.mutex.lock();
            for (wallet_id, mut meta, keys) in persisted {
                if meta.representative.is_zero() {
                    meta.representative = self.random_representative();
                }
                let wallet = Wallet::from_persisted(
                    self.ledger.clone(),
                    self.work_thresholds.clone(),
                    meta,
                    keys,
                );
                guard.insert(wallet_id, Arc::new(wallet));
            }
            info!("Found {} wallet(s) in B-Tree storage", guard.len());
            for id in guard.keys() {
                info!("Wallet: {}", id);
            }
        }
        self.compute_reps();
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
        let removed = self.mutex.lock().remove(id).is_some();
        self.ledger.store.wallet.destroy_wallet(id);
        if removed {
            self.compute_reps();
        }
    }

    pub fn foreach_representative<F>(&self, mut action: F)
    where
        F: FnMut(&PrivateKey),
    {
        if self.node_config.enable_voting {
            let mut action_accounts_l: Vec<PrivateKey> = Vec::new();
            {
                let guard = self.mutex.lock();
                for (wallet_id, wallet) in guard.iter() {
                    let representatives_l = wallet.representatives.lock().clone();
                    for account in representatives_l {
                        if !self.ledger.weight(&account).is_zero() {
                            if wallet.valid_password() {
                                if let Some(prv) = wallet.get_key(&account) {
                                    action_accounts_l.push(prv);
                                }
                            } else {
                                warn!("Representative locked inside wallet {}", wallet_id);
                            }
                        }
                    }
                }
            }
            for keys in action_accounts_l {
                action(&keys);
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

    pub fn compute_reps(&self) {
        let wallets_guard = self.mutex.lock();
        let mut reps_guard = self.wallet_reps.lock();
        reps_guard.clear();
        let half_principal_weight = self.online_reps.lock().minimum_principal_weight() / 2;
        for (_, wallet) in wallets_guard.iter() {
            let mut representatives = HashSet::new();
            let enc_keys = wallet.encrypted_keys.lock();
            for &pub_key in enc_keys.keys() {
                if reps_guard.check_rep(pub_key, half_principal_weight) {
                    representatives.insert(pub_key);
                }
            }
            *wallet.representatives.lock() = representatives;
        }
    }

    pub fn reload(&self) {
        let persisted = self.ledger.store.wallet.load_all();
        if persisted.is_empty() {
            return;
        }
        let mut changed = false;
        {
            let mut guard = self.mutex.lock();
            for (wallet_id, mut meta, keys) in persisted {
                if !guard.contains_key(&wallet_id) {
                    if meta.representative.is_zero() {
                        meta.representative = self.random_representative();
                    }
                    let wallet = Wallet::from_persisted(
                        self.ledger.clone(),
                        self.work_thresholds.clone(),
                        meta,
                        keys,
                    );
                    guard.insert(wallet_id, Arc::new(wallet));
                    changed = true;
                }
            }
        }
        if changed {
            self.compute_reps();
        }
    }

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
        let wallet = Arc::new(
            Wallet::new(self.ledger.clone(), self.work_thresholds.clone(), rep).unwrap(),
        );
        self.ledger
            .store
            .wallet
            .put_wallet_meta(&id, &wallet.to_persisted_meta());
        guard.insert(id, wallet);
    }

    pub fn change_seed(
        &self,
        id: WalletId,
        seed: &RawKey,
        count: u32,
    ) -> Result<(u32, Account), WalletsError> {
        let (restored, last_account) = {
            let guard = self.mutex.lock();
            let wallet = Self::get_wallet(&guard, &id)?;
            if !wallet.valid_password() {
                return Err(WalletsError::WalletLocked);
            }
            *wallet.seed.lock() = *seed;
            wallet.deterministic_index.store(0, Ordering::SeqCst);
            wallet.refresh_meta_encryption();
            let mut last_account = Account::zero();
            let mut restored = 0;
            let to_generate = if count == 0 {
                1 + wallet.deterministic_check(0)
            } else {
                count
            };
            for _ in 0..to_generate {
                let pk = wallet.insert_deterministic();
                if let Some(rec) = wallet.to_persisted_key(&pk) {
                    self.ledger.store.wallet.put_wallet_key(&id, &pk, &rec);
                }
                last_account = pk.into();
                restored += 1;
            }
            self.ledger
                .store
                .wallet
                .put_wallet_meta(&id, &wallet.to_persisted_meta());
            (restored, last_account)
        };
        self.compute_reps();
        Ok((restored, last_account))
    }

    pub fn get_accounts_of_wallet(
        &self,
        wallet_id: &WalletId,
    ) -> Result<Vec<Account>, WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        let mut accounts: Vec<Account> = wallet
            .encrypted_keys
            .lock()
            .keys()
            .map(|k| (*k).into())
            .collect();
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
        {
            let guard = self.mutex.lock();
            let wallet = Self::get_wallet(&guard, wallet_id)?;
            if !wallet.valid_password() {
                return Err(WalletsError::WalletLocked);
            }
            let removed_key = wallet.keys.lock().remove(pub_key).is_some();
            wallet.encrypted_keys.lock().remove(pub_key);
            let removed_watch = wallet.watch_keys.lock().remove(pub_key);
            wallet.deterministic_keys.lock().remove(pub_key);
            wallet.work_cache.lock().remove(pub_key);
            wallet.representatives.lock().remove(pub_key);
            if !removed_key && !removed_watch {
                return Err(WalletsError::AccountNotFound);
            }
            self.ledger.store.wallet.del_wallet_key(wallet_id, pub_key);
        }
        self.compute_reps();
        Ok(())
    }

    pub fn move_accounts(
        &self,
        source_id: &WalletId,
        target_id: &WalletId,
        accounts: &[PublicKey],
    ) -> Result<(), WalletsError> {
        {
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
                    source.encrypted_keys.lock().remove(pk);
                    source.deterministic_keys.lock().remove(pk);
                    source.representatives.lock().remove(pk);
                    self.ledger.store.wallet.del_wallet_key(source_id, pk);
                    target.insert_key(prv);
                    if let Some(rec) = target.to_persisted_key(pk) {
                        self.ledger.store.wallet.put_wallet_key(target_id, pk, &rec);
                    }
                }
            }
        }
        self.compute_reps();
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
        {
            let mut watch = wallet.watch_keys.lock();
            for account in accounts {
                if account.is_zero() {
                    return Err(WalletsError::BadPublicKey);
                }
                watch.insert((*account).into());
            }
        }
        for account in accounts {
            let pk: PublicKey = (*account).into();
            if let Some(rec) = wallet.to_persisted_key(&pk) {
                self.ledger.store.wallet.put_wallet_key(wallet_id, &pk, &rec);
            }
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
        wallet.lock_in_ram();
        Ok(())
    }

    pub fn rekey(
        &self,
        wallet_id: &WalletId,
        password: impl AsRef<str>,
    ) -> Result<(), WalletsError> {
        let guard = self.mutex.lock();
        let wallet = Self::get_wallet(&guard, wallet_id)?;
        if !wallet.rekey(password.as_ref()) {
            return Err(WalletsError::WalletLocked);
        }
        self.ledger
            .store
            .wallet
            .put_wallet_meta(wallet_id, &wallet.to_persisted_meta());
        let pks: Vec<PublicKey> = wallet.encrypted_keys.lock().keys().copied().collect();
        for pk in pks {
            if let Some(rec) = wallet.to_persisted_key(&pk) {
                self.ledger.store.wallet.put_wallet_key(wallet_id, &pk, &rec);
            }
        }
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
        if let Some(rec) = wallet.to_persisted_key(pub_key) {
            self.ledger
                .store
                .wallet
                .put_wallet_key(wallet_id, pub_key, &rec);
        }
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
        let pub_key = wallet.insert_key(priv_key);
        if let Some(rec) = wallet.to_persisted_key(&pub_key) {
            self.ledger
                .store
                .wallet
                .put_wallet_key(wallet_id, &pub_key, &rec);
        }
        let half_principal_weight = self.online_reps.lock().minimum_principal_weight() / 2;
        let mut rep_guard = self.wallet_reps.lock();
        if rep_guard.check_rep(pub_key, half_principal_weight) {
            wallet.representatives.lock().insert(pub_key);
        }
        Ok(pub_key)
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
        let pub_key = wallet.insert_deterministic_at(index);
        if let Some(rec) = wallet.to_persisted_key(&pub_key) {
            self.ledger
                .store
                .wallet
                .put_wallet_key(wallet_id, &pub_key, &rec);
        }
        self.ledger
            .store
            .wallet
            .put_wallet_meta(wallet_id, &wallet.to_persisted_meta());
        let half_principal_weight = self.online_reps.lock().minimum_principal_weight() / 2;
        let mut rep_guard = self.wallet_reps.lock();
        if rep_guard.check_rep(pub_key, half_principal_weight) {
            wallet.representatives.lock().insert(pub_key);
        }
        Ok(pub_key)
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
        let pub_key = wallet.insert_deterministic();
        if let Some(rec) = wallet.to_persisted_key(&pub_key) {
            self.ledger
                .store
                .wallet
                .put_wallet_key(wallet_id, &pub_key, &rec);
        }
        self.ledger
            .store
            .wallet
            .put_wallet_meta(wallet_id, &wallet.to_persisted_meta());
        let half_principal_weight = self.online_reps.lock().minimum_principal_weight() / 2;
        let mut rep_guard = self.wallet_reps.lock();
        if rep_guard.check_rep(pub_key, half_principal_weight) {
            info!(
                account = %pub_key.as_account().encode_account(),
                "New account qualified as representative"
            );
            wallet.representatives.lock().insert(pub_key);
        }
        Ok(pub_key)
    }

    fn enter_password(&self, wallet_id: WalletId, password: &str) -> Result<(), WalletsError> {
        let unlocked = {
            let guard = self.mutex.lock();
            let wallet = Self::get_wallet(&guard, &wallet_id)?;
            wallet.try_unlock(password)
        };
        self.compute_reps();
        if unlocked {
            Ok(())
        } else {
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
        wallet.refresh_meta_encryption();
        self.ledger
            .store
            .wallet
            .put_wallet_meta(&wallet_id, &wallet.to_persisted_meta());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_common::storage::{bpm::BufferPool, btree::BTree, StorageContext, SHARDS, SHARD_COUNT};

    #[test]
    fn compute_reps_and_foreach_representative() {
        let wallets = Wallets::new_null();
        let wallet_id = WalletId::from(1);
        wallets.create(wallet_id);

        let rep_key = PrivateKey::from_bytes(&[11u8; 32]);
        let non_rep_key = PrivateKey::from_bytes(&[22u8; 32]);

        // Give rep_key voting weight above vote_minimum
        wallets
            .ledger
            .rep_weights_updater
            .representation_put(rep_key.public_key(), Amount::nano(2_000_000));

        let rep_pub = wallets
            .insert_adhoc2(&wallet_id, rep_key.raw_key().as_bytes(), false)
            .unwrap();
        let non_rep_pub = wallets
            .insert_adhoc2(&wallet_id, non_rep_key.raw_key().as_bytes(), false)
            .unwrap();

        assert_eq!(wallets.voting_reps_count(), 1);
        assert!(wallets.voting_enabled());
        assert!(wallets.wallet_reps.lock().exists(&rep_pub.into()));
        assert!(!wallets.wallet_reps.lock().exists(&non_rep_pub.into()));

        let mut active_reps = Vec::new();
        wallets.foreach_representative(|k| active_reps.push(k.public_key()));
        assert_eq!(active_reps, alloc::vec![rep_pub]);

        // Later, non_rep_key receives delegated weight and compute_reps() runs
        wallets
            .ledger
            .rep_weights_updater
            .representation_put(non_rep_pub, Amount::nano(2_000_000));
        wallets.compute_reps();

        assert_eq!(wallets.voting_reps_count(), 2);
        let mut active_reps2 = Vec::new();
        wallets.foreach_representative(|k| active_reps2.push(k.public_key()));
        assert_eq!(active_reps2.len(), 2);
        assert!(active_reps2.contains(&rep_pub));
        assert!(active_reps2.contains(&non_rep_pub));

        // Removing a representative key updates voting_reps_count
        wallets.remove_key(&wallet_id, &non_rep_pub).unwrap();
        assert_eq!(wallets.voting_reps_count(), 1);
    }

    #[test]
    fn btree_wallet_persistence_across_reboot() {
        use kernel_common::storage::{init_wallet_storage, WALLET_SHARD};

        let storage = Arc::new(Mutex::new(StorageContext::new_ramdisk(8)));
        for i in 0..SHARD_COUNT {
            let bpm = BufferPool::new(storage.clone(), 64, 1 + i * 64);
            *SHARDS[i].lock() = Some(BTree::new(bpm));
        }
        init_wallet_storage(storage.clone());
        kernel_common::storage::bpm::NEED_CHECKPOINT
            .store(false, core::sync::atomic::Ordering::SeqCst);

        let wallet_id = WalletId::from(0xABCD);
        let unattended_wallet_id = WalletId::from(0xBEEF);
        let rep_key = PrivateKey::from_bytes(&[0x33; 32]);
        let rep_pub = rep_key.public_key();
        let unattended_rep_key = PrivateKey::from_bytes(&[0x44; 32]);
        let unattended_rep_pub = unattended_rep_key.public_key();
        let custom_rep = PublicKey::from(777);
        let watch_acc = Account::from(888);
        let det_pub;

        kernel_common::storage::wallet_crypto::reset_hardware_wallet_key();

        {
            let wallets = Wallets::new_null();
            wallets
                .ledger
                .rep_weights_updater
                .representation_put(rep_pub, Amount::nano(5_000_000));
            wallets
                .ledger
                .rep_weights_updater
                .representation_put(unattended_rep_pub, Amount::nano(5_000_000));

            wallets.create(wallet_id);
            wallets
                .set_representative(wallet_id, custom_rep, false)
                .unwrap();

            let inserted = wallets
                .insert_adhoc2(&wallet_id, rep_key.raw_key().as_bytes(), false)
                .unwrap();
            assert_eq!(inserted, rep_pub);

            det_pub = wallets.deterministic_insert2(&wallet_id, false).unwrap();
            wallets.insert_watch(&wallet_id, &[watch_acc]).unwrap();
            wallets
                .work_set(&wallet_id, &rep_pub, WorkNonce::from(0xCAFE_BABE))
                .unwrap();
            wallets.rekey(&wallet_id, "my_rep_pass").unwrap();
            wallets.lock(&wallet_id).unwrap();

            // Mode A wallet: default empty password "" (auto-unlocks on reboot via Ky X1 eFuse key)
            wallets.create(unattended_wallet_id);
            wallets
                .insert_adhoc2(
                    &unattended_wallet_id,
                    unattended_rep_key.raw_key().as_bytes(),
                    false,
                )
                .unwrap();
        }

        // Verify that plaintext private keys are NEVER stored on NVMe
        let raw_persisted = rsnano_ledger::BtreeWalletStore::new().load_all();
        assert_eq!(raw_persisted.len(), 2);
        for (_, meta, keys) in &raw_persisted {
            assert_ne!(&meta.encrypted_seed, &[0u8; 32]);
            for (pk, krec) in keys {
                if *pk == rep_pub {
                    assert_ne!(&krec.encrypted_priv_key, rep_key.raw_key().as_bytes());
                }
                if *pk == unattended_rep_pub {
                    assert_ne!(
                        &krec.encrypted_priv_key,
                        unattended_rep_key.raw_key().as_bytes()
                    );
                }
            }
        }

        // Wallet writes must never set NEED_CHECKPOINT on the ledger WAL
        assert!(
            !kernel_common::storage::bpm::NEED_CHECKPOINT
                .load(core::sync::atomic::Ordering::SeqCst)
        );

        // Wiping all 16 ledger SHARDS must NOT affect wallet storage
        for i in 0..SHARD_COUNT {
            if let Some(db) = SHARDS[i].lock().as_mut() {
                db.wipe();
            }
        }

        // Simulate stealing the NVMe SSD and plugging it into a DIFFERENT board (different SoC eFuse key):
        // Even the unattended (empty-password) wallet must fail to unlock!
        *WALLET_SHARD.lock() = None;
        init_wallet_storage(storage.clone());
        kernel_common::storage::wallet_crypto::set_hardware_wallet_key([0xEE; 32]);
        {
            let mut foreign_board_wallets = Wallets::new_null();
            foreign_board_wallets
                .ledger
                .rep_weights_updater
                .representation_put(unattended_rep_pub, Amount::nano(5_000_000));
            foreign_board_wallets.initialize().unwrap();

            assert!(
                !foreign_board_wallets
                    .valid_password(&unattended_wallet_id)
                    .unwrap()
            );
            assert!(matches!(
                foreign_board_wallets.fetch(&unattended_wallet_id, &unattended_rep_pub.into()),
                Err(WalletsError::WalletLocked)
            ));
            let mut foreign_voted = Vec::new();
            foreign_board_wallets.foreach_representative(|k| foreign_voted.push(k.public_key()));
            assert!(foreign_voted.is_empty());
        }

        // Restore original board's Ky X1 SoC eFuse key and simulate normal cold hardware reboot
        kernel_common::storage::wallet_crypto::reset_hardware_wallet_key();
        *WALLET_SHARD.lock() = None;
        init_wallet_storage(storage.clone());

        {
            let mut wallets_after_boot = Wallets::new_null();
            wallets_after_boot
                .ledger
                .rep_weights_updater
                .representation_put(rep_pub, Amount::nano(5_000_000));
            wallets_after_boot
                .ledger
                .rep_weights_updater
                .representation_put(unattended_rep_pub, Amount::nano(5_000_000));
            wallets_after_boot.initialize().unwrap();

            // Mode A wallet (empty password) auto-unlocked via Ky X1 eFuse key on reboot!
            assert!(wallets_after_boot
                .valid_password(&unattended_wallet_id)
                .unwrap());
            assert_eq!(
                wallets_after_boot
                    .fetch(&unattended_wallet_id, &unattended_rep_pub.into())
                    .unwrap()
                    .raw_key(),
                unattended_rep_key.raw_key()
            );

            assert!(wallets_after_boot.wallet_exists(&wallet_id));
            assert_eq!(
                wallets_after_boot.get_representative(wallet_id).unwrap(),
                custom_rep
            );
            assert_eq!(
                wallets_after_boot.deterministic_index_get(&wallet_id).unwrap(),
                1
            );
            assert_eq!(
                wallets_after_boot.key_type(wallet_id, &rep_pub),
                KeyType::Adhoc
            );
            assert_eq!(
                wallets_after_boot.key_type(wallet_id, &det_pub),
                KeyType::Deterministic
            );
            assert!(wallets_after_boot.exists(&watch_acc.into()));
            assert_eq!(
                wallets_after_boot.work_get2(&wallet_id, &rep_pub).unwrap(),
                WorkNonce::from(0xCAFE_BABE)
            );

            // Both representative accounts were detected on boot via initialize() -> compute_reps()
            assert_eq!(wallets_after_boot.voting_reps_count(), 2);
            assert!(wallets_after_boot.voting_enabled());

            // Mode B wallet (password-protected) stays locked until wallet_enter; only Mode A votes initially
            assert!(!wallets_after_boot.valid_password(&wallet_id).unwrap());
            let mut initial_voted = Vec::new();
            wallets_after_boot.foreach_representative(|k| initial_voted.push(k.public_key()));
            assert_eq!(initial_voted, alloc::vec![unattended_rep_pub]);

            assert_eq!(
                wallets_after_boot.enter_password(wallet_id, "wrong"),
                Err(WalletsError::InvalidPassword)
            );
            wallets_after_boot
                .enter_password(wallet_id, "my_rep_pass")
                .unwrap();
            assert!(wallets_after_boot.valid_password(&wallet_id).unwrap());

            let fetched = wallets_after_boot
                .fetch(&wallet_id, &rep_pub.into())
                .unwrap();
            assert_eq!(fetched.raw_key(), rep_key.raw_key());

            let mut voted_keys = Vec::new();
            wallets_after_boot.foreach_representative(|k| voted_keys.push(k.public_key()));
            assert_eq!(voted_keys.len(), 2);
            assert!(voted_keys.contains(&rep_pub));
            assert!(voted_keys.contains(&unattended_rep_pub));

            // Destroy removes from B-Tree
            wallets_after_boot.destroy(&wallet_id);
            wallets_after_boot.destroy(&unattended_wallet_id);
        }

        // Cold reboot after destroy
        *WALLET_SHARD.lock() = None;
        init_wallet_storage(storage.clone());

        {
            let mut wallets_after_destroy = Wallets::new_null();
            wallets_after_destroy.initialize().unwrap();
            assert!(!wallets_after_destroy.wallet_exists(&wallet_id));
        }

        *WALLET_SHARD.lock() = None;
        for i in 0..SHARD_COUNT {
            *SHARDS[i].lock() = None;
        }
    }
}
