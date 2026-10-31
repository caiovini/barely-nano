use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::sleep,
};

use rsnano_core::{Block, BlockHash, Networks, PrivateKey, ProtocolInfo, RawKey};
use rsnano_messages::{
    Message, MessageDeserializer, MessageSerializer, NodeIdHandshake, NodeIdHandshakeQuery,
    NodeIdHandshakeResponse, Publish,
};

pub struct PeerSession {
    stream: TcpStream,
    serializer: MessageSerializer,
}

impl PeerSession {
    pub async fn connect(
        target: SocketAddr,
        network: Networks,
        genesis_hash: BlockHash,
    ) -> Result<Self> {
        let mut stream = TcpStream::connect(target)
            .await
            .with_context(|| format!("Failed to connect to {}", target))?;

        let _ = stream.set_nodelay(true);

        let protocol = ProtocolInfo::default_for(network);
        let node_id_key = PrivateKey::from(RawKey::from(12345));

        Self::handshake(&mut stream, protocol, genesis_hash, node_id_key).await?;

        Ok(Self {
            stream,
            serializer: MessageSerializer::new(protocol),
        })
    }

    async fn handshake(
        stream: &mut TcpStream,
        protocol: ProtocolInfo,
        genesis_hash: BlockHash,
        node_id_key: PrivateKey,
    ) -> Result<()> {
        let mut serializer = MessageSerializer::new(protocol);
        let mut deserializer = MessageDeserializer::new(protocol);

        let cookie: [u8; 32] = rand::random();
        let query = NodeIdHandshake {
            query: Some(NodeIdHandshakeQuery { cookie }),
            response: None,
            is_v2: true,
        };
        let buf = serializer.serialize(&Message::NodeIdHandshake(query));
        stream
            .write_all(buf)
            .await
            .context("Failed to write handshake query")?;

        let mut recv_buf = vec![0u8; 2048];
        let mut response_msg = None;

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut recv_buf))
                .await
            {
                Ok(Ok(0)) => bail!("Connection closed by peer during handshake"),
                Ok(Ok(n)) => {
                    deserializer.push(&recv_buf[..n]);
                    while let Some(res) = deserializer.try_deserialize() {
                        let parsed =
                            res.map_err(|e| anyhow::anyhow!("Deserialize error: {:?}", e))?;
                        if let Message::NodeIdHandshake(hs) = parsed.message {
                            response_msg = Some(hs);
                            break;
                        }
                    }
                    if response_msg.is_some() {
                        break;
                    }
                }
                Ok(Err(e)) => bail!("Socket read error during handshake: {}", e),
                Err(_) => continue,
            }
        }

        let hs_response = response_msg
            .ok_or_else(|| anyhow::anyhow!("Timed out waiting for handshake response"))?;

        if let Some(resp) = &hs_response.response {
            resp.validate(&cookie)
                .map_err(|e| anyhow::anyhow!("Handshake signature validation failed: {:?}", e))?;
        }

        if let Some(peer_query) = &hs_response.query {
            let reply = NodeIdHandshake {
                query: None,
                response: Some(NodeIdHandshakeResponse::new_v2(
                    &peer_query.cookie,
                    &node_id_key,
                    genesis_hash,
                )),
                is_v2: true,
            };
            let buf = serializer.serialize(&Message::NodeIdHandshake(reply));
            stream
                .write_all(buf)
                .await
                .context("Failed to send handshake response")?;
            stream.flush().await?;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(())
    }

    /// Stream a batch of blocks over this TCP session.
    /// If `rate_bps` > 0, enforces target rate.
    pub async fn send_blocks(
        &mut self,
        blocks: &[Block],
        rate_bps: usize,
    ) -> Result<(usize, Duration)> {
        let start = Instant::now();
        let mut sent_count = 0;
        let mut write_buf = Vec::with_capacity(32 * 1024);

        let delay_per_block = if rate_bps > 0 {
            Some(Duration::from_secs_f64(1.0 / (rate_bps as f64)))
        } else {
            None
        };

        for (i, block) in blocks.iter().enumerate() {
            let publish = Publish::new_from_originator(block.clone());
            let encoded = self.serializer.serialize(&Message::Publish(publish));
            write_buf.extend_from_slice(encoded);

            if write_buf.len() >= 16 * 1024 || i + 1 == blocks.len() {
                self.stream
                    .write_all(&write_buf)
                    .await
                    .context("Failed to write blocks to socket")?;
                write_buf.clear();
            }

            sent_count += 1;

            if let Some(delay) = delay_per_block {
                if sent_count % 10 == 0 {
                    let expected_elapsed = delay * (sent_count as u32);
                    let actual_elapsed = start.elapsed();
                    if expected_elapsed > actual_elapsed {
                        sleep(expected_elapsed - actual_elapsed).await;
                    }
                }
            }
        }

        if !write_buf.is_empty() {
            self.stream.write_all(&write_buf).await?;
        }

        self.stream.flush().await.context("Failed to flush socket")?;
        Ok((sent_count, start.elapsed()))
    }
}
