use super::WorkRequest;
use rsnano_core::WorkNonce;

#[derive(Default)]
pub struct DistributedWorkClient {}

impl DistributedWorkClient {
    pub async fn generate_work(
        &self,
        _url: &str,
        _request: WorkRequest,
    ) -> anyhow::Result<WorkNonce> {
        Err(anyhow!("Distributed work not supported in no_std"))
    }
}
