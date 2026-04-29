use core::time::Duration;
use core::ops::{Add, AddAssign, Sub, SubAssign};

/// A bare-metal compatible replacement for `std::time::SystemTime`
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SystemTime(Duration);

impl SystemTime {
    pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::from_secs(0));

    pub fn now() -> Self {
        // We don't have a real-time clock (RTC) driver yet, so we just return
        // the uptime since the device booted based on the CLINT timer.
        // For real network synchronization, an NTP client or RTC driver is required.
        let ticks = kernel_common::clint::mtime();
        let nanos = ticks * 100;
        SystemTime(Duration::from_nanos(nanos))
    }

    pub fn duration_since(&self, earlier: SystemTime) -> Result<Duration, ()> {
        if self.0 >= earlier.0 {
            Ok(self.0 - earlier.0)
        } else {
            Err(())
        }
    }
}

impl Add<Duration> for SystemTime {
    type Output = SystemTime;
    fn add(self, rhs: Duration) -> SystemTime {
        SystemTime(self.0 + rhs)
    }
}

impl Sub<Duration> for SystemTime {
    type Output = SystemTime;
    fn sub(self, rhs: Duration) -> SystemTime {
        SystemTime(self.0 - rhs)
    }
}

pub struct SystemTimeFactory(TimeStrategy);

impl SystemTimeFactory {
    pub fn new_null() -> Self {
        Self(TimeStrategy::Stub(
            SystemTime::UNIX_EPOCH + Duration::from_secs(60 * 60 * 24 * 365 * 50),
        ))
    }

    pub fn new_null_with(configured_response: SystemTime) -> Self {
        Self(TimeStrategy::Stub(configured_response))
    }

    pub fn now(&self) -> SystemTime {
        match &self.0 {
            TimeStrategy::Real => SystemTime::now(),
            TimeStrategy::Stub(now) => *now,
        }
    }
}

impl Default for SystemTimeFactory {
    fn default() -> Self {
        Self(TimeStrategy::Real)
    }
}

enum TimeStrategy {
    Real,
    Stub(SystemTime),
}
