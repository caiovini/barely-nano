use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::{fmt::Debug, mem::size_of, time::Duration};
use hashbrown::HashMap;

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Amount, BlockHash, DescTallyKey, PublicKey, Vote, VoteError,
};
use rsnano_nullable_clock::Timestamp as Instant;
use rsnano_stats::{DetailType, StatType, Stats};

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

#[derive(Default)]
pub struct CacheEntryCollection {
    by_hash: HashMap<BlockHash, CacheEntry>,
    by_id: BTreeMap<usize, BlockHash>,
    by_tally: BTreeSet<(DescTallyKey, DescTallyKey, BlockHash)>,
}

impl CacheEntryCollection {
    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub fn insert(&mut self, entry: CacheEntry) {
        let hash = entry.hash;
        let id = entry.id;
        let tally_key = Self::tally_key(&entry);
        if let Some(old) = self.by_hash.insert(hash, entry) {
            self.by_id.remove(&old.id);
            self.by_tally.remove(&Self::tally_key(&old));
        }
        self.by_id.insert(id, hash);
        self.by_tally.insert(tally_key);
    }

    pub fn modify_by_hash<F>(&mut self, hash: &BlockHash, f: F) -> bool
    where
        F: FnOnce(&mut CacheEntry),
    {
        let Some(entry) = self.by_hash.get_mut(hash) else {
            return false;
        };
        let old_key = Self::tally_key(entry);
        self.by_tally.remove(&old_key);
        f(entry);
        let new_key = Self::tally_key(entry);
        self.by_tally.insert(new_key);
        true
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    pub fn pop_front(&mut self) -> Option<CacheEntry> {
        let (_, hash) = self.by_id.pop_first()?;
        let entry = self.by_hash.remove(&hash)?;
        self.by_tally.remove(&Self::tally_key(&entry));
        Some(entry)
    }

    pub fn get_by_hash(&self, hash: &BlockHash) -> Option<&CacheEntry> {
        self.by_hash.get(hash)
    }

    pub fn remove_by_hash(&mut self, hash: &BlockHash) -> Option<CacheEntry> {
        let entry = self.by_hash.remove(hash)?;
        self.by_id.remove(&entry.id);
        self.by_tally.remove(&Self::tally_key(&entry));
        Some(entry)
    }

    pub fn clear(&mut self) {
        self.by_hash.clear();
        self.by_id.clear();
        self.by_tally.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &CacheEntry> {
        self.by_hash.values()
    }

    pub fn iter_by_tally_desc(&self) -> impl Iterator<Item = &CacheEntry> {
        self.by_tally
            .iter()
            .filter_map(|(_, _, hash)| self.by_hash.get(hash))
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }

    #[inline]
    fn tally_key(entry: &CacheEntry) -> (DescTallyKey, DescTallyKey, BlockHash) {
        (
            DescTallyKey(entry.tally),
            DescTallyKey(entry.final_tally),
            entry.hash,
        )
    }
}

#[derive(Clone)]
struct VoterEntry {
    representative: PublicKey,
    timestamp: rsnano_core::utils::UnixMillisTimestamp,
    vote: Arc<Vote>,
    weight: Amount,
}

pub struct CacheEntry {
    pub id: usize,
    pub hash: BlockHash,
    voters: Vec<VoterEntry>,
    tally: Amount,
    final_tally: Amount,
    pub last_vote: Instant,
}

impl CacheEntry {
    pub fn new(id: usize, hash: BlockHash) -> Self {
        Self {
            id,
            hash,
            voters: Vec::new(),
            tally: Amount::zero(),
            final_tally: Amount::zero(),
            last_vote: Instant::now(),
        }
    }

    pub fn vote(&mut self, vote: &Arc<Vote>, rep_weight: Amount, max_voters: usize) -> bool {
        let updated = self.add_vote(vote, rep_weight, max_voters);
        if updated {
            self.recalculate_tally();
            self.last_vote = Instant::now();
        }
        updated
    }

    fn add_vote(&mut self, vote: &Arc<Vote>, rep_weight: Amount, max_voters: usize) -> bool {
        let representative = vote.voter;
        if let Some(existing) = self
            .voters
            .iter_mut()
            .find(|v| v.representative == representative)
        {
            // We already have a vote from this rep; update if newer timestamp (final votes have u64::MAX)
            if vote.timestamp() > existing.timestamp {
                existing.timestamp = vote.timestamp();
                existing.vote = vote.clone();
                existing.weight = rep_weight;
                true
            } else {
                false
            }
        } else if self.voters.len() < max_voters {
            self.voters.push(VoterEntry {
                representative,
                timestamp: vote.timestamp(),
                vote: vote.clone(),
                weight: rep_weight,
            });
            true
        } else {
            // Buffer is full: replace the lowest-weight voter if the new voter has strictly higher weight
            if let Some(min_entry) = self.voters.iter_mut().min_by_key(|v| v.weight) {
                if min_entry.weight < rep_weight {
                    *min_entry = VoterEntry {
                        representative,
                        timestamp: vote.timestamp(),
                        vote: vote.clone(),
                        weight: rep_weight,
                    };
                    return true;
                }
            }
            false
        }
    }

    fn recalculate_tally(&mut self) {
        let mut tally = Amount::zero();
        let mut final_tally = Amount::zero();
        for voter in &self.voters {
            tally = tally.wrapping_add(voter.weight);
            if voter.vote.is_final() {
                final_tally = final_tally.wrapping_add(voter.weight);
            }
        }
        self.tally = tally;
        self.final_tally = final_tally;
    }

    pub fn votes(&self) -> Vec<Arc<Vote>> {
        self.voters.iter().map(|i| i.vote.clone()).collect()
    }

    pub fn tally(&self) -> Amount {
        self.tally
    }

    pub fn final_tally(&self) -> Amount {
        self.final_tally
    }

    pub fn size(&self) -> usize {
        self.voters.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopEntry {
    pub hash: BlockHash,
    pub tally: Amount,
    pub final_tally: Amount,
}

impl VoteCache {
    pub fn new(config: VoteCacheConfig, stats: Arc<Stats>) -> Self {
        VoteCache {
            last_cleanup: Instant::now(),
            config,
            cache: CacheEntryCollection::default(),
            next_id: 0,
            stats,
        }
    }

    pub fn insert(
        &mut self,
        vote: &Arc<Vote>,
        rep_weight: Amount,
        results: &HashMap<BlockHash, Result<(), VoteError>>,
    ) {
        if rep_weight.is_zero() || self.config.max_size == 0 || self.config.max_voters == 0 {
            return;
        }

        // Cache votes with a matching hash that wasn't processed by an active election
        for hash in &vote.hashes {
            let should_cache = match results.get(hash) {
                None => results.is_empty(),
                Some(Err(VoteError::Indeterminate)) => true,
                _ => false,
            };
            if should_cache {
                self.insert_impl(vote, *hash, rep_weight);
            }
        }
    }

    fn insert_impl(&mut self, vote: &Arc<Vote>, hash: BlockHash, rep_weight: Amount) {
        self.stats.inc(StatType::VoteCache, DetailType::Insert);
        let max_voters = self.config.max_voters;

        if self.cache.contains(&hash) {
            self.cache.modify_by_hash(&hash, |existing| {
                existing.vote(vote, rep_weight, max_voters);
            });
        } else {
            self.trim_overflow();
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            let mut cache_entry = CacheEntry::new(id, hash);
            cache_entry.vote(vote, rep_weight, max_voters);
            self.cache.insert(cache_entry);
        }
    }

    fn trim_overflow(&mut self) {
        while self.cache.len() >= self.config.max_size {
            self.cache.pop_front();
            self.stats.inc(StatType::VoteCache, DetailType::Cleanup);
        }
    }

    pub fn find(&self, hash: &BlockHash) -> Vec<Arc<Vote>> {
        match self.cache.get_by_hash(hash) {
            Some(entry) => entry.votes(),
            None => Vec::new(),
        }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.cache.contains(hash)
    }

    pub fn size(&self) -> usize {
        self.cache.len()
    }

    pub fn empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn erase(&mut self, hash: &BlockHash) -> bool {
        self.cache.remove_by_hash(hash).is_some()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub fn top(&mut self, min_tally: impl Into<Amount>) -> Vec<TopEntry> {
        self.stats.inc(StatType::VoteCache, DetailType::Top);
        let min_tally = min_tally.into();
        let now = Instant::now();

        if self.last_cleanup.elapsed(now) >= self.config.age_cutoff / 2 {
            self.cleanup(now);
            self.last_cleanup = now;
        }

        let age_cutoff = self.config.age_cutoff;
        let mut results = Vec::new();
        let mut stale_hashes = Vec::new();

        for entry in self.cache.iter_by_tally_desc() {
            let tally = entry.tally();
            if tally < min_tally {
                break;
            }
            if entry.last_vote.elapsed(now) >= age_cutoff {
                stale_hashes.push(entry.hash);
            } else {
                results.push(TopEntry {
                    hash: entry.hash,
                    tally,
                    final_tally: entry.final_tally(),
                });
            }
        }

        for hash in stale_hashes {
            self.cache.remove_by_hash(&hash);
            self.stats.inc(StatType::VoteCache, DetailType::Cleanup);
        }

        // Secondary sort by final_tally descending, then tally descending (matching RSNano)
        results.sort_by(|a, b| {
            let res = b.final_tally.cmp(&a.final_tally);
            if res == core::cmp::Ordering::Equal {
                b.tally.cmp(&a.tally)
            } else {
                res
            }
        });

        results
    }

    fn cleanup(&mut self, now: Instant) {
        let age_cutoff = self.config.age_cutoff;
        let stale: Vec<BlockHash> = self
            .cache
            .iter()
            .filter(|entry| entry.last_vote.elapsed(now) >= age_cutoff)
            .map(|entry| entry.hash)
            .collect();

        for hash in stale {
            self.cache.remove_by_hash(&hash);
            self.stats.inc(StatType::VoteCache, DetailType::Cleanup);
        }
    }
}

impl ContainerInfoProvider for VoteCache {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf("cache", self.cache.len(), size_of::<CacheEntry>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use rsnano_core::{utils::UnixMillisTimestamp, PrivateKey};

    fn create_vote(key: &PrivateKey, timestamp: u64, hashes: Vec<BlockHash>) -> Arc<Vote> {
        Arc::new(Vote::new(
            key,
            UnixMillisTimestamp::new(timestamp),
            0,
            hashes,
        ))
    }

    #[test]
    fn insert_and_find() {
        let mut cache = VoteCache::new(Default::default(), Arc::new(Stats::default()));
        let key = PrivateKey::from(1);
        let hash = BlockHash::from(100);
        let vote = create_vote(&key, 1000, vec![hash]);

        assert!(cache.empty());
        assert_eq!(cache.size(), 0);

        cache.insert(&vote, Amount::nano(1000), &HashMap::new());

        assert!(!cache.empty());
        assert_eq!(cache.size(), 1);
        assert!(cache.contains(&hash));

        let found = cache.find(&hash);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].voter, key.public_key());

        let tops = cache.top(Amount::nano(500));
        assert_eq!(tops.len(), 1);
        assert_eq!(tops[0].hash, hash);
        assert_eq!(tops[0].tally, Amount::nano(1000));
        assert_eq!(tops[0].final_tally, Amount::zero());
    }

    #[test]
    fn ignore_active_election_votes() {
        let mut cache = VoteCache::new(Default::default(), Arc::new(Stats::default()));
        let key = PrivateKey::from(1);
        let hash1 = BlockHash::from(100);
        let hash2 = BlockHash::from(200);
        let vote = create_vote(&key, 1000, vec![hash1, hash2]);

        let mut results = HashMap::new();
        results.insert(hash1, Ok(()));
        results.insert(hash2, Err(VoteError::Indeterminate));

        cache.insert(&vote, Amount::nano(1000), &results);

        assert!(!cache.contains(&hash1));
        assert!(cache.contains(&hash2));
    }

    #[test]
    fn max_voters_replaces_lowest_weight() {
        let config = VoteCacheConfig {
            max_size: 16,
            max_voters: 2,
            age_cutoff: Duration::from_secs(60),
        };
        let mut cache = VoteCache::new(config, Arc::new(Stats::default()));
        let hash = BlockHash::from(42);

        let key1 = PrivateKey::from(1);
        let key2 = PrivateKey::from(2);
        let key3 = PrivateKey::from(3);

        cache.insert(
            &create_vote(&key1, 1000, vec![hash]),
            Amount::nano(100),
            &HashMap::new(),
        );
        cache.insert(
            &create_vote(&key2, 1000, vec![hash]),
            Amount::nano(200),
            &HashMap::new(),
        );
        // Higher weight replaces key1 (100)
        cache.insert(
            &create_vote(&key3, 1000, vec![hash]),
            Amount::nano(300),
            &HashMap::new(),
        );

        let tops = cache.top(Amount::zero());
        assert_eq!(tops.len(), 1);
        assert_eq!(tops[0].tally, Amount::nano(500));
        assert_eq!(cache.find(&hash).len(), 2);
    }

    #[test]
    fn max_size_evicts_oldest() {
        let config = VoteCacheConfig {
            max_size: 2,
            max_voters: 4,
            age_cutoff: Duration::from_secs(60),
        };
        let mut cache = VoteCache::new(config, Arc::new(Stats::default()));
        let key = PrivateKey::from(1);

        let h1 = BlockHash::from(1);
        let h2 = BlockHash::from(2);
        let h3 = BlockHash::from(3);

        cache.insert(&create_vote(&key, 1000, vec![h1]), Amount::nano(100), &HashMap::new());
        cache.insert(&create_vote(&key, 1000, vec![h2]), Amount::nano(100), &HashMap::new());
        cache.insert(&create_vote(&key, 1000, vec![h3]), Amount::nano(100), &HashMap::new());

        assert_eq!(cache.size(), 2);
        assert!(!cache.contains(&h1));
        assert!(cache.contains(&h2));
        assert!(cache.contains(&h3));
    }
}
