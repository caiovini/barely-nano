use rsnano_core::Block;
use rsnano_rpc_messages::{BlockHashArgs, HashRpcMessage};

pub fn block_hash(args: BlockHashArgs) -> HashRpcMessage {
    let block_enum: Block = args.block.into();
    HashRpcMessage::new(block_enum.hash())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_hash() {
        let block = Block::new_test_instance();
        let json_block = block.json_representation();
        let result = block_hash(BlockHashArgs::new(json_block));
        assert_eq!(result.hash, block.hash());
    }
}
