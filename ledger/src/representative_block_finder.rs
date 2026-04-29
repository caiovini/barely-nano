use rsnano_core::{Block, BlockHash};
use rsnano_ledger_store::BtreeStore;

/// Goes back in the block history until it finds a block with representative information
pub(crate) struct RepresentativeBlockFinder<'a> {
    store: &'a BtreeStore,
}

impl<'a> RepresentativeBlockFinder<'a> {
    pub fn new(store: &'a BtreeStore) -> Self {
        Self { store }
    }

    pub fn find_rep_block(&self, hash: BlockHash) -> BlockHash {
        let mut current = hash;
        let mut result = BlockHash::zero();
        while result.is_zero() {
            let Some(block) = self.store.block.get(&current) else {
                return BlockHash::zero();
            };
            (current, result) = match &*block {
                Block::LegacySend(_) => (block.previous(), BlockHash::zero()),
                Block::LegacyReceive(_) => (block.previous(), BlockHash::zero()),
                Block::LegacyOpen(_) => (BlockHash::zero(), block.hash()),
                Block::LegacyChange(_) => (BlockHash::zero(), block.hash()),
                Block::State(_) => (BlockHash::zero(), block.hash()),
            };
        }

        result
    }
}
