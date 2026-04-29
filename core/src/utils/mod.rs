mod backperssure_channel;
mod cancellation_token;
mod container_info;
mod fair_queue;
mod peer;
mod stream;

pub use backperssure_channel::*;
pub use cancellation_token::CancellationToken;
use chrono::{DateTime, TimeZone, Utc};
pub use container_info::*;
pub use fair_queue::*;
pub use peer::*;
pub use stream::*;

use crate::Amount;
use core::{
    ops::{Add, Mul},
    time::Duration,
};
use alloc::sync::Arc;
use spin::Mutex;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use alloc::string::String;
use alloc::format;
use alloc::borrow::ToOwned;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Ipv6Addr([u8; 16]);

impl Ipv6Addr {
    pub const UNSPECIFIED: Self = Ipv6Addr([0; 16]);
    pub const fn new(a: u16, b: u16, c: u16, d: u16, e: u16, f: u16, g: u16, h: u16) -> Self {
        Ipv6Addr([
            (a >> 8) as u8, a as u8,
            (b >> 8) as u8, b as u8,
            (c >> 8) as u8, c as u8,
            (d >> 8) as u8, d as u8,
            (e >> 8) as u8, e as u8,
            (f >> 8) as u8, f as u8,
            (g >> 8) as u8, g as u8,
            (h >> 8) as u8, h as u8,
        ])
    }

    pub const fn octets(&self) -> [u8; 16] {
        self.0
    }

    pub const fn new_from_octets(octets: [u8; 16]) -> Self {
        Self(octets)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SocketAddrV6 {
    addr: Ipv6Addr,
    port: u16,
}

impl SocketAddrV6 {
    pub const fn new(addr: Ipv6Addr, port: u16, _flowinfo: u32, _scope_id: u32) -> Self {
        Self { addr, port }
    }

    pub const fn ip(&self) -> Ipv6Addr {
        self.addr
    }

    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl core::str::FromStr for SocketAddrV6 {
    type Err = anyhow::Error;
    fn from_str(_s: &str) -> Result<Self, Self::Err> {
        // Placeholder
        Ok(Self::default())
    }
}

pub trait Serialize {
    fn serialize(&self, stream: &mut dyn BufferWriter);
}

pub trait FixedSizeSerialize: Serialize {
    fn serialized_size() -> usize;
}

pub trait Deserialize {
    type Target;
    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self::Target>;
}

impl Serialize for u64 {
    fn serialize(&self, stream: &mut dyn BufferWriter) {
        stream.write_u64_be_safe(*self)
    }
}

impl FixedSizeSerialize for u64 {
    fn serialized_size() -> usize {
        core::mem::size_of::<u64>()
    }
}

impl Deserialize for u64 {
    type Target = Self;
    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<u64> {
        stream.read_u64_be()
    }
}

impl Serialize for [u8; 64] {
    fn serialize(&self, stream: &mut dyn BufferWriter) {
        stream.write_bytes_safe(self)
    }
}

impl FixedSizeSerialize for [u8; 64] {
    fn serialized_size() -> usize {
        64
    }
}

impl Deserialize for [u8; 64] {
    type Target = Self;

    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self::Target> {
        let mut buffer = [0; 64];
        stream.read_bytes(&mut buffer, 64)?;
        Ok(buffer)
    }
}

pub fn get_cpu_count() -> usize {
    1
}

pub type MemoryIntensiveInstrumentationCallback = extern "C" fn() -> bool;

pub fn milliseconds_since_epoch() -> u64 {
    SteadyClock::default().now().millis() as u64
}

pub fn system_time_as_seconds(time: Timestamp) -> u64 {
    (time.millis() / 1000) as u64
}

/// Elapsed seconds since UNIX_EPOCH
#[derive(PartialEq, Eq, Clone, Copy, PartialOrd, Ord, Default, Hash)]
pub struct UnixTimestamp(u64);

impl UnixTimestamp {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    pub const fn new(seconds_since_epoch: u64) -> Self {
        Self(seconds_since_epoch)
    }

    pub const fn new_test_instance() -> Self {
        Self::new(1740000000)
    }

    pub fn now() -> Self {
        Self(Self::seconds_since_unix_epoch())
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    fn seconds_since_unix_epoch() -> u64 {
        (SteadyClock::default().now().millis() / 1000) as u64
    }

    pub fn to_be_bytes(&self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_be_bytes(bytes))
    }

    pub fn add(&self, seconds: u64) -> Self {
        Self(self.0 + seconds)
    }

    pub fn utc(&self) -> DateTime<Utc> {
        Utc.timestamp_opt(self.0 as i64, 0)
            .latest()
            .unwrap_or_default()
    }
}

impl From<u64> for UnixTimestamp {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl Add<Duration> for UnixTimestamp {
    type Output = UnixTimestamp;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0 + rhs.as_secs())
    }
}

impl Mul<u64> for UnixTimestamp {
    type Output = UnixTimestamp;

    fn mul(self, rhs: u64) -> Self::Output {
        UnixTimestamp::new(self.0 * rhs)
    }
}

impl TryFrom<Timestamp> for UnixTimestamp {
    type Error = core::convert::Infallible;

    fn try_from(value: Timestamp) -> Result<Self, Self::Error> {
        Ok(Self((value.millis() / 1000) as u64))
    }
}

impl core::fmt::Display for UnixTimestamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

impl core::fmt::Debug for UnixTimestamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

/// Elapsed milliseconds since UNIX_EPOCH
#[derive(PartialEq, Eq, Clone, Copy, PartialOrd, Ord, Default, Hash)]
pub struct UnixMillisTimestamp(u64);

impl UnixMillisTimestamp {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    pub const fn new(millis_since_epoch: u64) -> Self {
        Self(millis_since_epoch)
    }

    pub const fn new_test_instance() -> Self {
        Self::new(1740000000000)
    }

    pub fn now() -> Self {
        Self(milliseconds_since_epoch())
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn to_be_bytes(&self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_be_bytes(bytes))
    }

    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0
            .checked_add(duration.as_millis() as u64)
            .map(|i| Self(i))
    }

    pub fn elapsed(&self, now: UnixMillisTimestamp) -> Duration {
        Duration::from_millis(now.0.checked_sub(self.0).unwrap_or(0))
    }

    pub fn utc(&self) -> DateTime<Utc> {
        let seconds = (self.0 / 1000) as i64;
        let millis = (self.0 % 1000) as u32;
        let nanos = millis * 1000 * 1000;
        Utc.timestamp_opt(seconds, nanos)
            .latest()
            .unwrap_or_default()
    }
}

impl From<u64> for UnixMillisTimestamp {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<UnixMillisTimestamp> for UnixTimestamp {
    fn from(value: UnixMillisTimestamp) -> Self {
        Self::new(value.0 / 1000)
    }
}

impl From<UnixTimestamp> for UnixMillisTimestamp {
    fn from(value: UnixTimestamp) -> Self {
        Self::new(value.0 * 1000)
    }
}

impl Add<Duration> for UnixMillisTimestamp {
    type Output = UnixMillisTimestamp;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0 + rhs.as_millis() as u64)
    }
}

impl Mul<u64> for UnixMillisTimestamp {
    type Output = UnixMillisTimestamp;

    fn mul(self, rhs: u64) -> Self::Output {
        UnixMillisTimestamp::new(self.0 * rhs)
    }
}

impl core::fmt::Display for UnixMillisTimestamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

impl core::fmt::Debug for UnixMillisTimestamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

pub fn get_env_or_default<T>(_variable_name: &str, default: T) -> T
where
    T: core::str::FromStr + Copy,
{
    default
}

pub fn get_env_or_default_string(_variable_name: &str, default: impl Into<String>) -> String {
    default.into()
}

pub fn get_env_bool(_variable_name: impl AsRef<str>) -> Option<bool> {
    None
}

pub fn parse_endpoint(s: &str) -> SocketAddrV6 {
    s.parse().unwrap()
}

pub const NULL_ENDPOINT: SocketAddrV6 = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);

pub const TEST_ENDPOINT_1: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 1), 1111, 0, 0);

pub const TEST_ENDPOINT_2: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 2), 2222, 0, 0);

pub const TEST_ENDPOINT_3: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 3), 3333, 0, 0);

pub fn new_test_timestamp() -> Timestamp {
    Timestamp::new_test_instance()
}

#[derive(Clone)]
pub struct OneShotNotification<T>(Arc<Mutex<(bool, Option<T>)>>);
 
impl<T> OneShotNotification<T> {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new((false, None))))
    }
 
    pub fn notify(&self, t: T) {
        *self.0.lock() = (true, Some(t));
    }
 
    pub fn cancel(&self) {
        *self.0.lock() = (true, None);
    }
 
    pub fn wait(&self) -> Option<T> {
        loop {
            let mut guard = self.0.lock();
            if guard.0 {
                return guard.1.take();
            }
            core::hint::spin_loop();
        }
    }
}

pub trait Tickable: Send {
    fn tick(&mut self, cancel_token: &CancellationToken);
}

/// Lower timestamps have a higher priority
#[derive(PartialEq, Eq, Copy, Clone, Hash)]
pub struct TimePriority(UnixMillisTimestamp);

impl TimePriority {
    // highest timestamp means lowest priority!
    pub const MIN: TimePriority = TimePriority::new(u64::MAX);

    pub const fn new(timestamp: u64) -> Self {
        Self(UnixMillisTimestamp::new(timestamp))
    }
}

impl Default for TimePriority {
    fn default() -> Self {
        Self::MIN
    }
}

impl Ord for TimePriority {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other.0.cmp(&self.0)
    }
}

impl PartialOrd for TimePriority {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Debug for TimePriority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<UnixMillisTimestamp> for TimePriority {
    fn from(value: UnixMillisTimestamp) -> Self {
        Self(value)
    }
}

impl From<TimePriority> for UnixMillisTimestamp {
    fn from(value: TimePriority) -> Self {
        value.0
    }
}

#[derive(PartialEq, Eq, Copy, Clone, Default, PartialOrd, Ord, Debug, Hash)]
pub struct BlockPriority {
    pub balance: Amount,
    pub time: TimePriority,
}

impl BlockPriority {
    pub const MIN: BlockPriority = BlockPriority::new(Amount::zero(), TimePriority::MIN);

    pub const fn new(balance: Amount, time: TimePriority) -> Self {
        Self { balance, time }
    }

    pub fn new_test_instance() -> Self {
        Self::new(Amount::nano(1), TimePriority::new(42))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_priority_order() {
        let a = BlockPriority::new(Amount::from(100), TimePriority::new(5));
        let b = BlockPriority::new(Amount::from(100), TimePriority::new(6));
        let c = BlockPriority::new(Amount::from(101), TimePriority::new(4));
        assert!(a > b);
        assert!(c > a);
    }
}
