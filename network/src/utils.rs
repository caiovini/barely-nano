use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};
use core::future::Future;
use core::pin::Pin;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub fn into_core_addr(addr: rsnano_core::utils::SocketAddrV6) -> SocketAddrV6 {
    addr
}

pub fn into_core_ipv6(addr: rsnano_core::utils::Ipv6Addr) -> Ipv6Addr {
    addr
}

pub fn from_core_addr(addr: SocketAddrV6) -> rsnano_core::utils::SocketAddrV6 {
    addr
}

pub fn from_core_ipv6(addr: Ipv6Addr) -> rsnano_core::utils::Ipv6Addr {
    addr
}

pub fn is_ipv4_mapped(input: &Ipv6Addr) -> bool {
    matches!(
        input.octets(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, _, _, _, _]
    )
}

pub fn map_address_to_subnetwork(input: &Ipv6Addr) -> Ipv6Addr {
    const IPV6_SUBNET_PREFIX_LENGTH: u8 = 32; // Equivalent to network prefix /32.
    const IPV4_SUBNET_PREFIX_LENGTH: u8 = (128 - 32) + 24; // Limits for /24 IPv4 subnetwork (we're using mapped IPv4 to IPv6 addresses, hence (128 - 32))
    if is_ipv4_mapped(input) {
        first_ipv6_subnet_address(input, IPV4_SUBNET_PREFIX_LENGTH)
    } else {
        first_ipv6_subnet_address(input, IPV6_SUBNET_PREFIX_LENGTH)
    }
}

pub fn ipv4_address_or_ipv6_subnet(input: &Ipv6Addr) -> Ipv6Addr {
    if is_ipv4_mapped(input) {
        *input
    } else {
        // Assuming /48 subnet prefix for IPv6 as it's relatively easy to acquire such a /48 address range
        first_ipv6_subnet_address(input, 48)
    }
}

fn first_ipv6_subnet_address(input: &Ipv6Addr, prefix_bits: u8) -> Ipv6Addr {
    fill_remaining_bits(input, prefix_bits, 0)
}

fn fill_remaining_bits(input: &Ipv6Addr, prefix_bits: u8, filler: u8) -> Ipv6Addr {
    debug_assert_eq!(prefix_bits % 8, 0);
    let index = (prefix_bits / 8) as usize;
    let mut octets = input.octets();
    octets[index..].fill(filler);
    Ipv6Addr::from(octets)
}

pub fn is_ipv4_or_v4_mapped_address(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(_) => true,
        IpAddr::V6(ip) => is_ipv4_mapped(ip),
    }
}

pub fn into_ipv6_socket_address(input: SocketAddr) -> SocketAddrV6 {
    match input {
        SocketAddr::V4(a) => SocketAddrV6::new(a.ip().to_ipv6_mapped(), a.port(), 0, 0),
        SocketAddr::V6(a) => a,
    }
}

pub fn into_ipv6_address(input: IpAddr) -> Ipv6Addr {
    match input {
        IpAddr::V4(ip) => ip.to_ipv6_mapped(),
        IpAddr::V6(ip) => ip,
    }
}

pub fn reserved_address(endpoint: &SocketAddrV6, allow_local_peers: bool) -> bool {
    const RFC1700_MIN: Ipv6Addr = Ipv4Addr::new(0, 0, 0, 0).to_ipv6_mapped();
    const RFC1700_MAX: Ipv6Addr = Ipv4Addr::new(0, 0xff, 0xff, 0xff).to_ipv6_mapped();
    const RFC1918_1_MIN: Ipv6Addr = Ipv4Addr::new(0x0a, 0, 0, 0).to_ipv6_mapped();
    const RFC1918_1_MAX: Ipv6Addr = Ipv4Addr::new(0x0a, 0xff, 0xff, 0xff).to_ipv6_mapped();
    const RFC1918_2_MIN: Ipv6Addr = Ipv4Addr::new(0xac, 0x10, 0x00, 0x00).to_ipv6_mapped();
    const RFC1918_2_MAX: Ipv6Addr = Ipv4Addr::new(0xac, 0x1f, 0xff, 0xff).to_ipv6_mapped();
    const RFC1918_3_MIN: Ipv6Addr = Ipv4Addr::new(0xc0, 0xa8, 0x00, 0x00).to_ipv6_mapped();
    const RFC1918_3_MAX: Ipv6Addr = Ipv4Addr::new(0xc0, 0xa8, 0xff, 0xff).to_ipv6_mapped();
    const RFC6598_MIN: Ipv6Addr = Ipv4Addr::new(0x64, 0x40, 0x00, 0x00).to_ipv6_mapped();
    const RFC6598_MAX: Ipv6Addr = Ipv4Addr::new(0x64, 0x7f, 0xff, 0xff).to_ipv6_mapped();
    const RFC5737_1_MIN: Ipv6Addr = Ipv4Addr::new(0xc0, 0x00, 0x02, 0x00).to_ipv6_mapped();
    const RFC5737_1_MAX: Ipv6Addr = Ipv4Addr::new(0xc0, 0x00, 0x02, 0xff).to_ipv6_mapped();
    const RFC5737_2_MIN: Ipv6Addr = Ipv4Addr::new(0xc6, 0x33, 0x64, 0x00).to_ipv6_mapped();
    const RFC5737_2_MAX: Ipv6Addr = Ipv4Addr::new(0xc6, 0x33, 0x64, 0xff).to_ipv6_mapped();
    const RFC5737_3_MIN: Ipv6Addr = Ipv4Addr::new(0xcb, 0x00, 0x71, 0x00).to_ipv6_mapped();
    const RFC5737_3_MAX: Ipv6Addr = Ipv4Addr::new(0xcb, 0x00, 0x71, 0xff).to_ipv6_mapped();
    const IPV4_MULTICAST_MIN: Ipv6Addr = Ipv4Addr::new(0xe0, 0x00, 0x00, 0x00).to_ipv6_mapped();
    const IPV4_MULTICAST_MAX: Ipv6Addr = Ipv4Addr::new(0xef, 0xff, 0xff, 0xff).to_ipv6_mapped();
    const RFC6890_MIN: Ipv6Addr = Ipv4Addr::new(0xf0, 0x00, 0x00, 0x00).to_ipv6_mapped();
    const RFC6890_MAX: Ipv6Addr = Ipv4Addr::new(0xff, 0xff, 0xff, 0xff).to_ipv6_mapped();

    const RFC6666_MIN: Ipv6Addr = Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0);
    const RFC6666_MAX: Ipv6Addr = Ipv6Addr::new(0x100u16, 0, 0, 0, 0xffff, 0xffff, 0xffff, 0xffff);
    const RFC3849_MIN: Ipv6Addr = Ipv6Addr::new(0x2001u16, 0xdb8, 0, 0, 0, 0, 0, 0);
    const RFC3849_MAX: Ipv6Addr = Ipv6Addr::new(
        0x2001u16, 0xdb8, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    );
    const RFC4193_MIN: Ipv6Addr = Ipv6Addr::new(0xfc00u16, 0, 0, 0, 0, 0, 0, 0);
    const RFC4193_MAX: Ipv6Addr = Ipv6Addr::new(
        0xfd00u16, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    );
    const IPV6_MULTICAST_MIN: Ipv6Addr = Ipv6Addr::new(0xff00u16, 0, 0, 0, 0, 0, 0, 0);
    const IPV6_MULTICAST_MAX: Ipv6Addr = Ipv6Addr::new(
        0xff00u16, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    );

    if endpoint.port() == 0 {
        return true;
    }

    let ip = *endpoint.ip();
    if (ip >= RFC1700_MIN && ip <= RFC1700_MAX)
        || (ip >= RFC5737_1_MIN && ip <= RFC5737_1_MAX)
        || (ip >= RFC5737_2_MIN && ip <= RFC5737_2_MAX)
        || (ip >= RFC5737_3_MIN && ip <= RFC5737_3_MAX)
        || (ip >= IPV4_MULTICAST_MIN && ip <= IPV4_MULTICAST_MAX)
        || (ip >= RFC6890_MIN && ip <= RFC6890_MAX)
        || (ip >= RFC6666_MIN && ip <= RFC6666_MAX)
        || (ip >= RFC3849_MIN && ip <= RFC3849_MAX)
        || (ip >= IPV6_MULTICAST_MIN && ip <= IPV6_MULTICAST_MAX)
    {
        return true;
    }

    if !allow_local_peers {
        if (ip >= RFC1918_1_MIN && ip <= RFC1918_1_MAX)
            || (ip >= RFC1918_2_MIN && ip <= RFC1918_2_MAX)
            || (ip >= RFC1918_3_MIN && ip <= RFC1918_3_MAX)
            || (ip >= RFC6598_MIN && ip <= RFC6598_MAX)
            || (ip >= RFC4193_MIN && ip <= RFC4193_MAX)
        {
            return true;
        }
    }

    false
}

#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<TokenInner>,
}

struct TokenInner {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(TokenInner {
                cancelled: AtomicBool::new(false),
                wakers: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            let mut wakers = self.inner.wakers.lock();
            for waker in wakers.drain(..) {
                waker.wake();
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        WaitForCancellationFuture { token: self }
    }
}

pub struct WaitForCancellationFuture<'a> {
    token: &'a CancellationToken,
}

impl<'a> Future for WaitForCancellationFuture<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.token.is_cancelled() {
            Poll::Ready(())
        } else {
            let mut wakers = self.token.inner.wakers.lock();
            if self.token.is_cancelled() {
                Poll::Ready(())
            } else {
                wakers.push(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
