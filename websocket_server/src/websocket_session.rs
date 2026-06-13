extern crate alloc;

use alloc::string::ToString;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::net::SocketAddr;

use rsnano_websocket_messages::{
    to_topic, ConfirmationJsonOptions, MessageEnvelope, Request, Topic,
};
use rsnano_nullable_tcp::TcpStream;
use embedded_websocket::{
    WebSocketServer, WebSocketSendMessageType,
    WebSocketReceiveMessageType, read_http_header,
};
use anyhow::Result;

use super::{ConfirmationOptions, Options, VoteJsonOptions, VoteOptions};

pub struct WebsocketSessionEntry {
    /// Map of subscriptions -> options registered by this session.
    pub subscriptions: Mutex<BTreeMap<Topic, Options>>,
    // We need a way to send messages to the session loop.
    // In no_std, we can use a Mutex-protected VecDeque for a simple queue.
    send_queue: Mutex<Vec<MessageEnvelope>>,
}

impl WebsocketSessionEntry {
    pub fn new() -> Self {
        Self {
            subscriptions: Mutex::new(BTreeMap::new()),
            send_queue: Mutex::new(Vec::new()),
        }
    }

    pub fn blocking_write(&self, envelope: &MessageEnvelope) -> Result<()> {
        if !self.should_filter(envelope) {
            self.send_queue.lock().push(envelope.clone());
        }
        Ok(())
    }

    fn should_filter(&self, envelope: &MessageEnvelope) -> bool {
        if envelope.ack.is_some() {
            return false;
        }

        let Some(topic) = envelope.topic else {
            return true;
        };

        let subs = self.subscriptions.lock();
        if let Some(options) = subs.get(&topic) {
            if let Some(msg) = &envelope.message {
                self.should_filter_options(options, msg)
            } else {
                true
            }
        } else {
            true
        }
    }

    pub fn should_filter_options(&self, options: &Options, _message: &serde_json::Value) -> bool {
        match options {
            Options::Confirmation(_) => false, // For now, no filtering
            Options::Vote(i) => i.should_filter(_message),
            Options::Other => false,
        }
    }
}

pub struct WebsocketSession {
    entry: Arc<WebsocketSessionEntry>,
    topic_subscriber_count: Arc<[AtomicUsize; 11]>,
    peer_addr: SocketAddr,
    stream: TcpStream,
    ws: WebSocketServer,
    read_buffer: [u8; 4096],
    read_len: usize,
}

impl WebsocketSession {
    pub fn new(
        topic_subscriber_count: Arc<[AtomicUsize; 11]>,
        _peer_addr: SocketAddr,
        entry: Arc<WebsocketSessionEntry>,
        stream: TcpStream,
    ) -> Self {
        let ws = WebSocketServer::new_server();
        Self {
            entry,
            topic_subscriber_count,
            peer_addr: _peer_addr,
            stream,
            ws,
            read_buffer: [0u8; 4096],
            read_len: 0,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        // First, handle the server handshake
        self.handle_handshake().await?;

        loop {
            // 1. Check if we have messages to send
            let to_send = {
                let mut queue = self.entry.send_queue.lock();
                if !queue.is_empty() {
                    let msg = queue.remove(0);
                    Some(msg)
                } else {
                    None
                }
            };

            if let Some(msg) = to_send {
                let message_text = serde_json::to_string(&msg).unwrap();
                let mut buffer = [0u8; 4096];
                let len = self.ws.write(WebSocketSendMessageType::Text, true, message_text.as_bytes(), &mut buffer)
                    .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                
                let mut written = 0;
                while written < len {
                    self.stream.writable().await?;
                    written += self.stream.try_write(&buffer[written..len])?;
                }
            }

            // 2. Try to read from stream
            self.stream.readable().await?;
            match self.stream.try_read(&mut self.read_buffer[self.read_len..]) {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    self.read_len += n;
                    let mut buffer = [0u8; 4096];
                    match self.ws.read(&self.read_buffer[..self.read_len], &mut buffer) {
                        Ok(result) => {
                            if result.len_from > 0 {
                                self.read_buffer.copy_within(result.len_from..self.read_len, 0);
                                self.read_len -= result.len_from;

                                match result.message_type {
                                    WebSocketReceiveMessageType::Text => {
                                        let text = core::str::from_utf8(&buffer[..result.len_to]).unwrap();
                                        if let Ok(request) = serde_json::from_str::<Request>(text) {
                                            self.handle_message(request).await?;
                                        }
                                    }
                                    WebSocketReceiveMessageType::CloseCompleted => {
                                        return Ok(());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    async fn handle_handshake(&mut self) -> Result<()> {
        loop {
            self.stream.readable().await?;
            let n = self.stream.try_read(&mut self.read_buffer[self.read_len..])?;
            if n == 0 { return Err(anyhow::anyhow!("Closed during handshake")); }
            self.read_len += n;

            let mut headers = [httparse::EMPTY_HEADER; 16];
            let mut req = httparse::Request::new(&mut headers);
            match req.parse(&self.read_buffer[..self.read_len]) {
                Ok(httparse::Status::Complete(consumed)) => {
                    let header_it = req.headers.iter().map(|h| (h.name, h.value));
                    if let Some(ws_ctx) = read_http_header(header_it).map_err(|e| anyhow::anyhow!("{:?}", e))? {
                        let mut buffer = [0u8; 1024];
                        let len = self.ws.server_accept(&ws_ctx.sec_websocket_key, None, &mut buffer)
                            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
                        
                        let mut written = 0;
                        while written < len {
                            self.stream.writable().await?;
                            written += self.stream.try_write(&buffer[written..len])?;
                        }
                        
                        self.read_buffer.copy_within(consumed..self.read_len, 0);
                        self.read_len -= consumed;
                        return Ok(());
                    } else {
                        return Err(anyhow::anyhow!("Not a websocket upgrade request"));
                    }
                }
                Ok(httparse::Status::Partial) => {
                    if self.read_len >= self.read_buffer.len() {
                        return Err(anyhow::anyhow!("Handshake too large"));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("HTTP parse error: {:?}", e)),
            }
        }
    }

    async fn handle_message(&self, message: Request<'_>) -> Result<()> {
        let topic = to_topic(message.topic.unwrap_or(""));
        let mut action_succeeded = false;
        let mut ack = message.ack;
        let mut reply_action = message.action.unwrap_or("");
        
        if message.action == Some("subscribe") && topic != Topic::Invalid {
            let mut subs = self.entry.subscriptions.lock();
            let options = match topic {
                Topic::Confirmation => {
                    if let Some(options_value) = message.options {
                        if let Ok(json_opts) = serde_json::from_value::<ConfirmationJsonOptions>(options_value) {
                            Options::Confirmation(ConfirmationOptions::new(json_opts))
                        } else {
                            Options::Other
                        }
                    } else {
                        Options::Other
                    }
                }
                Topic::Vote => {
                    if let Some(options_value) = message.options {
                        if let Ok(json_opts) = serde_json::from_value::<VoteJsonOptions>(options_value) {
                            Options::Vote(VoteOptions::new(json_opts))
                        } else {
                            Options::Other
                        }
                    } else {
                        Options::Other
                    }
                }
                _ => Options::Other,
            };
            let inserted = subs.insert(topic, options).is_none();
            if inserted {
                self.topic_subscriber_count[topic as usize].fetch_add(1, Ordering::SeqCst);
            }
            action_succeeded = true;
        } else if message.action == Some("update") {
            let mut subs = self.entry.subscriptions.lock();
            if let Some(option) = subs.get_mut(&topic) {
                if let Some(options_value) = message.options {
                    option.update(&options_value);
                    action_succeeded = true;
                }
            }
        } else if message.action == Some("unsubscribe") && topic != Topic::Invalid {
            let mut subs = self.entry.subscriptions.lock();
            if subs.remove(&topic).is_some() {
                self.topic_subscriber_count[topic as usize].fetch_sub(1, Ordering::SeqCst);
            }
            action_succeeded = true;
        } else if message.action == Some("ping") {
            action_succeeded = true;
            ack = true;
            reply_action = "pong";
        }

        if ack && action_succeeded {
            self.entry.blocking_write(&MessageEnvelope::new_ack(
                message.id.map(|s| s.to_string()),
                reply_action.to_string(),
            ))?;
        }
        Ok(())
    }
}

impl Drop for WebsocketSession {
    fn drop(&mut self) {
        let subs = self.entry.subscriptions.lock();
        for (topic, _) in subs.iter() {
            self.topic_subscriber_count[*topic as usize].fetch_sub(1, Ordering::SeqCst);
        }
    }
}
