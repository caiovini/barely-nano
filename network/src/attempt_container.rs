use rsnano_nullable_clock::Timestamp;
use core::net::SocketAddrV6;
use hashbrown::HashMap;

pub struct AttemptContainer {
    attempts: HashMap<SocketAddrV6, Attempt>,
}

impl AttemptContainer {
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    pub fn insert(&mut self, peer: SocketAddrV6, now: Timestamp) -> bool {
        if self.attempts.contains_key(&peer) {
            return false;
        }
        self.attempts.insert(peer, Attempt::new(now));
        true
    }

    pub fn remove(&mut self, peer: &SocketAddrV6) {
        self.attempts.remove(peer);
    }

    pub fn get(&self, peer: &SocketAddrV6) -> Option<&Attempt> {
        self.attempts.get(peer)
    }

    pub fn count(&self) -> usize {
        self.attempts.len()
    }

    pub fn purge(&mut self, now: Timestamp, cutoff_period: core::time::Duration) {
        self.attempts.retain(|_, attempt| {
            now.millis() - attempt.last_attempt.millis() < cutoff_period.as_millis() as i64
        });
    }
}

pub struct Attempt {
    pub last_attempt: Timestamp,
}

impl Attempt {
    pub fn new(now: Timestamp) -> Self {
        Self { last_attempt: now }
    }
}
