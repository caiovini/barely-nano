mod account_create;
mod account_list;
mod account_move;
mod account_remove;
mod accounts_create;
mod password_change;
mod password_enter;
mod password_valid;
mod receive;
mod receive_minimum;
mod search_receivable;
mod search_receivable_all;
mod send;
mod wallet_add;
mod wallet_add_watch;
mod wallet_balances;
mod wallet_change_seed;
mod wallet_contains;
mod wallet_create;
mod wallet_destroy;
mod wallet_export;
mod wallet_frontiers;
mod wallet_history;
mod wallet_info;
mod wallet_ledger;
mod wallet_lock;
mod wallet_locked;
mod wallet_receivable;
mod wallet_representative;
mod wallet_representative_set;
mod wallet_republish;
mod wallet_work_get;
mod work_get;
mod work_set;

use crate::RpcCommandHandler;
use anyhow::bail;
use rsnano_core::{BlockDetails, WorkNonce};
use rsnano_node::wallets::WalletsExt;
use rsnano_rpc_messages::{AccountRepresentativeSetArgs, BlockDto};

impl RpcCommandHandler {
    pub(crate) fn account_representative_set(
        &self,
        args: AccountRepresentativeSetArgs,
    ) -> anyhow::Result<BlockDto> {
        let work: WorkNonce = args.work.unwrap_or_default();
        if work.is_zero() && !self.node.work_factory.work_generation_enabled() {
            bail!("Work generation is disabled");
        }
        let any = self.node.ledger.any();
        let info = self.load_account(&any, &args.account)?;
        if !work.is_zero() {
            let details = BlockDetails::new(info.epoch, false, false, false);
            if self
                .node
                .network_params
                .work
                .difficulty(&info.head.into(), work)
                < self.node.network_params.work.threshold(&details)
            {
                bail!("Invalid work");
            }
        }
        let generate_work = work.is_zero();
        let saved = self.node.wallets.change_sync(
            args.wallet,
            args.account,
            args.representative.into(),
            work,
            generate_work,
        )?;
        Ok(BlockDto::new(saved.hash()))
    }
}

#[cfg(test)]
mod tests {
    use crate::{test_rpc_command, test_rpc_command_requires_control, test_rpc_command_with_node};
    use alloc::{string::String, sync::Arc, vec};
    use rsnano_core::{Account, PrivateKey, RawKey, WalletId, WorkNonce};
    use rsnano_node::Node;
    use rsnano_rpc_messages::{
        AccountCreateArgs, AccountMoveArgs, AccountsCreateArgs, AccountsRpcMessage,
        AccountsWithWorkResponse, AmountRpcMessage, ChangedResponse, DestroyedResponse,
        ExistsResponse, JsonResponse, LockedResponse, MovedResponse, RemovedDto, RpcCommand,
        SetResponse, StartedResponse, SuccessResponse, ValidResponse, WalletAddArgs,
        WalletAddWatchArgs, WalletBalancesArgs, WalletChangeSeedArgs, WalletChangeSeedResponse,
        WalletCreateArgs, WalletCreateResponse, WalletInfoResponse, WalletRepresentativeResponse,
        WalletRepresentativeSetArgs, WalletRpcMessage, WalletWithAccountArgs,
        WalletWithPasswordArgs, WorkResponse, WorkSetArgs,
    };

    #[test]
    fn wallet_lifecycle_and_accounts() {
        let node = Arc::new(Node::new_null());

        let created: WalletCreateResponse = test_rpc_command_with_node(
            RpcCommand::WalletCreate(WalletCreateArgs { seed: None }),
            node.clone(),
        );
        let wallet_id = created.wallet;

        // Create deterministic account
        let acc1: rsnano_rpc_messages::AccountResponse = test_rpc_command_with_node(
            RpcCommand::AccountCreate(AccountCreateArgs {
                wallet: wallet_id,
                index: None,
                work: Some(false.into()),
            }),
            node.clone(),
        );

        // Create multiple deterministic accounts
        let created_many: AccountsRpcMessage = test_rpc_command_with_node(
            RpcCommand::AccountsCreate(AccountsCreateArgs {
                wallet: wallet_id,
                count: 2.into(),
                work: Some(false.into()),
            }),
            node.clone(),
        );
        assert_eq!(created_many.accounts.len(), 2);

        // Add adhoc key
        let adhoc_key = PrivateKey::from_bytes(&[7u8; 32]);
        let added: rsnano_rpc_messages::AccountResponse = test_rpc_command_with_node(
            RpcCommand::WalletAdd(WalletAddArgs {
                wallet: wallet_id,
                key: adhoc_key.raw_key(),
                work: Some(false.into()),
            }),
            node.clone(),
        );
        assert_eq!(added.account, Account::from(adhoc_key.public_key()));

        // Wallet contains & account list
        let contains: ExistsResponse = test_rpc_command_with_node(
            RpcCommand::WalletContains(WalletWithAccountArgs {
                wallet: wallet_id,
                account: acc1.account,
            }),
            node.clone(),
        );
        assert_eq!(contains.exists, true.into());

        let list: AccountsRpcMessage = test_rpc_command_with_node(
            RpcCommand::AccountList(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(list.accounts.len(), 4);

        // Wallet info
        let info: WalletInfoResponse = test_rpc_command_with_node(
            RpcCommand::WalletInfo(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(info.accounts_count, 4.into());
        assert_eq!(info.deterministic_count, 3.into());
        assert_eq!(info.adhoc_count, 1.into());

        // Representative get & set
        let new_rep = Account::from(99);
        let set_rep: SetResponse = test_rpc_command_with_node(
            RpcCommand::WalletRepresentativeSet(WalletRepresentativeSetArgs {
                wallet: wallet_id,
                representative: new_rep,
                update_existing_accounts: None,
            }),
            node.clone(),
        );
        assert_eq!(set_rep, SetResponse::new(true));

        let got_rep: WalletRepresentativeResponse = test_rpc_command_with_node(
            RpcCommand::WalletRepresentative(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(got_rep.representative, new_rep);

        // Work set & get
        let _: SuccessResponse = test_rpc_command_with_node(
            RpcCommand::WorkSet(WorkSetArgs {
                wallet: wallet_id,
                account: acc1.account,
                work: WorkNonce::from(0x1234),
            }),
            node.clone(),
        );
        let got_work: WorkResponse = test_rpc_command_with_node(
            RpcCommand::WorkGet(WalletWithAccountArgs {
                wallet: wallet_id,
                account: acc1.account,
            }),
            node.clone(),
        );
        assert_eq!(got_work.work, WorkNonce::from(0x1234));

        let all_works: AccountsWithWorkResponse = test_rpc_command_with_node(
            RpcCommand::WalletWorkGet(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(
            all_works.works.get(&acc1.account),
            Some(&WorkNonce::from(0x1234))
        );

        // Password change, lock, unlock
        let changed: ChangedResponse = test_rpc_command_with_node(
            RpcCommand::PasswordChange(WalletWithPasswordArgs {
                wallet: wallet_id,
                password: String::from("secret"),
            }),
            node.clone(),
        );
        assert_eq!(changed, ChangedResponse::new(true));

        let locked: LockedResponse = test_rpc_command_with_node(
            RpcCommand::WalletLock(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(locked.locked, true.into());

        let is_locked: LockedResponse = test_rpc_command_with_node(
            RpcCommand::WalletLocked(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(is_locked.locked, true.into());

        let bad_pw: ValidResponse = test_rpc_command_with_node(
            RpcCommand::PasswordEnter(WalletWithPasswordArgs {
                wallet: wallet_id,
                password: String::from("wrong"),
            }),
            node.clone(),
        );
        assert_eq!(bad_pw.valid, false.into());

        let good_pw: ValidResponse = test_rpc_command_with_node(
            RpcCommand::PasswordEnter(WalletWithPasswordArgs {
                wallet: wallet_id,
                password: String::from("secret"),
            }),
            node.clone(),
        );
        assert_eq!(good_pw.valid, true.into());

        let valid_pw: ValidResponse = test_rpc_command_with_node(
            RpcCommand::PasswordValid(WalletRpcMessage::new(wallet_id)),
            node.clone(),
        );
        assert_eq!(valid_pw.valid, true.into());

        // Move account to second wallet
        let created2: WalletCreateResponse = test_rpc_command_with_node(
            RpcCommand::WalletCreate(WalletCreateArgs { seed: None }),
            node.clone(),
        );
        let wallet2 = created2.wallet;

        let moved: MovedResponse = test_rpc_command_with_node(
            RpcCommand::AccountMove(AccountMoveArgs {
                wallet: wallet2,
                source: wallet_id,
                accounts: vec![added.account],
            }),
            node.clone(),
        );
        assert_eq!(moved.moved, true.into());

        // Remove account
        let removed: RemovedDto = test_rpc_command_with_node(
            RpcCommand::AccountRemove(WalletWithAccountArgs {
                wallet: wallet2,
                account: added.account,
            }),
            node.clone(),
        );
        assert_eq!(removed.removed, true.into());

        // Watch account, change seed, export, search receivable, destroy
        let _: SuccessResponse = test_rpc_command_with_node(
            RpcCommand::WalletAddWatch(WalletAddWatchArgs {
                wallet: wallet2,
                accounts: vec![Account::from(55)],
            }),
            node.clone(),
        );

        let seed_resp: WalletChangeSeedResponse = test_rpc_command_with_node(
            RpcCommand::WalletChangeSeed(WalletChangeSeedArgs {
                wallet: wallet2,
                seed: RawKey::from(42),
                count: Some(2.into()),
            }),
            node.clone(),
        );
        assert_eq!(seed_resp.restored_count, 2.into());

        let exported: JsonResponse = test_rpc_command_with_node(
            RpcCommand::WalletExport(WalletRpcMessage::new(wallet2)),
            node.clone(),
        );
        assert!(exported.json.contains(&RawKey::from(42).encode_hex()));

        let started: StartedResponse = test_rpc_command_with_node(
            RpcCommand::SearchReceivable(WalletRpcMessage::new(wallet2)),
            node.clone(),
        );
        assert_eq!(started.started, true.into());

        let _: SuccessResponse =
            test_rpc_command_with_node(RpcCommand::SearchReceivableAll, node.clone());

        let destroyed: DestroyedResponse = test_rpc_command_with_node(
            RpcCommand::WalletDestroy(WalletRpcMessage::new(wallet2)),
            node,
        );
        assert_eq!(destroyed.destroyed, true.into());
    }

    #[test]
    fn receive_minimum_and_control_checks() {
        let min: AmountRpcMessage = test_rpc_command(RpcCommand::ReceiveMinimum);
        assert!(!min.amount.is_zero());

        test_rpc_command_requires_control(RpcCommand::ReceiveMinimum);
        test_rpc_command_requires_control(RpcCommand::WalletDestroy(WalletRpcMessage::new(
            WalletId::from(1),
        )));
        test_rpc_command_requires_control(RpcCommand::WalletBalances(WalletBalancesArgs {
            wallet: WalletId::from(1),
            threshold: None,
        }));
    }
}
