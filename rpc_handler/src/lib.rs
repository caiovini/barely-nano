#![no_std]

extern crate alloc;

use alloc::string::ToString;

mod ledger;
mod node;
mod utils;
// mod wallets;

use anyhow::anyhow;
use rsnano_core::{Account, AccountInfo, BlockHash, SavedBlock};
use rsnano_ledger::AnySet;
use rsnano_node::Node;
use rsnano_rpc_messages::{RpcCommand, RpcError, StatsType};
use serde_json::{to_value, Value};
use alloc::sync::Arc;
use tracing::debug;
use utils::*;

#[derive(Clone)]
pub struct RpcCommandHandler {
    node: Arc<Node>,
    enable_control: bool,
    // On bare-metal we don't have oneshot channels yet
    #[cfg(not(target_os = "none"))]
    stop: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl RpcCommandHandler {
    #[cfg(not(target_os = "none"))]
    pub fn new(node: Arc<Node>, enable_control: bool, tx_stop: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            node,
            enable_control,
            stop: Arc::new(Mutex::new(Some(tx_stop))),
        }
    }

    #[cfg(target_os = "none")]
    pub fn new(node: Arc<Node>, enable_control: bool) -> Self {
        Self {
            node,
            enable_control,
        }
    }

    pub fn handle(&self, command: RpcCommand) -> serde_json::Value {
        debug!(?command, "Handling RPC command");
        self.call_handler(command).unwrap_or_else(Self::error_value)
    }

    fn error_value(error: anyhow::Error) -> serde_json::Value {
        serde_json::to_value(RpcError::new(error.to_string())).unwrap()
    }

    fn call_handler(&self, command: RpcCommand) -> anyhow::Result<serde_json::Value> {
        self.check_control_enabled(&command)?;
        let response = match command {
            RpcCommand::AccountBalance(args) => to_value(self.account_balance(args)),
            RpcCommand::AccountBlockCount(args) => to_value(self.account_block_count(args)?),
            RpcCommand::AccountInfo(args) => to_value(self.account_info(args)?),
            RpcCommand::AccountHistory(args) => to_value(self.account_history(args)?),
            RpcCommand::AccountRepresentative(args) => to_value(self.account_representative(args)?),
            RpcCommand::AccountWeight(args) => to_value(self.account_weight(args)),
            RpcCommand::AccountsBalances(args) => to_value(self.accounts_balances(args)),
            RpcCommand::AccountsReceivable(args) => to_value(self.accounts_receivable(args)),
            RpcCommand::AccountsRepresentatives(args) => to_value(self.accounts_representatives(args)),
            RpcCommand::BlockCount => to_value(self.block_count()),
            RpcCommand::BlockHash(args) => to_value(block_hash(args)),
            RpcCommand::BlockInfo(args) => to_value(self.block_info(args)?),
            RpcCommand::Blocks(args) => to_value(self.blocks(args)?),
            RpcCommand::BlocksInfo(args) => to_value(self.blocks_info(args)?),
            RpcCommand::ConfirmationActive(args) => to_value(self.confirmation_active(args)),
            RpcCommand::ConfirmationQuorum(args) => to_value(self.confirmation_quorum(args)),
            RpcCommand::FrontierCount => to_value(self.frontier_count()),
            RpcCommand::Keepalive(_) => self.not_implemented(),
            RpcCommand::Peers(args) => to_value(self.peers(args)),
            RpcCommand::Representatives(args) => to_value(self.representatives(args)),
            RpcCommand::Telemetry(args) => to_value(self.telemetry(args)?),
            RpcCommand::Uptime => to_value(self.uptime()),
            RpcCommand::Version => to_value(self.version()),
            RpcCommand::ActiveDifficulty => to_value(self.active_difficulty()),
            RpcCommand::Stats(_) |
            RpcCommand::StatsClear => self.not_implemented(),
            _ => self.not_implemented(),
        }?;

        Ok(response)
    }

    fn check_control_enabled(&self, command: &RpcCommand) -> anyhow::Result<()> {
        if !self.enable_control && requires_control(command) {
            Err(anyhow!("RPC control is disabled"))
        } else {
            Ok(())
        }
    }

    fn load_block_any(&self, any: &dyn AnySet, hash: &BlockHash) -> anyhow::Result<SavedBlock> {
        any.get_block(hash)
            .ok_or_else(|| anyhow!(Self::BLOCK_NOT_FOUND))
    }

    fn load_account(&self, any: &dyn AnySet, account: &Account) -> anyhow::Result<AccountInfo> {
        any.get_account(account)
            .ok_or_else(|| anyhow!(Self::ACCOUNT_NOT_FOUND))
    }

    const BLOCK_NOT_FOUND: &str = "Block not found";
    const NOT_IMPLEMENTED: &str = "Not implemented yet";
    const ACCOUNT_NOT_FOUND: &str = "Account not found";

    fn not_implemented(&self) -> Result<Value, serde_json::Error> {
        Ok(Value::String(Self::NOT_IMPLEMENTED.to_string()))
    }
}

fn requires_control(command: &RpcCommand) -> bool {
    match command {
        RpcCommand::AccountCreate(_)
        | RpcCommand::AccountMove(_)
        | RpcCommand::AccountRemove(_)
        | RpcCommand::AccountRepresentativeSet(_)
        | RpcCommand::AccountsCreate(_)
        | RpcCommand::BlockCreate(_)
        | RpcCommand::BootstrapLazy(_)
        | RpcCommand::DatabaseTxnTracker(_)
        | RpcCommand::Keepalive(_)
        | RpcCommand::Ledger(_)
        | RpcCommand::NodeId
        | RpcCommand::PasswordChange(_)
        | RpcCommand::PopulateBacklog
        | RpcCommand::Receive(_)
        | RpcCommand::ReceiveMinimum
        | RpcCommand::ReceiveMinimumSet(_)
        | RpcCommand::SearchReceivable(_)
        | RpcCommand::SearchReceivableAll
        | RpcCommand::Send(_)
        | RpcCommand::Stop
        | RpcCommand::UncheckedClear
        | RpcCommand::Unopened(_)
        | RpcCommand::WalletAdd(_)
        | RpcCommand::WalletAddWatch(_)
        | RpcCommand::WalletChangeSeed(_)
        | RpcCommand::WalletCreate(_)
        | RpcCommand::WalletDestroy(_)
        | RpcCommand::WalletLock(_)
        | RpcCommand::WalletLedger(_)
        | RpcCommand::WalletRepresentativeSet(_)
        | RpcCommand::WalletReceivable(_)
        | RpcCommand::WalletRepublish(_)
        | RpcCommand::WalletWorkGet(_)
        | RpcCommand::WorkGenerate(_)
        | RpcCommand::WorkCancel(_)
        | RpcCommand::WorkGet(_)
        | RpcCommand::WorkSet(_)
        | RpcCommand::WorkPeerAdd(_)
        | RpcCommand::WorkPeers => true,
        RpcCommand::Stats(s) => matches!(s.stats_type, StatsType::Objects),
        RpcCommand::Process(args) => args.force == Some(true.into()),
        _ => false,
    }
}

fn legacy_bootstrap_disabled() -> anyhow::Result<serde_json::Value> {
    Err(anyhow!("Legacy bootstrap is disabled"))
}

#[cfg(test)]
use serde::de::DeserializeOwned;

#[cfg(test)]
pub fn test_rpc_command_requires_control(cmd: RpcCommand) {
    let node = Arc::new(Node::new_null());
    let (tx_stop, _rx_stop) = tokio::sync::oneshot::channel();
    let cmd_handler = RpcCommandHandler::new(node, false, tx_stop);
    let result = cmd_handler.handle(cmd);
    let error: RpcError = serde_json::from_value(result).unwrap();
    assert_eq!(error.error, "RPC control is disabled");
}

#[cfg(test)]
pub fn test_rpc_command<T>(cmd: RpcCommand) -> T
where
    T: DeserializeOwned,
{
    let node = Arc::new(Node::new_null());
    test_rpc_command_with_node(cmd, node)
}

#[cfg(test)]
pub fn test_rpc_command_with_node<T>(cmd: RpcCommand, node: Arc<Node>) -> T
where
    T: DeserializeOwned,
{
    let (tx_stop, _rx_stop) = tokio::sync::oneshot::channel();
    let cmd_handler = RpcCommandHandler::new(node, true, tx_stop);
    let result = cmd_handler.handle(cmd);
    serde_json::from_value(result).unwrap()
}
