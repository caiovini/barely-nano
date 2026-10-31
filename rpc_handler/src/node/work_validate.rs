use alloc::borrow::ToOwned;
use crate::RpcCommandHandler;
use rsnano_core::{BlockDetails, DifficultyV1};
use rsnano_rpc_messages::{WorkValidateArgs, WorkValidateResponse};

impl RpcCommandHandler {
    pub(crate) fn work_validate(&self, args: WorkValidateArgs) -> WorkValidateResponse {
        let default_difficulty = self.node.network_params.work.threshold_base();

        let difficulty = if let Some(multiplier) = args.multiplier {
            DifficultyV1::from_multiplier(multiplier.inner(), default_difficulty)
        } else if let Some(diff) = args.difficulty {
            diff.into()
        } else {
            default_difficulty
        };

        let result_difficulty = self
            .node
            .network_params
            .work
            .difficulty(&args.hash.into(), args.work.unwrap_or_default());

        let valid = if args.difficulty.is_some() || args.multiplier.is_some() {
            if result_difficulty >= difficulty {
                Some("1".to_owned())
            } else {
                Some("0".to_owned())
            }
        } else {
            None
        };

        let valid_all = if result_difficulty >= default_difficulty {
            "1".to_owned()
        } else {
            "0".to_owned()
        };

        let receive_difficulty = self.node.network_params.work.threshold(&BlockDetails::new(
            rsnano_core::Epoch::Epoch2,
            false,
            true,
            false,
        ));
        let valid_receive = if result_difficulty >= receive_difficulty {
            "1".to_owned()
        } else {
            "0".to_owned()
        };

        let result_multiplier = DifficultyV1::to_multiplier(result_difficulty, default_difficulty);

        WorkValidateResponse {
            valid,
            valid_all,
            valid_receive,
            difficulty: result_difficulty.into(),
            multiplier: result_multiplier.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_rpc_command;
    use rsnano_core::{BlockHash, WorkNonce};
    use rsnano_rpc_messages::RpcCommand;

    #[test]
    fn test_work_validate() {
        let args = WorkValidateArgs {
            hash: BlockHash::from(1),
            work: Some(WorkNonce::from(1)),
            multiplier: None,
            difficulty: None,
        };
        let response: WorkValidateResponse = test_rpc_command(RpcCommand::WorkValidate(args));
        assert!(response.valid.is_none());
        assert!(!response.valid_all.is_empty());
        assert!(!response.valid_receive.is_empty());
    }
}
