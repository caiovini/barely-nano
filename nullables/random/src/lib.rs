#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use rand::{CryptoRng, RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use spin::Mutex;

static GLOBAL_RNG: Mutex<Option<ChaCha8Rng>> = Mutex::new(None);

pub fn set_seed(seed: u64) {
    let mut lock = GLOBAL_RNG.lock();
    *lock = Some(ChaCha8Rng::seed_from_u64(seed));
}

pub struct NullableRng {
    strategy: RngStrategy,
}

impl NullableRng {
    pub fn new_null() -> Self {
        Self::new_null_u64(42)
    }

    pub fn new_null_u64(val: u64) -> Self {
        Self {
            strategy: RngStrategy::Nulled(RngStub::new(val.to_be_bytes().to_vec())),
        }
    }

    pub fn new_null_bytes(bytes: &[u8]) -> Self {
        Self {
            strategy: RngStrategy::Nulled(RngStub::new(bytes.to_vec())),
        }
    }

    pub fn rng() -> Self {
        Self {
            strategy: RngStrategy::Real,
        }
    }

    fn get_global_rng() -> u32 {
        let mut lock = GLOBAL_RNG.lock();
        if lock.is_none() {
            *lock = Some(ChaCha8Rng::seed_from_u64(0xDEADBEEF));
        }
        lock.as_mut().unwrap().next_u32()
    }

    fn fill_global_bytes(dest: &mut [u8]) {
        let mut lock = GLOBAL_RNG.lock();
        if lock.is_none() {
            *lock = Some(ChaCha8Rng::seed_from_u64(0xDEADBEEF));
        }
        lock.as_mut().unwrap().fill_bytes(dest);
    }
}

impl RngCore for NullableRng {
    fn next_u32(&mut self) -> u32 {
        match &mut self.strategy {
            RngStrategy::Real => Self::get_global_rng(),
            RngStrategy::Nulled(i) => i.next_u32(),
        }
    }

    fn next_u64(&mut self) -> u64 {
        match &mut self.strategy {
            RngStrategy::Real => {
                let mut buf = [0u8; 8];
                Self::fill_global_bytes(&mut buf);
                u64::from_be_bytes(buf)
            }
            RngStrategy::Nulled(i) => i.next_u64(),
        }
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        match &mut self.strategy {
            RngStrategy::Real => Self::fill_global_bytes(dest),
            RngStrategy::Nulled(i) => i.fill_bytes(dest),
        }
    }
}

impl rand_core_06::RngCore for NullableRng {
    fn next_u32(&mut self) -> u32 {
        RngCore::next_u32(self)
    }

    fn next_u64(&mut self) -> u64 {
        RngCore::next_u64(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        RngCore::fill_bytes(self, dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> core::result::Result<(), rand_core_06::Error> {
        RngCore::fill_bytes(self, dest);
        Ok(())
    }
}

impl CryptoRng for NullableRng {}

enum RngStrategy {
    Real,
    Nulled(RngStub),
}

struct RngStub {
    data: Vec<u8>,
    index: usize,
}

impl RngStub {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, index: 0 }
    }

    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        self.fill_bytes(&mut buf);
        u32::from_be_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill_bytes(&mut buf);
        u64::from_be_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for i in dest {
            *i = self.data[self.index];
            self.index += 1;
            if self.index >= self.data.len() {
                self.index = 0;
            }
        }
    }
}
