use crate::{AnySet, Ledger, LedgerSet};
use rsnano_core::{
    Account, AccountInfo, Amount, Block, BlockHash, ChangeBlockArgs, Epoch, Link, OpenBlockArgs,
    PendingInfo, PendingKey, PrivateKey, PublicKey, ReceiveBlockArgs, SavedBlock, SendBlockArgs,
    StateBlockArgs, WorkNonce, DEV_GENESIS_KEY,
};

/// Provides a simplified interface for inserting blocks into the ledger (for tests)
pub struct LedgerInserter<'a> {
    ledger: &'a Ledger,
}

impl<'a> LedgerInserter<'a> {
    pub fn new(ledger: &'a Ledger) -> Self {
        Self { ledger }
    }

    pub fn genesis(&self) -> LedgerBlockInserter<'_> {
        LedgerBlockInserter {
            ledger: self.ledger,
            key: &DEV_GENESIS_KEY,
        }
    }

    pub fn account(&self, key: &'a PrivateKey) -> LedgerBlockInserter<'a> {
        LedgerBlockInserter {
            ledger: self.ledger,
            key,
        }
    }
}

pub struct LedgerBlockInserter<'a> {
    ledger: &'a Ledger,
    key: &'a PrivateKey,
}

impl<'a> LedgerBlockInserter<'a> {
    pub fn send(
        &mut self,
        destination: impl Into<Account>,
        amount: impl Into<Amount>,
    ) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = StateBlockArgs {
            key: self.key,
            previous: info.head,
            representative: info.representative,
            balance: info.balance - amount.into(),
            link: destination.into().into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn send_and_change(
        &mut self,
        destination: impl Into<Account>,
        amount: impl Into<Amount>,
        representative: impl Into<PublicKey>,
    ) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = StateBlockArgs {
            key: self.key,
            previous: info.head,
            representative: representative.into(),
            balance: info.balance - amount.into(),
            link: destination.into().into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn legacy_send(
        &mut self,
        destination: impl Into<Account>,
        amount: impl Into<Amount>,
    ) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = SendBlockArgs {
            key: self.key,
            previous: info.head,
            destination: destination.into(),
            balance: info.balance - amount.into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn receive(&mut self, corresponding_send: BlockHash) -> SavedBlock {
        let info = self.get_account_info();
        let pending = self.get_pending(corresponding_send);

        let representative = if info.block_count == 0 {
            self.key.public_key()
        } else {
            info.representative
        };

        let block: Block = StateBlockArgs {
            key: self.key,
            previous: info.head,
            representative,
            balance: info.balance + pending.amount,
            link: corresponding_send.into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn receive_and_change(
        &mut self,
        corresponding_send: BlockHash,
        representative: impl Into<PublicKey>,
    ) -> SavedBlock {
        let info = self.get_account_info();
        let pending = self.get_pending(corresponding_send);

        let block: Block = StateBlockArgs {
            key: self.key,
            previous: info.head,
            representative: representative.into(),
            balance: info.balance + pending.amount,
            link: corresponding_send.into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    fn get_pending(&self, corresponding_send: BlockHash) -> PendingInfo {
        self.ledger
            .any()
            .get_pending(&PendingKey::new(self.key.account(), corresponding_send))
            .expect("no pending receive found")
    }

    pub fn legacy_open(&mut self, corresponding_send: BlockHash) -> SavedBlock {
        let block: Block = OpenBlockArgs {
            key: self.key,
            source: corresponding_send,
            representative: self.key.public_key(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn legacy_receive(&mut self, corresponding_send: BlockHash) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = ReceiveBlockArgs {
            key: self.key,
            previous: info.head,
            source: corresponding_send,
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn change(&mut self, new_rep: impl Into<PublicKey>) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = StateBlockArgs {
            key: self.key,
            previous: info.head,
            representative: new_rep.into(),
            balance: info.balance,
            link: Link::zero(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn legacy_change(&mut self, new_rep: impl Into<PublicKey>) -> SavedBlock {
        let info = self.get_opened_account();

        let block: Block = ChangeBlockArgs {
            key: self.key,
            previous: info.head,
            representative: new_rep.into(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    pub fn epoch_v1(&mut self) -> SavedBlock {
        let info = self.get_account_info();

        let block: Block = StateBlockArgs {
            key: &DEV_GENESIS_KEY,
            previous: info.head,
            representative: info.representative,
            balance: info.balance,
            link: *self.ledger.constants.epochs.link(Epoch::Epoch1).unwrap(),
            work: WorkNonce::new(u64::MAX),
        }
        .into();

        self.ledger.process_one(&block).unwrap()
    }

    fn get_opened_account(&mut self) -> AccountInfo {
        let info = self.get_account_info();

        if info.block_count == 0 {
            panic!("Unopened account: {}", self.key.account().encode_account());
        }

        info
    }

    fn get_account_info(&mut self) -> AccountInfo {
        self.ledger
            .any()
            .get_account(&self.key.account())
            .unwrap_or_default()
    }
}
