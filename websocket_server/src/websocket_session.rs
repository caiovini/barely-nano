extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::ToString;
use alloc::sync::Arc;
use core::future::{poll_fn, Future};
use core::net::SocketAddr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Poll, Waker};
use spin::Mutex;

use anyhow::Result;
use embedded_websocket::{
    read_http_header, WebSocketReceiveMessageType, WebSocketSendMessageType, WebSocketServer,
};
use rsnano_node::wallets::Wallets;
use rsnano_nullable_tcp::TcpStream;
use rsnano_websocket_messages::{
    to_topic, ConfirmationJsonOptions, MessageEnvelope, Request, Topic,
};

use super::{ConfirmationOptions, Options, VoteJsonOptions, VoteOptions};

pub struct WebsocketSessionEntry {
    /// Map of subscriptions -> options registered by this session.
    pub subscriptions: Mutex<BTreeMap<Topic, Options>>,
    send_queue: Mutex<VecDeque<MessageEnvelope>>,
    waker: Mutex<Option<Waker>>,
    closed: AtomicBool,
    wallets: Arc<Wallets>,
}

impl WebsocketSessionEntry {
    const MAX_SEND_QUEUE_LEN: usize = 1024;

    pub fn new(wallets: Arc<Wallets>) -> Self {
        Self {
            subscriptions: Mutex::new(BTreeMap::new()),
            send_queue: Mutex::new(VecDeque::new()),
            waker: Mutex::new(None),
            closed: AtomicBool::new(false),
            wallets,
        }
    }

    pub fn blocking_write(&self, envelope: &MessageEnvelope) -> Result<()> {
        if !self.should_filter(envelope) {
            let mut queue = self.send_queue.lock();
            if queue.len() >= Self::MAX_SEND_QUEUE_LEN {
                queue.pop_front();
            }
            queue.push_back(envelope.clone());
            drop(queue);
            if let Some(waker) = self.waker.lock().take() {
                waker.wake();
            }
        }
        Ok(())
    }

    pub async fn write(&self, envelope: &MessageEnvelope) -> Result<()> {
        self.blocking_write(envelope)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().take() {
            waker.wake();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn pop_queued(&self) -> Option<MessageEnvelope> {
        self.send_queue.lock().pop_front()
    }

    pub fn has_queued(&self) -> bool {
        !self.send_queue.lock().is_empty()
    }

    fn register_waker(&self, waker: &Waker) {
        let mut guard = self.waker.lock();
        match &*guard {
            Some(existing) if existing.will_wake(waker) => {}
            _ => *guard = Some(waker.clone()),
        }
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

    pub fn should_filter_options(&self, options: &Options, message: &serde_json::Value) -> bool {
        match options {
            Options::Confirmation(i) => i.should_filter(message, &self.wallets),
            Options::Vote(i) => i.should_filter(message),
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
        peer_addr: SocketAddr,
        entry: Arc<WebsocketSessionEntry>,
        stream: TcpStream,
    ) -> Self {
        let ws = WebSocketServer::new_server();
        Self {
            entry,
            topic_subscriber_count,
            peer_addr,
            stream,
            ws,
            read_buffer: [0u8; 4096],
            read_len: 0,
        }
    }

    async fn send_ws_frame(
        &mut self,
        msg_type: WebSocketSendMessageType,
        payload: &[u8],
    ) -> Result<()> {
        let mut buffer = alloc::vec![0u8; payload.len() + 14];
        let len = self
            .ws
            .write(msg_type, true, payload, &mut buffer)
            .map_err(|e| anyhow::anyhow!("{:?}", e))?;

        let mut written = 0;
        while written < len {
            self.stream.writable().await?;
            written += self.stream.try_write(&buffer[written..len])?;
        }
        Ok(())
    }

    async fn flush_send_queue(&mut self) -> Result<()> {
        while let Some(msg) = self.entry.pop_queued() {
            let message_text = serde_json::to_string_pretty(&msg)?;
            self.send_ws_frame(WebSocketSendMessageType::Text, message_text.as_bytes())
                .await?;
        }
        Ok(())
    }

    pub async fn run(mut self) -> Result<()> {
        let _ = &self.peer_addr;
        // First, handle the server handshake
        self.handle_handshake().await?;

        loop {
            if self.entry.is_closed() {
                break;
            }

            // 1. Flush any queued outgoing messages
            self.flush_send_queue().await?;

            // 2. Process any complete WebSocket frames already in read_buffer
            while self.read_len > 0 {
                let mut buffer = [0u8; 4096];
                match self.ws.read(&self.read_buffer[..self.read_len], &mut buffer) {
                    Ok(result) => {
                        if result.len_from == 0 {
                            break;
                        }
                        self.read_buffer.copy_within(result.len_from..self.read_len, 0);
                        self.read_len -= result.len_from;

                        match result.message_type {
                            WebSocketReceiveMessageType::Text => {
                                if let Ok(text) = core::str::from_utf8(&buffer[..result.len_to]) {
                                    if let Ok(request) = serde_json::from_str::<Request>(text) {
                                        self.handle_message(request).await?;
                                    }
                                }
                            }
                            WebSocketReceiveMessageType::Ping => {
                                let pong_len = result.len_to;
                                self.send_ws_frame(
                                    WebSocketSendMessageType::Pong,
                                    &buffer[..pong_len],
                                )
                                .await?;
                            }
                            WebSocketReceiveMessageType::CloseMustReply => {
                                let close_len = result.len_to;
                                let _ = self
                                    .send_ws_frame(
                                        WebSocketSendMessageType::CloseReply,
                                        &buffer[..close_len],
                                    )
                                    .await;
                                return Ok(());
                            }
                            WebSocketReceiveMessageType::CloseCompleted => {
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                    Err(embedded_websocket::Error::ReadFrameIncomplete) => break,
                    Err(_) => return Ok(()),
                }
            }

            // Flush any ACKs produced by handle_message before waiting on socket
            self.flush_send_queue().await?;

            // 3. Wait until either send_queue has outgoing messages, session is closed, or TCP stream is readable
            let stream_ready = {
                let mut readable_fut = core::pin::pin!(self.stream.readable());
                poll_fn(|cx| {
                    if self.entry.is_closed() {
                        return Poll::Ready(Ok(false));
                    }
                    self.entry.register_waker(cx.waker());
                    if self.entry.has_queued() {
                        return Poll::Ready(Ok(false));
                    }
                    match readable_fut.as_mut().poll(cx) {
                        Poll::Ready(Ok(())) => Poll::Ready(Ok(true)),
                        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                        Poll::Pending => Poll::Pending,
                    }
                })
                .await
            };

            let stream_ready = match stream_ready {
                Ok(r) => r,
                Err(_) => break,
            };

            if self.entry.is_closed() {
                break;
            }

            if !stream_ready {
                // Woken up because send_queue has new broadcast messages
                continue;
            }

            // 4. Read available bytes from TCP stream
            match self.stream.try_read(&mut self.read_buffer[self.read_len..]) {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    self.read_len += n;
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
            if n == 0 {
                return Err(anyhow::anyhow!("Closed during handshake"));
            }
            self.read_len += n;

            let mut headers = [httparse::EMPTY_HEADER; 16];
            let mut req = httparse::Request::new(&mut headers);
            match req.parse(&self.read_buffer[..self.read_len]) {
                Ok(httparse::Status::Complete(consumed)) => {
                    let header_it = req.headers.iter().map(|h| (h.name, h.value));
                    if let Some(ws_ctx) =
                        read_http_header(header_it).map_err(|e| anyhow::anyhow!("{:?}", e))?
                    {
                        let mut buffer = [0u8; 1024];
                        let len = self
                            .ws
                            .server_accept(&ws_ctx.sec_websocket_key, None, &mut buffer)
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
                        if let Ok(json_opts) =
                            serde_json::from_value::<ConfirmationJsonOptions>(options_value)
                        {
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
                        if let Ok(json_opts) = serde_json::from_value::<VoteJsonOptions>(options_value)
                        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::{
        pin::Pin,
        task::{Context, RawWaker, RawWakerVTable},
    };
    use embedded_websocket::{WebSocketClient, WebSocketOptions};
    use rsnano_core::Account;
    use rsnano_nullable_random::NullableRng;

    fn block_on<F: Future>(mut fut: F) -> F::Output {
        fn noop_clone(_: *const ()) -> RawWaker {
            RawWaker::new(core::ptr::null(), &VTABLE)
        }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = unsafe { Pin::new_unchecked(&mut fut) };
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("future returned Pending in synchronous test"),
        }
    }

    #[test]
    fn session_entry_filters_confirmations_and_acks() {
        let wallets = Arc::new(Wallets::new_null());
        let entry = WebsocketSessionEntry::new(wallets);

        // ACKs are never filtered
        let ack = MessageEnvelope::new_ack(Some("1".to_string()), "subscribe".to_string());
        entry.blocking_write(&ack).unwrap();
        assert!(entry.has_queued());
        assert_eq!(entry.pop_queued().unwrap().ack.as_deref(), Some("subscribe"));

        // Unsubscribed topics are filtered
        let started = MessageEnvelope::new(
            Topic::StartedElection,
            serde_json::json!({ "hash": "1234" }),
        );
        entry.blocking_write(&started).unwrap();
        assert!(!entry.has_queued());

        // Subscribe to Confirmation with account filter
        let acc1 = Account::from(1).encode_account();
        let acc2 = Account::from(2).encode_account();
        let conf_opts = ConfirmationOptions::new(ConfirmationJsonOptions {
            accounts: Some(alloc::vec![acc1.clone()]),
            ..Default::default()
        });
        entry
            .subscriptions
            .lock()
            .insert(Topic::Confirmation, Options::Confirmation(conf_opts));

        let env_filtered = MessageEnvelope::new(
            Topic::Confirmation,
            serde_json::json!({
                "confirmation_type": "active_quorum",
                "account": acc2,
                "block": { "link_as_account": acc2 }
            }),
        );
        entry.blocking_write(&env_filtered).unwrap();
        assert!(!entry.has_queued());

        let env_allowed = MessageEnvelope::new(
            Topic::Confirmation,
            serde_json::json!({
                "confirmation_type": "active_quorum",
                "account": acc1,
                "block": { "link_as_account": acc2 }
            }),
        );
        entry.blocking_write(&env_allowed).unwrap();
        assert!(entry.has_queued());
        assert_eq!(entry.pop_queued().unwrap().topic, Some(Topic::Confirmation));
    }

    #[test]
    fn session_handles_handshake_subscribe_ping_and_unsubscribe() {
        let mut client_ws = WebSocketClient::new_client(NullableRng::rng());
        let options = WebSocketOptions {
            path: "/",
            host: "localhost",
            origin: "localhost",
            sub_protocols: None,
            additional_headers: None,
        };
        let mut handshake_buf = [0u8; 512];
        let (hs_len, key) = client_ws
            .client_connect(&options, &mut handshake_buf)
            .unwrap();

        // Advance client_ws state to Open so it can encode masked client frames
        let mut dummy_server = WebSocketServer::new_server();
        let mut resp_buf = [0u8; 512];
        let resp_len = dummy_server
            .server_accept(&key, None, &mut resp_buf)
            .unwrap();
        client_ws
            .client_accept(&key, &resp_buf[..resp_len])
            .unwrap();

        let sub_req = r#"{"action":"subscribe","topic":"confirmation","ack":true,"id":"sub1"}"#;
        let ping_req = r#"{"action":"ping","id":"ping1"}"#;

        let mut frame1 = [0u8; 256];
        let f1_len = client_ws
            .write(
                WebSocketSendMessageType::Text,
                true,
                sub_req.as_bytes(),
                &mut frame1,
            )
            .unwrap();

        let mut frame2 = [0u8; 256];
        let f2_len = client_ws
            .write(
                WebSocketSendMessageType::Text,
                true,
                ping_req.as_bytes(),
                &mut frame2,
            )
            .unwrap();

        let mut incoming = Vec::new();
        incoming.extend_from_slice(&handshake_buf[..hs_len]);
        incoming.extend_from_slice(&frame1[..f1_len]);
        incoming.extend_from_slice(&frame2[..f2_len]);

        let stream = TcpStream::new_null_with(incoming);
        let sub_counts: Arc<[AtomicUsize; 11]> =
            Arc::new(core::array::from_fn(|_| AtomicUsize::new(0)));
        let wallets = Arc::new(Wallets::new_null());
        let entry = Arc::new(WebsocketSessionEntry::new(wallets));
        let peer_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();

        let session = WebsocketSession::new(
            sub_counts.clone(),
            peer_addr,
            entry.clone(),
            stream,
        );

        // Run session until stream runs out of incoming test data (before Drop decrements count, check entry)
        block_on(session.run()).unwrap();

        // Entry still has the subscription recorded in its map
        assert!(entry.subscriptions.lock().contains_key(&Topic::Confirmation));
    }
}
