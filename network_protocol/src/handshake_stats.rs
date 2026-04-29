use core::sync::atomic::{AtomicUsize, Ordering};
use rsnano_messages::MessageType;
use strum::IntoEnumIterator;

pub struct HandshakeStats {
    pub handshakes_sent: AtomicUsize,
    pub handshakes_received: AtomicUsize,
    pub initiate: AtomicUsize,
    pub response_sent: AtomicUsize,
    pub response_ok: AtomicUsize,
    pub response_invalid: AtomicUsize,
    pub handshake_error: AtomicUsize,
    pub network_error: AtomicUsize,
    pub errors: [AtomicUsize; 6],
}

impl Default for HandshakeStats {
    fn default() -> Self {
        Self {
            handshakes_sent: AtomicUsize::new(0),
            handshakes_received: AtomicUsize::new(0),
            initiate: AtomicUsize::new(0),
            response_sent: AtomicUsize::new(0),
            response_ok: AtomicUsize::new(0),
            response_invalid: AtomicUsize::new(0),
            handshake_error: AtomicUsize::new(0),
            network_error: AtomicUsize::new(0),
            errors: [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
        }
    }
}
