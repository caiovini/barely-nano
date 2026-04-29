use rsnano_core::utils::ContainerInfo;
use rsnano_nullable_clock::Timestamp;
use core::net::Ipv6Addr;
use hashbrown::{HashMap, HashSet};

pub struct PeerExclusion {
    excluded: HashMap<Ipv6Addr, ExcludedPeer>,
}

impl PeerExclusion {
    pub fn new() -> Self {
        Self {
            excluded: HashMap::new(),
        }
    }

    pub fn exclude(&mut self, ip: Ipv6Addr, now: Timestamp) {
        self.excluded.insert(ip, ExcludedPeer::new(now));
    }

    pub fn is_excluded(&self, ip: &Ipv6Addr) -> bool {
        self.excluded.contains_key(ip)
    }

    pub fn remove(&mut self, ip: &Ipv6Addr) {
        self.excluded.remove(ip);
    }

    pub fn count(&self) -> usize {
        self.excluded.len()
    }

    pub fn purge(&mut self, now: Timestamp, cutoff_period: core::time::Duration) {
        self.excluded.retain(|_, peer| {
            now.millis() - peer.excluded_at.millis() < cutoff_period.as_millis() as i64
        });
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("excluded", self.excluded.len(), 0)].into()
    }
}

pub struct ExcludedPeer {
    pub excluded_at: Timestamp,
}

impl ExcludedPeer {
    pub fn new(now: Timestamp) -> Self {
        Self { excluded_at: now }
    }
}
