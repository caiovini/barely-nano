#![no_std]

extern crate alloc;

mod cpu_work_generator;
mod work_pool;
mod work_queue;
mod work_thread;
mod work_thresholds;
mod xorshift;

pub(crate) use cpu_work_generator::CpuWorkGenerator;
pub(crate) use work_pool::WorkGenerator;
pub use work_pool::{WorkPool, WorkPoolBuilder};
pub use work_queue::WorkTicket;
pub(crate) use work_queue::{WorkItem, WorkQueueCoordinator};
pub(crate) use work_thread::WorkThread;
pub use work_thresholds::{dev_difficulty, WorkThresholds, WORK_THRESHOLDS_STUB};
pub(crate) use xorshift::XorShift1024Star;

pub(crate) trait WorkRng {
    fn next_work(&mut self) -> u64;
}
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OpenClConfig {
    pub platform: u32,
    pub device: u32,
    pub threads: u32,
}
