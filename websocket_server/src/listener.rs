extern crate alloc;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::net::SocketAddr;

use rsnano_core::{Amount, SavedBlock, ConfirmedElection};
use rsnano_ledger::Ledger;
use rsnano_websocket_messages::{
    ConfirmationJsonOptions, MessageEnvelope, Topic,
};

use super::{ConfirmationOptions, Options, WebsocketSessionEntry};
use crate::{confirmation_message_factory::ConfirmationMessageFactory, WebsocketSession};
use rsnano_nullable_tcp::TcpListener;
use kernel_common::async_rt;

pub struct WebsocketListener {
    endpoint: Mutex<SocketAddr>,
    ledger: Arc<Ledger>,
    topic_subscriber_count: Arc<[AtomicUsize; 11]>,
    sessions: Arc<Mutex<Vec<Weak<WebsocketSessionEntry>>>>,
}

impl WebsocketListener {
    pub fn new(
        endpoint: SocketAddr,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            endpoint: Mutex::new(endpoint),
            ledger,
            topic_subscriber_count: Arc::new(core::array::from_fn(|_| AtomicUsize::new(0))),
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn any_subscriber(&self, topic: Topic) -> bool {
        self.subscriber_count(topic) > 0
    }

    pub fn subscriber_count(&self, topic: Topic) -> usize {
        self.topic_subscriber_count[topic as usize].load(Ordering::SeqCst)
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let endpoint = *self.endpoint.lock();
        let listener = TcpListener::bind(endpoint).await?;
        
        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    let sub_count = Arc::clone(&self.topic_subscriber_count);
                    let sessions = Arc::clone(&self.sessions);
                    let entry = Arc::new(WebsocketSessionEntry::new());

                    {
                        let mut sessions_lock = sessions.lock();
                        sessions_lock.retain(|s| s.strong_count() > 0);
                        sessions_lock.push(Arc::downgrade(&entry));
                    }

                    let session = WebsocketSession::new(sub_count, peer_addr, entry, stream);
                    async_rt::spawn(async move {
                        if let Err(_e) = session.run().await {
                            // Handle error
                        }
                    });
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    /// Broadcast \p message to all session subscribing to the message topic.
    pub fn broadcast(&self, message: &MessageEnvelope) {
        let sessions = self.sessions.lock();
        for session in sessions.iter() {
            if let Some(session) = session.upgrade() {
                let _ = session.blocking_write(message);
            }
        }
    }

    /// Broadcast block confirmation.
    pub fn broadcast_confirmation(
        &self,
        block: &SavedBlock,
        amount: &Amount,
        election: &ConfirmedElection,
    ) {
        if !self.any_subscriber(Topic::Confirmation) {
            return;
        }

        let sessions = self.sessions.lock();
        for session in sessions.iter() {
            if let Some(session) = session.upgrade() {
                let subs = session.subscriptions.lock();
                if let Some(options) = subs.get(&Topic::Confirmation) {
                    let default_opts = ConfirmationOptions::new(ConfirmationJsonOptions::default());
                    let conf_opts = if let Options::Confirmation(i) = options {
                        i
                    } else {
                        &default_opts
                    };

                    let message = ConfirmationMessageFactory {
                        ledger: &self.ledger,
                        options: conf_opts,
                        block,
                        amount,
                        election,
                    }
                    .create_message();

                    drop(subs);
                    let _ = session.blocking_write(&message);
                }
            }
        }
    }
}
