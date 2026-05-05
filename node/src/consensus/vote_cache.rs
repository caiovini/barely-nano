use alloc::vec::Vec;
use alloc::sync::Arc;
use hashbrown::{HashMap, HashSet};
use core::fmt::Debug;
use core::time::Duration;

use rsnano_nullable_clock::Timestamp as Instant;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Amount, BlockHash, Vote, VoteError,
};
use rsnano_stats::Stats;

type FxHashMap<K, V> = HashMap<K, V>;
type FxHashSet<T> = HashSet<T>;

#[derive(Clone, Debug, PartialEq)]
pub struct VoteCacheConfig {
    pub max_size: usize,
    pub max_voters: usize,
    pub age_cutoff: Duration,
}

impl Default for VoteCacheConfig {
    fn default() -> Self {
        Self {
            max_size: 4096,
            max_voters: 64,
            age_cutoff: Duration::from_secs(15 * 60),
        }
    }
}

pub struct VoteCache {
    config: VoteCacheConfig,
    cache: CacheEntryCollection,
    next_id: usize,
    last_cleanup: Instant,
    stats: Arc<Stats>,
}

impl VoteCache {
    pub fn find(&self, _hash: &BlockHash) -> Vec<Arc<Vote>> {
        Vec::new()
    }
}

pub struct CacheEntryCollection {}
impl Default for CacheEntryCollection {
    fn default() -> Self {
        Self {}
    }
}
impl CacheEntryCollection {
    pub fn contains(&self, _hash: &BlockHash) -> bool { false }
    pub fn insert(&mut self, _entry: CacheEntry) {}
    pub fn modify_by_hash<F>(&mut self, _hash: &BlockHash, _f: F) -> bool { false }
    pub fn len(&self) -> usize { 0 }
    pub fn pop_front(&mut self) -> Option<CacheEntry> { None }
    pub fn get_by_hash(&self, _hash: &BlockHash) -> Option<&CacheEntry> { None }
    pub fn remove_by_hash(&mut self, _hash: &BlockHash) -> Option<CacheEntry> { None }
    pub fn clear(&mut self) {}
    pub fn iter(&self) -> impl Iterator<Item = &CacheEntry> { core::iter::empty() }
    pub fn iter_by_tally_desc(&self) -> impl Iterator<Item = &CacheEntry> { core::iter::empty() }
    pub fn is_empty(&self) -> bool { true }
}

pub struct CacheEntry {
    pub hash: BlockHash,
    pub last_vote: Instant,
}
impl CacheEntry {
    pub fn new(_id: usize, hash: BlockHash) -> Self {
        Self { hash, last_vote: Instant::new_null() }
    }
    pub fn vote(&mut self, _vote: &Arc<Vote>, _rep_weight: Amount, _max_voters: usize) {}
    pub fn votes(&self) -> Vec<Arc<Vote>> { Vec::new() }
    pub fn tally(&self) -> Amount { Amount::zero() }
    pub fn final_tally(&self) -> Amount { Amount::zero() }
}

pub struct TopEntry {
    pub hash: BlockHash,
    pub tally: Amount,
    pub final_tally: Amount,
}

impl VoteCache {
    pub fn new(config: VoteCacheConfig, stats: Arc<Stats>) -> Self {
        VoteCache {
            last_cleanup: Instant::new_null(),
            config,
            cache: CacheEntryCollection::default(),
            next_id: 0,
            stats,
        }
    }

    pub fn insert(&mut self, _vote: &Arc<Vote>, _rep_weight: Amount, _results: &HashMap<BlockHash, Result<(), VoteError>>) {}
    pub fn contains(&self, _hash: &BlockHash) -> bool { false }
    pub fn size(&self) -> usize { 0 }
    pub fn empty(&self) -> bool { true }
    pub fn erase(&mut self, _hash: &BlockHash) -> bool { false }
    pub fn clear(&mut self) {}
    pub fn top(&mut self, _min_tally: impl Into<Amount>) -> Vec<TopEntry> { Vec::new() }
}

impl ContainerInfoProvider for VoteCache {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder().finish()
    }
}
