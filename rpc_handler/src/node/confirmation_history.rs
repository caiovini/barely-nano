use alloc::vec::Vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::{
    ConfirmationEntry, ConfirmationHistoryArgs, ConfirmationHistoryResponse, ConfirmationStats,
};
use core::time::Duration;

impl RpcCommandHandler {
    pub(crate) fn confirmation_history(
        &self,
        args: ConfirmationHistoryArgs,
    ) -> ConfirmationHistoryResponse {
        let mut elections = Vec::new();
        let mut running_total = Duration::ZERO;
        let hash = args.hash.unwrap_or_default();
        
        let guard = self.node.recently_cemented.lock();
        for election in guard.iter() {
            if hash.is_zero() || election.winner.hash() == hash {
                elections.push(ConfirmationEntry {
                    hash: election.winner.hash(),
                    duration: (election.election_duration.as_millis() as u64).into(),
                    time: 0.into(), // SystemTime is not available in no_std
                    tally: election.tally,
                    final_tally: election.final_tally,
                    blocks: election.block_count.into(),
                    voters: election.voter_count.into(),
                    request_count: 0.into(),
                });
            }
            running_total += election.election_duration;
        }

        ConfirmationHistoryResponse {
            confirmation_stats: ConfirmationStats {
                count: (elections.len() as u32).into(),
                average: if elections.is_empty() {
                    None
                } else {
                    Some((running_total.as_millis() as u64 / elections.len() as u64).into())
                },
            },
            confirmations: elections,
        }
    }
}
