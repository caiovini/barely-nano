extern crate alloc;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::net::SocketAddr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use rsnano_core::{Amount, SavedBlock};
use rsnano_ledger::Ledger;
use rsnano_node::{consensus::election::ConfirmedElection, wallets::Wallets};
use rsnano_websocket_messages::{ConfirmationJsonOptions, MessageEnvelope, Topic};

use super::{ConfirmationOptions, Options, WebsocketSessionEntry};
use crate::{confirmation_message_factory::ConfirmationMessageFactory, WebsocketSession};
use kernel_common::async_rt;
use rsnano_nullable_tcp::TcpListener;

pub struct WebsocketListener {
    endpoint: Mutex<SocketAddr>,
    wallets: Arc<Wallets>,
    ledger: Arc<Ledger>,
    topic_subscriber_count: Arc<[AtomicUsize; 11]>,
    sessions: Arc<Mutex<Vec<Weak<WebsocketSessionEntry>>>>,
    stopped: AtomicBool,
}

impl WebsocketListener {
    const MAX_SESSIONS: usize = 32;

    pub fn new(endpoint: SocketAddr, wallets: Arc<Wallets>, ledger: Arc<Ledger>) -> Self {
        Self {
            endpoint: Mutex::new(endpoint),
            wallets,
            ledger,
            topic_subscriber_count: Arc::new(core::array::from_fn(|_| AtomicUsize::new(0))),
            sessions: Arc::new(Mutex::new(Vec::new())),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn any_subscriber(&self, topic: Topic) -> bool {
        self.subscriber_count(topic) > 0
    }

    pub fn subscriber_count(&self, topic: Topic) -> usize {
        self.topic_subscriber_count[topic as usize].load(Ordering::SeqCst)
    }

    pub fn listening_port(&self) -> u16 {
        self.endpoint.lock().port()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let endpoint = *self.endpoint.lock();
        let listener = TcpListener::bind(endpoint).await?;

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    if self.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let sub_count = Arc::clone(&self.topic_subscriber_count);
                    let sessions = Arc::clone(&self.sessions);
                    let wallets = Arc::clone(&self.wallets);
                    let entry = Arc::new(WebsocketSessionEntry::new(wallets));

                    {
                        let mut sessions_lock = sessions.lock();
                        sessions_lock.retain(|s| s.strong_count() > 0);
                        if sessions_lock.len() >= Self::MAX_SESSIONS {
                            continue;
                        }
                        sessions_lock.push(Arc::downgrade(&entry));
                    }

                    let session = WebsocketSession::new(sub_count, peer_addr, entry, stream);
                    async_rt::spawn_named(
                        async move {
                            let _ = session.run().await;
                        },
                        Some("WebsocketSession"),
                    );
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    /// Close all websocket sessions and stop listening for new connections
    pub async fn stop_async(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let mut sessions = self.sessions.lock();
        for session in sessions.drain(..) {
            if let Some(session) = session.upgrade() {
                session.close();
            }
        }
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

pub trait WebsocketListenerExt {
    fn start(&self);
    fn stop(&self);
}

impl WebsocketListenerExt for Arc<WebsocketListener> {
    fn start(&self) {
        let self_l = Arc::clone(self);
        async_rt::spawn_named(
            async move {
                let _ = self_l.run().await;
            },
            Some("WebsocketListener"),
        );
    }

    fn stop(&self) {
        let self_l = Arc::clone(self);
        async_rt::spawn(async move {
            self_l.stop_async().await;
        });
    }
}
