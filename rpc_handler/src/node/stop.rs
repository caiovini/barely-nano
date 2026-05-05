use alloc::string::String;
use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::vec;
use crate::RpcCommandHandler;
use rsnano_rpc_messages::SuccessResponse;

impl RpcCommandHandler {
    pub(crate) fn stop(&self) -> SuccessResponse {
        #[cfg(not(target_os = "none"))]
        if let Some(tx_stop) = self.stop.lock().take() {
            let _ = tx_stop.send(());
        }
        SuccessResponse::new()
    }
}
