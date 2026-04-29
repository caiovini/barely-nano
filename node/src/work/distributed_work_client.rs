use super::WorkRequest;
use rsnano_core::{Root, WorkNonce};

#[derive(Default)]
pub(crate) struct DistributedWorkClient {}

impl DistributedWorkClient {
    pub async fn generate_work(
        &self,
        _url: &str,
        _request: WorkRequest,
    ) -> anyhow::Result<WorkNonce> {
        Err(anyhow!("Distributed work not supported in no_std"))
    }
}
