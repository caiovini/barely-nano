use alloc::sync::Arc;
use rsnano_nullable_clock::Timestamp as Instant;
use spin::{Mutex, RwLock};

use tracing::{debug, info};

use rsnano_core::utils::{CancellationToken, Tickable};
use rsnano_ledger::Ledger;
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

        let channels = self.network.try_read().map(|n| n.channels_info()).unwrap_or_default();
        info!(
            "Peers: {} (realtime: {} | inbound: {} | outbound: {}) | NetQueue: {}",
            channels.total,
            channels.realtime,
            channels.inbound,
            channels.outbound,
            channels.queued_messages
        );

        if let Some(st) = self.bootstrapper.get_state().try_lock() {
            let active = st.candidate_accounts.priority_len() > 0
                || st.candidate_accounts.blocked_len() > 0
                || st.running_queries.len() > 0
                || !st.frontier_scan.is_done();
            debug!("Bootstrap active: {}", active);
        }

        {
            let (delta, online, peered) = {
                let online_reps = self.online_reps.try_lock();
                if let Some(reps) = online_reps {
                    (
                        reps.quorum_delta(),
                        reps.online_weight(),
                        reps.peered_weight(),
                    )
                } else {
                    Default::default()
                }
            };
            info!(
                "Quorum: {} (online stake: {} | stake peered: {})",
                delta.format_balance(0),
                online.format_balance(0),
                peered.format_balance(0)
            );
        }

        let elections = self.active_elections.try_read().map(|e| e.info()).unwrap_or_default();
        info!(
            "Elections active: {} (priority: {} | hinted: {} | optimistic: {})",
            elections.total, elections.priority, elections.hinted, elections.optimistic
        );

        let _missing = confirmed_count.saturating_sub(block_count + pruned_count);
        let unchecked_count = self.unchecked.len();
        let (queries_count, prio_len, blocked_len, blocking_known) =
            if let Some(st) = self.bootstrapper.get_state().try_lock() {
                (
                    st.running_queries.len(),
                    st.candidate_accounts.priority_len(),
                    st.candidate_accounts.blocked_len(),
                    st.candidate_accounts.known_dependencies(),
                )
            } else {
                (0, 0, 0, 0)
            };
        let bp_queue = self.block_processor.total_queue_len();
        let _bp_progress = self.block_processor.total_processed();
        let _peer_count = channels.total;

        let blocking_unknown = blocked_len.saturating_sub(blocking_known);

        let blocks_checked_rate = self.block_rates.bps();
        let blocks_confirmed_rate = self.block_rates.cps();

        let account_count = self.ledger.account_count();

        info!(
            "Blocks: {} (conf: {} | unchecked: {} | accs: {}) | rate: {} bps | {} cps | Boot: [q: {}, prio: {}, blk: {} (k:{}/u:{}), bpq: {}]",
            block_count,
            confirmed_count,
            unchecked_count,
            account_count,
            blocks_checked_rate,
            blocks_confirmed_rate,
            queries_count,
            prio_len,
            blocked_len,
            blocking_known,
            blocking_unknown,
            bp_queue,
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
