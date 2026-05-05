use alloc::sync::Arc;
use spin::Mutex;
use hashbrown::HashMap;
use alloc::vec::Vec;
use alloc::boxed::Box;

use tracing::info;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, KeyDerivationFunction, Networks,
    PrivateKey, PublicKey, Root, SavedBlock, WalletId,
};
use rsnano_ledger::Ledger;
use rsnano_work::WorkThresholds;

use super::{Wallet, WalletRepresentatives};
use crate::{
    block_processing::BlockProcessorQueue,
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
        Vec::new()
    }

    pub fn foreach_representative<F>(&self, mut _action: F)
    where
        F: FnMut(&PrivateKey),
    {
    }

    pub fn exists(&self, _pub_key: &PublicKey) -> bool {
        false
    }

    pub fn compute_reps(&self) {}

    pub fn reload(&self) {}
    
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
    fn insert_adhoc2(&self, wallet_id: &WalletId, key: &[u8; 32], generate_work: bool) -> anyhow::Result<PublicKey>;
}

impl WalletsExt for Wallets {
    fn insert_adhoc2(&self, _wallet_id: &WalletId, _key: &[u8; 32], _generate_work: bool) -> anyhow::Result<PublicKey> {
        Ok(PublicKey::zero())
    }
}
