use alloc::sync::Arc;
use rsnano_nullable_clock::Timestamp as Instant;
use spin::{Mutex, RwLock};

use tracing::{debug, info};

use rsnano_core::{
    Account,
    utils::{CancellationToken, Tickable},
};
use rsnano_ledger::{ConfirmedSet, Ledger};
use rsnano_network::Network;

use crate::{
    block_processing::{BlockProcessor, UncheckedMap},
    block_rate_calculator::CurrentBlockRates,
    bootstrap::Bootstrapper,
    consensus::ActiveElectionsContainer,
    representatives::OnlineReps,
};

/// Periodically prints info about BPS, CPS, elections, peers,...
pub struct NodeMonitor {
    ledger: Arc<Ledger>,
    network: Arc<RwLock<Network>>,
    online_reps: Arc<Mutex<OnlineReps>>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    block_rates: Arc<CurrentBlockRates>,
    bootstrapper: Arc<Bootstrapper>,
    unchecked: Arc<UncheckedMap>,
    block_processor: Arc<BlockProcessor>,
    last_time: Option<Instant>,
}

impl NodeMonitor {
    pub fn new(
        ledger: Arc<Ledger>,
        network: Arc<RwLock<Network>>,
        online_reps: Arc<Mutex<OnlineReps>>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        block_rates: Arc<CurrentBlockRates>,
        bootstrapper: Arc<Bootstrapper>,
        unchecked: Arc<UncheckedMap>,
        block_processor: Arc<BlockProcessor>,
    ) -> Self {
        Self {
            ledger,
            network,
            online_reps,
            active_elections,
            block_rates,
            bootstrapper,
            unchecked,
            block_processor,
            last_time: None,
        }
    }

    fn log(&self) {
        let confirmed_count = self.ledger.confirmed_count();
        let block_count = self.ledger.block_count();
        let pruned_count = self.ledger.pruned_count();

        if block_count < 500 {
            debug!("Dumping Ledger Blocks:");
            let any = self.ledger.any();
            let accounts = any.accounts_range(Account::zero()..);
            for (acc, info) in accounts {
                debug!(
                    "  Account {}: head={}, count={}, conf={}",
                    acc.encode_account(),
                    info.head,
                    info.block_count,
                    self.ledger
                        .confirmed()
                        .get_conf_info(&acc)
                        .map(|i| i.frontier)
                        .unwrap_or_default()
                );
            }
        }

        {
            let state = self.bootstrapper.get_state();
            let guard = state.lock();
            let blocked_len = guard.candidate_accounts.blocked_len();
            if blocked_len > 0 {
                debug!("--- BLOCKED ACCOUNTS SAMPLE (Total: {}) ---", blocked_len);
                let mut count = 0;
                for entry in guard.candidate_accounts.iter_blocked() {
                    if count >= 10 {
                        break;
                    }
                    debug!(
                        "  #{} Acc: {} -> Dep: {} (Known Acc: {})",
                        count,
                        entry.account.encode_account(),
                        entry.dependency,
                        if entry.dependency_account.is_zero() {
                            "none".into()
                        } else {
                            entry.dependency_account.encode_account()
                        }
                    );
                    count += 1;
                }
                debug!("--------------------------------------------");
            }
        }

        let channels = self.network.read().channels_info();
        info!(
            "Peers: {} (realtime: {} | inbound: {} | outbound: {}) | NetQueue: {}",
            channels.total,
            channels.realtime,
            channels.inbound,
            channels.outbound,
            channels.queued_messages
        );

        debug!("Bootstrap active: {}", self.bootstrapper.is_active());

        {
            let (delta, online, peered) = {
                let online_reps = self.online_reps.lock();
                (
                    online_reps.quorum_delta(),
                    online_reps.online_weight(),
                    online_reps.peered_weight(),
                )
            };
            info!(
                "Quorum: {} (online stake: {} | stake peered: {})",
                delta.format_balance(0),
                online.format_balance(0),
                peered.format_balance(0)
            );
        }

        let elections = self.active_elections.read().info();
        info!(
            "Elections active: {} (priority: {} | hinted: {} | optimistic: {})",
            elections.total, elections.priority, elections.hinted, elections.optimistic
        );

        let _missing = confirmed_count.saturating_sub(block_count + pruned_count);
        let unchecked_count = self.unchecked.len();
        let _queries_count = self.bootstrapper.state().running_queries.len();
        let _prio_len = self.bootstrapper.state().candidate_accounts.priority_len();
        let blocked_len = self.bootstrapper.state().candidate_accounts.blocked_len();
        let _bp_queue = self.block_processor.total_queue_len();
        let _bp_progress = self.block_processor.total_processed();
        let _peer_count = self.network.read().channels_info().total;

        let blocking_known = self
            .bootstrapper
            .state()
            .candidate_accounts
            .known_dependencies();
        let _blocking_unknown = blocked_len.saturating_sub(blocking_known);

        let blocks_checked_rate = self.block_rates.bps();
        let blocks_confirmed_rate = self.block_rates.cps();

        let account_count = self.ledger.account_count();
        info!(
            "Blocks: {} (conf: {} | unchecked: {} | accs: {}) | rate: {} bps | {} cps",
            block_count,
            confirmed_count,
            unchecked_count,
            account_count,
            blocks_checked_rate,
            blocks_confirmed_rate,
        );
    }
}

impl Tickable for NodeMonitor {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        if self.last_time.is_some() {
            self.log();
        } else {
            // Wait for node to warm up before logging
        }
        self.last_time = Some(Instant::now());
    }
}
