use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

#[derive(Debug, Deserialize, Clone)]
pub struct BlockCountResponse {
    pub count: String,
    pub unchecked: String,
    pub cemented: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VersionResponse {
    pub network: Option<String>,
    pub node_vendor: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AccountInfoResponse {
    pub frontier: String,
    pub balance: String,
    pub block_count: String,
}

pub struct RpcMonitor {
    host_port: String,
    path: String,
}

impl RpcMonitor {
    pub fn new(endpoint: &str) -> Self {
        let stripped = endpoint
            .trim()
            .strip_prefix("http://")
            .or_else(|| endpoint.trim().strip_prefix("https://"))
            .unwrap_or(endpoint.trim());

        let (host_port, path) = match stripped.split_once('/') {
            Some((hp, p)) => (hp.to_string(), format!("/{}", p)),
            None => (stripped.to_string(), "/".to_string()),
        };

        Self { host_port, path }
    }

    async fn post_json<T: DeserializeOwned>(&self, payload: &serde_json::Value) -> Result<T> {
        let body = serde_json::to_string(payload)?;
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.path,
            self.host_port,
            body.len(),
            body
        );

        let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(&self.host_port))
            .await
            .context("RPC connection timed out")?
            .with_context(|| format!("Failed to connect to RPC {}", self.host_port))?;

        timeout(Duration::from_secs(3), stream.write_all(request.as_bytes()))
            .await
            .context("RPC write timed out")??;

        let mut buf = Vec::with_capacity(4096);
        timeout(Duration::from_secs(3), stream.read_to_end(&mut buf))
            .await
            .context("RPC read timed out")??;

        let resp_str = String::from_utf8_lossy(&buf);
        let json_body = if let Some((_, b)) = resp_str.split_once("\r\n\r\n") {
            b.trim()
        } else if let Some((_, b)) = resp_str.split_once("\n\n") {
            b.trim()
        } else {
            bail!("Invalid HTTP response from RPC: {}", resp_str);
        };

        serde_json::from_str::<T>(json_body)
            .with_context(|| format!("Failed to parse RPC JSON: {}", json_body))
    }

    pub async fn get_version(&self) -> Result<VersionResponse> {
        let payload = serde_json::json!({ "action": "version" });
        self.post_json(&payload).await
    }

    pub async fn get_block_count(&self) -> Result<BlockCountResponse> {
        let payload = serde_json::json!({ "action": "block_count" });
        self.post_json(&payload).await
    }

    pub async fn get_account_info(&self, account: &str) -> Result<AccountInfoResponse> {
        let payload = serde_json::json!({
            "action": "account_info",
            "account": account
        });
        self.post_json(&payload).await
    }
}
