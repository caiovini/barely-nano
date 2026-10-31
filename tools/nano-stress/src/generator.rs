use std::str::FromStr;

use rsnano_core::{
    Account, Amount, Block, BlockHash, Difficulty, DifficultyV1, Link, PrivateKey, PublicKey,
    Root, StateBlockArgs, WorkNonce, DEV_GENESIS_KEY,
};
use rsnano_ledger::DEV_GENESIS_BLOCK;
use rsnano_work::WorkThresholds;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Chained,
    Change,
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "chained" => Ok(Self::Chained),
            "change" => Ok(Self::Change),
            other => Err(format!(
                "Invalid strategy '{}' (expected 'chained' or 'change')",
                other
            )),
        }
    }
}

pub struct BlockBatch {
    pub blocks: Vec<Block>,
}

#[derive(Clone)]
pub struct AccountTrack {
    pub key: PrivateKey,
    pub frontier: BlockHash,
    pub balance: Amount,
    pub representative: PublicKey,
    pub is_open: bool,
}

pub struct BlockGenerator {
    current_frontier: BlockHash,
    current_balance: Amount,
    genesis_key: PrivateKey,
    dev_threshold: u64,
    accounts: Vec<AccountTrack>,
    round_robin_idx: usize,
    open_amount: Amount,
}

impl Default for BlockGenerator {
    fn default() -> Self {
        Self::new(WorkThresholds::publish_dev().threshold_base())
    }
}

impl BlockGenerator {
    pub fn new(dev_threshold: u64) -> Self {
        Self {
            current_frontier: DEV_GENESIS_BLOCK.hash(),
            current_balance: Amount::MAX,
            genesis_key: DEV_GENESIS_KEY.clone(),
            dev_threshold,
            accounts: Vec::new(),
            round_robin_idx: 0,
            open_amount: Amount::raw(1_000_000_000_000_000), // 10^15 raw
        }
    }

    pub fn with_zero_pow() -> Self {
        Self::new(0)
    }

    pub fn set_frontier(&mut self, frontier: BlockHash, balance: Amount) {
        self.current_frontier = frontier;
        self.current_balance = balance;
    }

    /// Configure generator to distribute stress traffic across multiple accounts.
    /// If num_accounts <= 1, traffic stays on the genesis account.
    pub fn init_accounts(&mut self, num_accounts: usize) {
        self.accounts.clear();
        self.round_robin_idx = 0;
        if num_accounts <= 1 {
            return;
        }

        let seed = rand::random::<u64>();
        for i in 0..num_accounts {
            let key = PrivateKey::from((seed ^ (i as u64)).wrapping_mul(0x9e3779b97f4a7c15) | 1);
            self.accounts.push(AccountTrack {
                representative: key.public_key(),
                key,
                frontier: BlockHash::zero(),
                balance: Amount::zero(),
                is_open: false,
            });
        }
    }

    #[inline]
    pub fn compute_work_internal(dev_threshold: u64, root: &Root) -> WorkNonce {
        if dev_threshold == 0 {
            return WorkNonce::new(0);
        }
        let diff = DifficultyV1::default();
        let mut nonce = rand::random::<u64>();
        loop {
            let work = WorkNonce::new(nonce);
            if diff.get_difficulty(root, work) >= dev_threshold {
                return work;
            }
            nonce = nonce.wrapping_add(1);
        }
    }

    /// Generate a batch of blocks according to the selected strategy and account distribution.
    pub fn generate_batch(&mut self, count: usize, strategy: Strategy) -> BlockBatch {
        let mut blocks = Vec::with_capacity(count);
        let dev_threshold = self.dev_threshold;

        if self.accounts.is_empty() {
            match strategy {
                Strategy::Chained => {
                    for i in 0..count {
                        let send_amount = Amount::raw(1000);
                        self.current_balance = self.current_balance - send_amount;

                        let dest_priv = PrivateKey::from((i as u64) + 1000);
                        let dest_account: Account = dest_priv.account();

                        let root = Root::from(self.current_frontier);
                        let work = Self::compute_work_internal(dev_threshold, &root);

                        let block: Block = StateBlockArgs {
                            key: &self.genesis_key,
                            previous: self.current_frontier,
                            representative: self.genesis_key.public_key(),
                            balance: self.current_balance,
                            link: dest_account.into(),
                            work,
                        }
                        .into();

                        self.current_frontier = block.hash();
                        blocks.push(block);
                    }
                }
                Strategy::Change => {
                    for i in 0..count {
                        let rep = PublicKey::from((i as u64) + 1);
                        let root = Root::from(self.current_frontier);
                        let work = Self::compute_work_internal(dev_threshold, &root);

                        let block: Block = StateBlockArgs {
                            key: &self.genesis_key,
                            previous: self.current_frontier,
                            representative: rep,
                            balance: self.current_balance,
                            link: Link::zero(),
                            work,
                        }
                        .into();

                        self.current_frontier = block.hash();
                        blocks.push(block);
                    }
                }
            }
        } else {
            let mut remaining = count;

            for acc in self.accounts.iter_mut() {
                if !acc.is_open && remaining >= 2 {
                    self.current_balance = self.current_balance - self.open_amount;
                    let root_gen = Root::from(self.current_frontier);
                    let work_gen = Self::compute_work_internal(dev_threshold, &root_gen);

                    let send_block: Block = StateBlockArgs {
                        key: &self.genesis_key,
                        previous: self.current_frontier,
                        representative: self.genesis_key.public_key(),
                        balance: self.current_balance,
                        link: acc.key.account().into(),
                        work: work_gen,
                    }
                    .into();

                    self.current_frontier = send_block.hash();
                    let send_hash = send_block.hash();
                    blocks.push(send_block);

                    let root_open = Root::from(acc.key.account());
                    let work_open = Self::compute_work_internal(dev_threshold, &root_open);

                    let open_block: Block = StateBlockArgs {
                        key: &acc.key,
                        previous: BlockHash::zero(),
                        representative: acc.representative,
                        balance: self.open_amount,
                        link: send_hash.into(),
                        work: work_open,
                    }
                    .into();

                    acc.frontier = open_block.hash();
                    acc.balance = self.open_amount;
                    acc.is_open = true;
                    blocks.push(open_block);

                    remaining -= 2;
                }
            }

            let num_accounts = self.accounts.len();
            for _ in 0..remaining {
                let acc_idx = self.round_robin_idx % num_accounts;
                self.round_robin_idx += 1;

                let acc = &mut self.accounts[acc_idx];
                if !acc.is_open {
                    let send_amount = Amount::raw(1000);
                    self.current_balance = self.current_balance - send_amount;
                    let dest = PrivateKey::from(rand::random::<u64>()).account();
                    let root = Root::from(self.current_frontier);
                    let work = Self::compute_work_internal(dev_threshold, &root);

                    let block: Block = StateBlockArgs {
                        key: &self.genesis_key,
                        previous: self.current_frontier,
                        representative: self.genesis_key.public_key(),
                        balance: self.current_balance,
                        link: dest.into(),
                        work,
                    }
                    .into();

                    self.current_frontier = block.hash();
                    blocks.push(block);
                    continue;
                }

                match strategy {
                    Strategy::Chained => {
                        let send_amount = Amount::raw(1000);
                        if acc.balance > send_amount {
                            acc.balance = acc.balance - send_amount;
                        }
                        let dest = PrivateKey::from(rand::random::<u64>()).account();
                        let root = Root::from(acc.frontier);
                        let work = Self::compute_work_internal(dev_threshold, &root);

                        let block: Block = StateBlockArgs {
                            key: &acc.key,
                            previous: acc.frontier,
                            representative: acc.representative,
                            balance: acc.balance,
                            link: dest.into(),
                            work,
                        }
                        .into();

                        acc.frontier = block.hash();
                        blocks.push(block);
                    }
                    Strategy::Change => {
                        let new_rep = PublicKey::from(rand::random::<u64>());
                        acc.representative = new_rep;
                        let root = Root::from(acc.frontier);
                        let work = Self::compute_work_internal(dev_threshold, &root);

                        let block: Block = StateBlockArgs {
                            key: &acc.key,
                            previous: acc.frontier,
                            representative: new_rep,
                            balance: acc.balance,
                            link: Link::zero(),
                            work,
                        }
                        .into();

                        acc.frontier = block.hash();
                        blocks.push(block);
                    }
                }
            }
        }

        BlockBatch { blocks }
    }

    #[cfg(test)]
    pub fn generate_chained_sends(&mut self, count: usize) -> BlockBatch {
        self.generate_batch(count, Strategy::Chained)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chained_block_generation() {
        let mut generator = BlockGenerator::with_zero_pow();
        let batch = generator.generate_chained_sends(10);
        assert_eq!(batch.blocks.len(), 10);

        let mut prev = DEV_GENESIS_BLOCK.hash();
        for block in &batch.blocks {
            assert_eq!(block.previous(), prev);
            assert!(block
                .account_field()
                .unwrap()
                .as_key()
                .verify(block.hash().as_bytes(), block.signature())
                .is_ok());
            prev = block.hash();
        }
    }

    #[test]
    fn test_multi_account_generation() {
        let mut generator = BlockGenerator::with_zero_pow();
        generator.init_accounts(5);
        let batch = generator.generate_batch(20, Strategy::Chained);
        assert_eq!(batch.blocks.len(), 20);

        for block in &batch.blocks {
            assert!(block
                .account_field()
                .unwrap()
                .as_key()
                .verify(block.hash().as_bytes(), block.signature())
                .is_ok());
        }

        for i in 0..5 {
            let send = &batch.blocks[i * 2];
            let open = &batch.blocks[i * 2 + 1];

            assert_eq!(open.previous(), BlockHash::zero());
            assert_eq!(open.link_field().unwrap(), send.hash().into());
        }
    }

    #[test]
    fn test_dev_pow_generation() {
        let mut generator = BlockGenerator::default();
        let batch = generator.generate_chained_sends(5);
        let diff = DifficultyV1::default();
        for block in &batch.blocks {
            let root = Root::from(block.previous());
            let d = diff.get_difficulty(&root, block.work());
            assert!(d >= generator.dev_threshold);
        }
    }
}
