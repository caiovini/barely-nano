use core::net::{Ipv6Addr, SocketAddrV6};
use spin::Mutex;
use core::time::Duration;
use hashbrown::HashMap;
use alloc::vec::Vec;

use anyhow::Result;
use rand::RngCore;
use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, Signature,
};
use rsnano_messages::Cookie;
use rsnano_nullable_clock::{Timestamp, SteadyClock};
use alloc::sync::Arc;

/// Node ID cookies for node ID handshakes
pub struct SynCookies {
    data: Mutex<LockedSynCookies>,
    max_cookies_per_ip: usize,
    clock: Arc<SteadyClock>,
}

impl SynCookies {
    pub fn new(max_cookies_per_ip: usize, clock: Arc<SteadyClock>) -> Self {
        Self {
            data: Mutex::new(LockedSynCookies {
                cookies: HashMap::new(),
                cookies_per_ip: HashMap::new(),
            }),
            max_cookies_per_ip,
            clock,
        }
    }

    /// Returns `None` if the IP is rate capped on syn cookie requests,
    /// or if the endpoint already has a syn cookie query
    pub fn assign(&self, endpoint: &SocketAddrV6) -> Option<Cookie> {
        let ip_addr = endpoint.ip();
        let mut lock = self.data.lock();

        if lock.cookies.contains_key(endpoint) {
            return None;
        }

        let ip_cookies = lock.cookies_per_ip.entry(*ip_addr).or_default();
        if *ip_cookies < self.max_cookies_per_ip {
            *ip_cookies += 1;
            let mut cookie = [0u8; 32];
            rsnano_nullable_random::NullableRng::rng().fill_bytes(&mut cookie);
            lock.cookies.insert(
                *endpoint,
                SynCookieInfo {
                    cookie,
                    created_at: self.clock.now(),
                },
            );
            
            Some(cookie)
        } else {
            None
        }
    }

    // Returns `false` if invalid, `true` if valid
    // Also removes the syn cookie from the store if valid
    pub fn validate(
        &self,
        endpoint: &SocketAddrV6,
        node_id: &Account,
        signature: &Signature,
    ) -> Result<()> {
        let ip_addr = endpoint.ip();
        let mut lock = self.data.lock();
        if let Some(info) = lock.cookies.get(endpoint) {
            node_id.as_key().verify(&info.cookie, signature)?;
            lock.cookies.remove(endpoint);
            lock.dec_cookie_count(*ip_addr);
        }
        Ok(())
    }

    pub fn purge(&self, cutoff: Duration) {
        let mut lock = self.data.lock();
        let now = self.clock.now();
        
        let mut removed_endpoints = Vec::new();
        for (endpoint, info) in lock.cookies.iter() {
            if now - info.created_at > cutoff {
                removed_endpoints.push(*endpoint);
            }
        }

        for endpoint in &removed_endpoints {
            lock.cookies.remove(endpoint);
            lock.dec_cookie_count(*endpoint.ip());
        }
    }

    pub fn cookie(&self, endpoint: &SocketAddrV6) -> Option<Cookie> {
        let ip_addr = endpoint.ip();
        let mut lock = self.data.lock();

        let info = lock.cookies.remove(endpoint);
        if info.is_some() {
            lock.dec_cookie_count(*ip_addr);
        }
        info.map(|i| i.cookie)
    }

    pub fn cookies_count(&self) -> usize {
        self.data.lock().cookies.len()
    }

    pub fn cookies_per_ip_count(&self) -> usize {
        self.data.lock().cookies_per_ip.len()
    }

    pub fn cookie_info_size() -> usize {
        core::mem::size_of::<SynCookieInfo>()
    }

    pub fn cookies_per_ip_size() -> usize {
        core::mem::size_of::<usize>()
    }
}

impl Default for SynCookies {
    fn default() -> Self {
        Self::new(10, Arc::new(SteadyClock::default()))
    }
}

impl ContainerInfoProvider for SynCookies {
    fn container_info(&self) -> ContainerInfo {
        [
            (
                "syn_cookies",
                self.cookies_count(),
                Self::cookie_info_size(),
            ),
            (
                "syn_cookies_per_ip",
                self.cookies_per_ip_count(),
                Self::cookies_per_ip_size(),
            ),
        ]
        .into()
    }
}
struct LockedSynCookies {
    cookies: HashMap<SocketAddrV6, SynCookieInfo>,
    cookies_per_ip: HashMap<Ipv6Addr, usize>,
}

impl LockedSynCookies {
    fn dec_cookie_count(&mut self, ip_addr: Ipv6Addr) {
        let ip_cookies = self.cookies_per_ip.entry(ip_addr).or_default();
        if *ip_cookies > 0 {
            *ip_cookies -= 1;
        }
    }
}

struct SynCookieInfo {
    cookie: Cookie,
    created_at: Timestamp,
}
