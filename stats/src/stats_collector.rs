use alloc::{
    sync::Arc,
    boxed::Box,
    vec::Vec,
};
use hashbrown::HashMap;
use spin::{Mutex, MutexGuard, RwLock};

use crate::Direction;

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct StatsKey {
    pub stat: &'static str,
    pub detail: &'static str,
    pub dir: Direction,
}

impl StatsKey {
    pub const fn new(stat: &'static str, detail: &'static str, dir: Direction) -> Self {
        Self { stat, detail, dir }
    }
}

pub struct StatsCollection(HashMap<StatsKey, u64>);

impl StatsCollection {
    pub fn new() -> Self {
        Self(Default::default())
    }

    pub fn get(&self, stat: &'static str, detail: &'static str) -> u64 {
        Self::get_dir(&self, stat, detail, Direction::In)
    }

    pub fn get_dir(&self, stat: &'static str, detail: &'static str, dir: Direction) -> u64 {
        let key = StatsKey { stat, detail, dir };
        self.0.get(&key).cloned().unwrap_or_default()
    }

    pub fn insert(&mut self, stat: &'static str, detail: &'static str, value: impl TryInto<u64>) {
        self.insert_dir(stat, detail, Direction::In, value);
    }

    pub fn insert_dir(
        &mut self,
        stat: &'static str,
        detail: &'static str,
        dir: Direction,
        value: impl TryInto<u64>,
    ) {
        let key = StatsKey { stat, detail, dir };
        self.0.insert(key, value.try_into().unwrap_or_default());
    }

    pub fn iter(&self) -> impl Iterator<Item = (&StatsKey, &u64)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains(&self, stat: &'static str, detail: &'static str, dir: Direction) -> bool {
        let key = StatsKey { stat, detail, dir };
        self.0.contains_key(&key)
    }
}

impl Default for StatsCollection {
    fn default() -> Self {
        Self::new()
    }
}

pub trait StatsSource {
    fn collect_stats(&self, result: &mut StatsCollection);
}

impl<T> StatsSource for Arc<T>
where
    T: StatsSource,
{
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.as_ref().collect_stats(result)
    }
}

impl<T> StatsSource for Arc<Mutex<T>>
where
    T: StatsSource,
{
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.lock().collect_stats(result)
    }
}

impl<T> StatsSource for Arc<RwLock<T>>
where
    T: StatsSource,
{
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.read().collect_stats(result)
    }
}

pub struct StatsCollector {
    stats: Mutex<StatsCollection>,
    sources: Vec<Box<dyn StatsSource + Send + Sync>>,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self {
            stats: Mutex::new(StatsCollection::new()),
            sources: Vec::new(),
        }
    }

    pub fn add_source(&mut self, source: impl StatsSource + Send + Sync + 'static) {
        self.sources.push(Box::new(source));
    }

    pub fn collect(&self) -> MutexGuard<StatsCollection> {
        let mut stats = self.stats.lock();
        for source in &self.sources {
            source.collect_stats(&mut stats);
        }
        stats
    }
}
