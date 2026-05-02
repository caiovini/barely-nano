use alloc::{
    sync::Arc,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicU64, Ordering, AtomicU32},
    time::Duration,
};
use spin::{RwLock, Mutex};
use rsnano_nullable_clock::{Timestamp, SteadyClock};

use anyhow::Result;
use tracing::debug;

use rsnano_core::utils::get_env_bool;

use crate::{
    DetailType, Direction, Sample, StatType, StatsCollection, StatsConfig, StatsLogSink,
    StatsSource,
};

const STATS_MAP_SIZE: usize = 4096;
const MAX_SAMPLES_PER_TYPE: usize = 16;

pub struct Stats {
    config: StatsConfig,
    mutables: Arc<StatMutables>,
    enable_logging: bool,
    clock: SteadyClock,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new(StatsConfig::default())
    }
}

impl Stats {
    pub fn new(config: StatsConfig) -> Self {
        let clock = SteadyClock::default();
        Self {
            config: config.clone(),
            mutables: Arc::new(StatMutables::new(clock.now())),
            enable_logging: get_env_bool("NANO_LOG_STATS").unwrap_or(false),
            clock,
        }
    }

    pub fn add(&self, stat_type: StatType, detail: DetailType, value: u64) {
        self.add_dir(stat_type, detail, Direction::In, value)
    }

    pub fn add_dir(&self, stat_type: StatType, detail: DetailType, dir: Direction, value: u64) {
        if value == 0 { return; }
        self.log_add(stat_type, detail, dir, value);
        
        self.mutables.add(stat_type, detail, dir, value);

        let all_key = DetailType::All;
        if detail != all_key {
            self.mutables.add(stat_type, all_key, dir, value);
        }
    }

    fn log_add(&self, stat_type: StatType, detail: DetailType, dir: Direction, value: u64) {
        if self.enable_logging {
            debug!("Stat: {:?}::{:?}::{:?} += {}", stat_type, detail, dir, value);
        }
    }

    pub fn add_dir_aggregate(&self, stat_type: StatType, detail: DetailType, dir: Direction, value: u64) {
        self.add_dir(stat_type, detail, dir, value)
    }

    pub fn inc(&self, stat_type: StatType, detail: DetailType) {
        self.add_dir(stat_type, detail, Direction::In, 1)
    }

    pub fn inc_dir(&self, stat_type: StatType, detail: DetailType, dir: Direction) {
        self.add_dir(stat_type, detail, dir, 1)
    }

    pub fn inc_dir_aggregate(&self, stat_type: StatType, detail: DetailType, dir: Direction) {
        self.add_dir_aggregate(stat_type, detail, dir, 1)
    }

    pub fn sample(&self, sample: Sample, value: i64, _expected_min_max: (i64, i64)) {
        self.log_sample(sample, value);
        let mut guard = self.mutables.samples.lock();
        let samples = &mut guard[sample as usize];
        if samples.len() >= MAX_SAMPLES_PER_TYPE {
            samples.remove(0);
        }
        samples.push(value);
    }

    fn log_sample(&self, sample: Sample, value: i64) {
        if self.enable_logging {
            debug!("Sample: {:?} -> {}", sample, value);
        }
    }

    pub fn samples(&self, sample: Sample) -> Vec<i64> {
        let guard = self.mutables.samples.lock();
        guard[sample as usize].clone()
    }

    pub fn log_counters(&self, sink: &mut dyn StatsLogSink) -> Result<()> {
        let now = self.clock.now();
        sink.begin()?;
        
        let mut entries = Vec::new();
        for i in 0..STATS_MAP_SIZE {
            let key_val = self.mutables.keys[i].load(Ordering::Relaxed);
            if key_val != 0 {
                let val = self.mutables.values[i].load(Ordering::Relaxed);
                if val > 0 {
                    let key = CounterKey::from_u32(key_val);
                    entries.push((key, val));
                }
            }
        }
        
        entries.sort_by_key(|e| e.0);

        for (key, val) in entries {
            sink.write_counter_entry(now, key.stat_type.as_str(), key.detail.as_str(), key.dir.as_str(), val)?;
        }
        
        sink.finalize();
        Ok(())
    }

    pub fn log_samples(&self, sink: &mut dyn StatsLogSink) -> Result<()> {
        let now = self.clock.now();
        sink.begin()?;
        let guard = self.mutables.samples.lock();
        for (i, samples) in guard.iter().enumerate() {
            if !samples.is_empty() {
                let sample_type: Sample = unsafe { core::mem::transmute(i as u8) };
                sink.write_sampler_entry(now, sample_type.as_str(), samples.clone(), (0, 0))?;
            }
        }
        sink.finalize();
        Ok(())
    }

    pub fn last_reset(&self) -> Duration {
        let last = self.mutables.timestamp.load(Ordering::Relaxed);
        Duration::from_millis(self.clock.now().millis() as u64 - last as u64)
    }

    pub fn clear(&self) {
        for i in 0..STATS_MAP_SIZE {
            self.mutables.keys[i].store(0, Ordering::Relaxed);
            self.mutables.values[i].store(0, Ordering::Relaxed);
        }
        let mut samples = self.mutables.samples.lock();
        for s in samples.iter_mut() {
            s.clear();
        }
        self.mutables.timestamp.store(self.clock.now().millis() as u64, Ordering::Relaxed);
    }

    pub fn count_all(&self, stat_type: StatType, dir: Direction) -> u64 {
        let mut result = 0;
        for i in 0..STATS_MAP_SIZE {
            let key_val = self.mutables.keys[i].load(Ordering::Relaxed);
            if key_val != 0 {
                let key = CounterKey::from_u32(key_val);
                if key.stat_type == stat_type && key.dir == dir && key.detail != DetailType::All {
                    result += self.mutables.values[i].load(Ordering::Relaxed);
                }
            }
        }
        result
    }

    pub fn count(&self, stat_type: StatType, detail: DetailType, dir: Direction) -> u64 {
        let key = CounterKey::new(stat_type, detail, dir);
        self.mutables.get_value(key)
    }

    pub fn flush_to_db(&self) -> Result<()> {
        let mut updates = Vec::new();
        for i in 0..STATS_MAP_SIZE {
            let key_val = self.mutables.keys[i].load(Ordering::Relaxed);
            if key_val != 0 {
                let val = self.mutables.values[i].load(Ordering::Relaxed);
                if val > 0 {
                    updates.push((CounterKey::from_u32(key_val), val));
                }
            }
        }

        if updates.is_empty() { return Ok(()); }

        kernel_common::uart::print(b"[stats] Bulk flushing ");
        kernel_common::uart::print_usize(updates.len());
        kernel_common::uart::print(b" counters...\n");

        let mut db_guard = kernel_common::storage::DB.lock();
        if let Some(db) = db_guard.as_mut() {
            for (key, val) in updates {
                let mut db_key = [0u8; 65];
                db_key[0] = 1; // Stats table
                db_key[1] = key.stat_type as u8;
                let detail_bytes = (key.detail as u16).to_le_bytes();
                db_key[2] = detail_bytes[0];
                db_key[3] = detail_bytes[1];
                db_key[4] = key.dir as u8;

                let mut db_val = [0u8; 256];
                db_val[0..8].copy_from_slice(&val.to_le_bytes());
                db.insert(db_key, db_val);
            }
            db.flush().map_err(|e| anyhow::anyhow!("DB flush failed: {:?}", e))?;
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
struct CounterKey {
    stat_type: StatType,
    detail: DetailType,
    dir: Direction,
}

impl CounterKey {
    fn new(stat_type: StatType, detail: DetailType, dir: Direction) -> Self {
        Self { stat_type, detail, dir }
    }

    fn to_u32(&self) -> u32 {
        let s = self.stat_type as u32;
        let d = self.detail as u32;
        let r = self.dir as u32;
        (s << 24) | (d << 8) | r | 0x80000000 // Set MSB to ensure non-zero
    }

    fn from_u32(val: u32) -> Self {
        let s = ((val >> 24) & 0x7F) as u8;
        let d = ((val >> 8) & 0xFFFF) as u16;
        let r = (val & 0xFF) as u8;
        Self {
            stat_type: unsafe { core::mem::transmute(s) },
            detail: unsafe { core::mem::transmute(d) },
            dir: unsafe { core::mem::transmute(r) },
        }
    }

    fn hash(&self) -> usize {
        let val = self.to_u32();
        ((val ^ (val >> 16)) as usize) % STATS_MAP_SIZE
    }
}

struct StatMutables {
    keys: [AtomicU32; STATS_MAP_SIZE],
    values: [AtomicU64; STATS_MAP_SIZE],
    samples: Mutex<[Vec<i64>; 8]>, // Fixed size array of samples (8 is enough for Sample enum)
    timestamp: AtomicU64,
}

impl StatMutables {
    fn new(now: Timestamp) -> Self {
        let keys = unsafe { core::mem::transmute([0u32; STATS_MAP_SIZE]) };
        let values = unsafe { core::mem::transmute([0u64; STATS_MAP_SIZE]) };
        Self {
            keys,
            values,
            samples: Mutex::new(Default::default()),
            timestamp: AtomicU64::new(now.millis() as u64),
        }
    }

    fn add(&self, stat_type: StatType, detail: DetailType, dir: Direction, value: u64) {
        let key = CounterKey::new(stat_type, detail, dir);
        let key_u32 = key.to_u32();
        let mut idx = key.hash();
        
        loop {
            let current_key = self.keys[idx].load(Ordering::Relaxed);
            if current_key == key_u32 {
                self.values[idx].fetch_add(value, Ordering::Relaxed);
                return;
            }
            if current_key == 0 {
                if self.keys[idx].compare_exchange(0, key_u32, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                    self.values[idx].fetch_add(value, Ordering::Relaxed);
                    return;
                }
                continue;
            }
            idx = (idx + 1) % STATS_MAP_SIZE;
        }
    }

    fn get_value(&self, key: CounterKey) -> u64 {
        let key_u32 = key.to_u32();
        let mut idx = key.hash();
        let start_idx = idx;
        
        loop {
            let current_key = self.keys[idx].load(Ordering::Relaxed);
            if current_key == key_u32 {
                return self.values[idx].load(Ordering::Relaxed);
            }
            if current_key == 0 {
                return 0;
            }
            idx = (idx + 1) % STATS_MAP_SIZE;
            if idx == start_idx { return 0; }
        }
    }
}

impl StatsSource for Stats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        for i in 0..STATS_MAP_SIZE {
            let key_val = self.mutables.keys[i].load(Ordering::Relaxed);
            if key_val != 0 {
                let key = CounterKey::from_u32(key_val);
                let val = self.mutables.values[i].load(Ordering::Relaxed);
                result.insert_dir(key.stat_type.as_str(), key.detail.as_str(), key.dir, val);
            }
        }
    }
}
