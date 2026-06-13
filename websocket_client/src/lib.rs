#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use rsnano_core::Account;
use rsnano_websocket_messages::{ConfirmationJsonOptions, MessageEnvelope, Request, Topic};
use rsnano_nullable_random::NullableRng;
use embedded_websocket::{
    WebSocketClient, WebSocketOptions, WebSocketSendMessageType,
    WebSocketReceiveMessageType,
};
use core::net::SocketAddr;
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use anyhow::Result;

pub enum Error {
    WebSocketErr(embedded_websocket::Error),
    InvalidMessage,
    InvalidJson(serde_json::Error),
    NetworkError(anyhow::Error),
}

impl From<embedded_websocket::Error> for Error {
    fn from(value: embedded_websocket::Error) -> Self {
        Self::WebSocketErr(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::InvalidJson(value)
    }
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        Self::NetworkError(value)
    }
}

pub struct NanoWebSocketClient {
    stream: TcpStream,
    ws: WebSocketClient<NullableRng>,
    read_buffer: [u8; 4096],
    read_len: usize,
}

impl NanoWebSocketClient {
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        let factory = TcpStreamFactory::new();
        let stream = factory.connect(addr).await?;
        
        let rng = NullableRng::rng();
        let mut ws = WebSocketClient::new_client(rng);
        
        let options = WebSocketOptions {
            path: "/",
            host: "localhost",
            origin: "localhost",
            sub_protocols: None,
            additional_headers: None,
        };
        
        let mut buffer = [0u8; 1024];
        let (len, key) = ws.client_connect(&options, &mut buffer).map_err(|e| anyhow::anyhow!("{:?}", e))?;
        
        // Send handshake
        let mut written = 0;
        while written < len {
            stream.writable().await?;
            match stream.try_write(&buffer[written..len]) {
                Ok(n) => written += n,
                Err(e) => return Err(anyhow::Error::from(e)),
            }
        }
        
        // Read handshake response
        let mut read_buf = [0u8; 1024];
        let mut read_len = 0;
        loop {
            stream.readable().await?;
            match stream.try_read(&mut read_buf[read_len..]) {
                Ok(0) => return Err(anyhow::anyhow!("Handshake failed: connection closed")),
                Ok(n) => {
                    read_len += n;
                    let result = ws.client_accept(&key, &read_buf[..read_len]);
                    match result {
                        Ok((consumed, _)) => {
                            if consumed > 0 {
                                break;
                            }
                        }
                        Err(e) => return Err(anyhow::anyhow!("{:?}", e)),
                    }
                }
                Err(e) => return Err(anyhow::Error::from(e)),
            }
        }
        
        Ok(Self {
            stream,
            ws,
            read_buffer: [0u8; 4096],
            read_len: 0,
        })
    }

    pub async fn subscribe(&mut self, args: SubscribeArgs<'_>) -> Result<(), Error> {
        let request = Request {
            action: Some("subscribe"),
            topic: Some(Topic::from(&args.topic).into()),
            ack: args.ack,
            id: args.id,
            options: get_subscription_options(args.topic),
        };
        self.send_request(&request).await
    }

    pub async fn send_request(&mut self, request: &Request<'_>) -> Result<(), Error> {
        let req_str = serde_json::to_string(request)?;
        self.send_text(req_str).await?;
        Ok(())
    }

    pub async fn send_text(&mut self, message: String) -> Result<(), Error> {
        let mut buffer = [0u8; 4096];
        let len = self.ws.write(WebSocketSendMessageType::Text, true, message.as_bytes(), &mut buffer)?;
        
        let mut written = 0;
        while written < len {
            self.stream.writable().await?;
            match self.stream.try_write(&buffer[written..len]) {
                Ok(n) => written += n,
                Err(e) => return Err(Error::NetworkError(anyhow::Error::from(e))),
            }
        }
        Ok(())
    }

    pub async fn next_message(&mut self) -> Result<MessageEnvelope, Error> {
        loop {
            // Process what we already have in the buffer
            if self.read_len > 0 {
                let mut buffer = [0u8; 4096];
                match self.ws.read(&self.read_buffer[..self.read_len], &mut buffer) {
                    Ok(result) => {
                        if result.len_from > 0 {
                            // Move remaining data to start of buffer
                            self.read_buffer.copy_within(result.len_from..self.read_len, 0);
                            self.read_len -= result.len_from;

                            match result.message_type {
                                WebSocketReceiveMessageType::Text => {
                                    let text = core::str::from_utf8(&buffer[..result.len_to]).map_err(|_| Error::InvalidMessage)?;
                                    let envelope: MessageEnvelope = serde_json::from_str(text)?;
                                    return Ok(envelope);
                                }
                                WebSocketReceiveMessageType::CloseCompleted => {
                                    return Err(Error::NetworkError(anyhow::anyhow!("WS Closed")));
                                }
                                _ => {} // Continue to read more if needed
                            }
                        }
                    }
                    Err(e) => return Err(Error::from(e)),
                }
            }

            // Read more from stream
            self.stream.readable().await?;
            match self.stream.try_read(&mut self.read_buffer[self.read_len..]) {
                Ok(0) => return Err(Error::NetworkError(anyhow::anyhow!("Connection closed"))),
                Ok(n) => {
                    self.read_len += n;
                }
                Err(e) => return Err(Error::NetworkError(anyhow::Error::from(e))),
            }
        }
    }
}

fn get_subscription_options(opts: TopicSub) -> Option<serde_json::Value> {
    match opts {
        TopicSub::Confirmation(args) => {
            let conf_type_str: &str = args.confirmation_types.into();
            let json_opts = ConfirmationJsonOptions {
                include_block: Some(args.include_block),
                include_election_info: Some(args.include_election_info),
                include_election_info_with_votes: Some(args.include_election_info_with_votes),
                include_linked_account: Some(args.include_linked_account),
                include_sideband_info: Some(args.include_sideband_info),
                confirmation_type: Some(conf_type_str.to_string()),
                all_local_accounts: if args.all_local_accounts {
                    Some(true)
                } else {
                    None
                },
                accounts: if args.accounts.is_empty() {
                    None
                } else {
                    Some(args.accounts.iter().map(|a| a.encode_account()).collect())
                },
            };
            serde_json::to_value(json_opts).ok()
        }
        _ => None,
    }
}

pub struct SubscribeArgs<'a> {
    pub topic: TopicSub,
    pub ack: bool,
    pub id: Option<&'a str>,
}

pub enum TopicSub {
    Confirmation(ConfirmationSubArgs),
    StartedElection,
    StoppedElection,
    Vote,
    Work,
    Bootstrap,
    Telemetry,
    NewUnconfirmedBlock,
}

impl From<&TopicSub> for Topic {
    fn from(value: &TopicSub) -> Self {
        match value {
            TopicSub::Confirmation(_) => Topic::Confirmation,
            TopicSub::StartedElection => Topic::StartedElection,
            TopicSub::StoppedElection => Topic::StoppedElection,
            TopicSub::Vote => Topic::Vote,
            TopicSub::Work => Topic::Work,
            TopicSub::Bootstrap => Topic::Bootstrap,
            TopicSub::Telemetry => Topic::Telemetry,
            TopicSub::NewUnconfirmedBlock => Topic::NewUnconfirmedBlock,
        }
    }
}

pub struct ConfirmationSubArgs {
    pub include_election_info: bool,
    pub include_election_info_with_votes: bool,
    pub include_linked_account: bool,
    pub include_sideband_info: bool,
    pub include_block: bool,
    pub all_local_accounts: bool,
    pub confirmation_types: ConfirmationTypeFilter,
    pub accounts: Vec<Account>,
}

pub enum ConfirmationTypeFilter {
    All,
    Active,
    ActiveQuorum,
    ActiveConfirmationHeight,
    Inactive,
}

impl From<ConfirmationTypeFilter> for &'static str {
    fn from(value: ConfirmationTypeFilter) -> Self {
        match value {
            ConfirmationTypeFilter::All => "all",
            ConfirmationTypeFilter::Active => "active",
            ConfirmationTypeFilter::ActiveQuorum => "active_quorum",
            ConfirmationTypeFilter::ActiveConfirmationHeight => "active_confirmation_height",
            ConfirmationTypeFilter::Inactive => "inactive",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_subscription() {
        // Mock handshake response from server
        // This is a minimal HTTP 101 Switching Protocols response
        let mut handshake_resp = Vec::new();
        handshake_resp.extend_from_slice(b"HTTP/1.1 101 Switching Protocols\r\n");
        handshake_resp.extend_from_slice(b"Upgrade: websocket\r\n");
        handshake_resp.extend_from_slice(b"Connection: Upgrade\r\n");
        handshake_resp.extend_from_slice(b"Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n");

        let stream = TcpStream::new_null_with(handshake_resp);
        let rng = NullableRng::rng();
        let ws = WebSocketClient::new_client(rng);
        
        let mut client = NanoWebSocketClient {
            stream,
            ws,
            read_buffer: [0u8; 4096],
            read_len: 0,
        };

        let args = SubscribeArgs {
            topic: TopicSub::Confirmation(ConfirmationSubArgs {
                include_election_info: true,
                include_election_info_with_votes: false,
                include_linked_account: true,
                include_sideband_info: true,
                include_block: true,
                all_local_accounts: false,
                confirmation_types: ConfirmationTypeFilter::All,
                accounts: Vec::new(),
            }),
            ack: true,
            id: Some("123"),
        };

        // This should not fail
        let result = client.subscribe(args).await;
        assert!(result.is_ok());
    }
}
