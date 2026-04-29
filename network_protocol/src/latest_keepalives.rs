use rand::seq::IteratorRandom;
use rsnano_messages::Keepalive;
use rsnano_network::{ChannelId, DeadChannelCleanupStep};
use hashbrown::HashMap;
use alloc::sync::Arc;
use spin::Mutex;

/// Keeps the last keepalive message per channel in memory, so that we can
/// later use that information, when we want to connect to more nodes
pub struct LatestKeepalives {
    entries: HashMap<ChannelId, Keepalive>,
    max_len: usize,
}

impl Default for LatestKeepalives {
    fn default() -> Self {
        Self::with_max_len(1000)
    }
}

impl LatestKeepalives {
    pub fn with_max_len(max_len: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_len,
        }
    }

    pub fn insert(&mut self, channel_id: ChannelId, keepalive: Keepalive) {
        while self.len() >= self.max_len {
            self.pop_random();
        }
        self.entries.insert(channel_id, keepalive);
    }

    pub fn get(&self, channel_id: ChannelId) -> Option<&Keepalive> {
        self.entries.get(&channel_id)
    }

    //TODO: randomize
    pub fn pop_random(&mut self) -> Option<Keepalive> {
        let channel_id = *self.entries.keys().next()?;
        self.entries.remove(&channel_id)
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        self.entries.remove(&channel_id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn max_len(&self) -> usize {
        self.max_len
    }
}

pub struct LatestKeepalivesCleanup {
    keepalives: Arc<Mutex<LatestKeepalives>>,
}

impl LatestKeepalivesCleanup {
    pub fn new(keepalives: Arc<Mutex<LatestKeepalives>>) -> Self {
        Self { keepalives }
    }
}

impl DeadChannelCleanupStep for LatestKeepalivesCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut keepalives = self.keepalives.lock();
        for channel_id in dead_channel_ids {
            keepalives.remove(*channel_id);
        }
    }
}
