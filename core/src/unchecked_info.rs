use super::BlockHash;
use crate::Block;
use crate::utils::UnixTimestamp;

/// Information on an unchecked block
#[derive(Clone, Debug)]
pub struct UncheckedInfo {
    pub block: Block,

    /// Seconds since posix epoch
    pub modified: u64,
}

impl UncheckedInfo {
    pub fn new(block: Block) -> Self {
        Self {
            block,
            modified: UnixTimestamp::now().as_u64(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UncheckedKey {
    pub previous: BlockHash,
    pub hash: BlockHash,
}

impl UncheckedKey {
    pub fn new(previous: BlockHash, hash: BlockHash) -> Self {
        Self { previous, hash }
    }
}
