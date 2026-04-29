use std::time::Duration;

pub struct Timestamp(i128);

pub struct SteadyClock;

impl SteadyClock {
    pub fn new_null_with(now: Timestamp) -> Self {
        Self::new_null_with_offsets([Duration::from_nanos(now.0 as u64)])
    }

    pub fn new_null_with_offsets(offsets: impl IntoIterator<Item = Duration>) -> Self {
        SteadyClock
    }
}

fn main() {}
