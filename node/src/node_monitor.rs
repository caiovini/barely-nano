use alloc::sync::Arc;
use rsnano_nullable_clock::Timestamp as Instant;
use spin::{Mutex, RwLock};

use tracing::info;

use rsnano_core::{
    Account,
    utils::{CancellationToken, Tickable},
};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_network::Network;

use crate::{
    block_rate_calculator::CurrentBlockRates, bootstrap::Bootstrapper,
    consensus::ActiveElectionsContainer, representatives::OnlineReps,
};

/// Periodically prints info about BPS, CPS, elections, peers,...
pub struct NodeMonitor {
    ledger: Arc<Ledger>,
    network: Arc<RwLock<Network>>,
    online_reps: Arc<Mutex<OnlineReps>>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    block_rates: Arc<CurrentBlockRates>,
    bootstrapper: Arc<Bootstrapper>,
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
    ) -> Self {
        Self {
            ledger,
            network,
            online_reps,
            active_elections,
            block_rates,
            bootstrapper,
            last_time: None,
        }
    }

    fn log(&self) {
        let confirmed_count = self.ledger.confirmed_count();
        let block_count = self.ledger.block_count();
        let pruned_count = self.ledger.pruned_count();

        if block_count < 10 {
            tracing::debug!("Dumping Ledger Blocks:");
            let any = self.ledger.any();
            // Since we don't have a good block iterator yet, we can try to find them via accounts
            let accounts = any.accounts_range(Account::zero()..);
            for (acc, info) in accounts {
                tracing::debug!(
                    "  Account {}: head={}, count={}",
                    acc.encode_account(),
                    info.head,
                    info.block_count
                );
                let mut current = info.head;
                while !current.is_zero() {
                    if let Some(block) = any.get_block(&current) {
                        let height = block.height();
                        tracing::debug!("    Block {}: {}", height, current);
                        current = block.previous();
                    } else {
                        tracing::debug!("    Block NOT FOUND: {}", current);
                        break;
                    }
                }
            }
        }

        let channels = self.network.read().channels_info();
        info!(
            "Peers: {} (realtime: {} | inbound: {} | outbound: {})",
            channels.total, channels.realtime, channels.inbound, channels.outbound
        );

        info!("Bootstrap active: {}", self.bootstrapper.is_active());

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

        // TODO: Maybe emphasize somehow that confirmed doesn't need to be equal to total; backlog is OK
        info!(
            "Network Height: {} | Blocks Stored: {} | Pruned: {} (missing: {})",
            confirmed_count,
            block_count,
            pruned_count,
            confirmed_count.saturating_sub(block_count + pruned_count)
        );

        let blocks_checked_rate = self.block_rates.bps();
        let blocks_confirmed_rate = self.block_rates.cps();

        info!(
            "Blocks rate: {} bps | {} cps",
            blocks_checked_rate, blocks_confirmed_rate,
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
