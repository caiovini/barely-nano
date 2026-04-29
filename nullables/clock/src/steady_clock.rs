use alloc::collections::VecDeque;
use core::{
    ops::{Add, AddAssign, Sub},
    time::Duration,
};
use spin::Mutex;

pub struct SteadyClock {
    time_source: TimeSource,
}

impl SteadyClock {
    pub fn new_null() -> Self {
        let mut offsets = VecDeque::new();
        offsets.push_back(DEFAULT_STUB_DURATION);
        Self {
            time_source: TimeSource::Stub(Mutex::new(offsets)),
        }
    }

    pub fn new_null_with(now: Timestamp) -> Self {
        let mut nows = VecDeque::new();
        nows.push_back(now.0);
        Self {
            time_source: TimeSource::Stub(Mutex::new(nows)),
        }
    }

    pub fn new_null_with_offsets(offsets: impl IntoIterator<Item = Duration>) -> Self {
        let mut last = DEFAULT_STUB_DURATION;
        let mut nows = VecDeque::new();
        nows.push_back(last);
        for offset in offsets.into_iter() {
            let now = last + offset.as_nanos() as i128;
            nows.push_back(now);
            last = now;
        }
        Self {
            time_source: TimeSource::Stub(Mutex::new(nows)),
        }
    }

    pub fn now(&self) -> Timestamp {
        Timestamp(self.time_source.now())
    }

    pub fn advance(&self, step: Duration) {
        match &self.time_source {
            TimeSource::Hardware => panic!("Only a nulled clock can be advanced!"),
            TimeSource::Stub(mutex) => {
                let mut times = mutex.lock();
                if times.len() != 1 {
                    panic!("Cannot advance because other configured responses exist!")
                }
                let val = times.pop_front().unwrap();
                times.push_back(val + step.as_nanos() as i128);
            }
        }
    }
}

impl Default for SteadyClock {
    fn default() -> Self {
        SteadyClock {
            time_source: TimeSource::Hardware,
        }
    }
}

enum TimeSource {
    Hardware,
    Stub(Mutex<VecDeque<i128>>),
}

impl TimeSource {
    fn now(&self) -> i128 {
        match self {
            TimeSource::Hardware => {
                // CLINT is 10 MHz, so 1 tick = 100 ns
                (kernel_common::clint::mtime() as i128) * 100
            }
            TimeSource::Stub(nows) => {
                let mut guard = nows.lock();
                if guard.len() == 1 {
                    *guard.front().unwrap()
                } else {
                    guard.pop_front().unwrap()
                }
            }
        }
    }
}

const DEFAULT_STUB_DURATION: i128 = 1000i128 * 1000 * 1000 * 60 * 60 * 24 * 365;

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Default, Hash)]
pub struct Timestamp(i128);

impl Timestamp {
    pub const MAX: Self = Self(i128::MAX);

    pub const fn new(nanos: i128) -> Self {
        Self(nanos)
    }

    pub const fn new_test_instance() -> Self {
        Self(DEFAULT_STUB_DURATION)
    }

    pub fn new_null() -> Self {
        Self(DEFAULT_STUB_DURATION)
    }

    pub fn elapsed(&self, now: Timestamp) -> Duration {
        Duration::from_nanos(now.0.checked_sub(self.0).unwrap_or_default() as u64)
    }

    pub fn checked_sub(&self, rhs: Duration) -> Option<Self> {
        self.0.checked_sub(rhs.as_nanos() as i128).map(Self)
    }

    pub fn millis(&self) -> i64 {
        (self.0 / 1_000_000) as i64
    }

    pub fn as_secs(&self) -> u64 {
        (self.0 / 1_000_000_000) as u64
    }

    pub const DEFAULT_STUB_NOW: Timestamp = Timestamp(DEFAULT_STUB_DURATION);
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0.add(rhs.as_nanos() as i128))
    }
}

impl AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs
    }
}

impl Sub<Timestamp> for Timestamp {
    type Output = Duration;

    fn sub(self, rhs: Timestamp) -> Self::Output {
        let diff = self.0 - rhs.0;
        if diff < 0 {
            Duration::from_nanos(0)
        } else {
            Duration::from_nanos(diff as u64)
        }
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, rhs: Duration) -> Self::Output {
        Self(self.0 - rhs.as_nanos() as i128)
    }
}
