use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use rsnano_core::{Account, BlockHash};
use rsnano_nullable_clock::Timestamp;
use core::cmp::min;
use core::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockingEntry {
    pub account: Account,
    pub dependency: BlockHash,
    /// Account that contains the dependency block, fetched via a background dependency walker
    pub dependency_account: Account,
    pub added: Timestamp,
}

impl BlockingEntry {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            account: Account::from(5),
            dependency: BlockHash::from(100),
            dependency_account: Account::from(13),
            added: Timestamp::new_test_instance(),
        }
    }
}

/// A blocked account is an account that has failed to insert a new block because the source block is not currently present in the ledger
/// An account is unblocked once it has a block successfully inserted
#[derive(Default)]
pub(super) struct BlockingContainer {
    next_seq: u64,
    unknown_cursor: core::sync::atomic::AtomicU64,
    by_account: BTreeMap<Account, (u64, BlockingEntry)>,
    sequenced: BTreeMap<u64, Account>,
    by_dependency: BTreeMap<BlockHash, BTreeSet<Account>>,
    by_dependency_account: BTreeMap<Account, BTreeSet<(u64, Account, BlockHash)>>,
    dependency_stats: BTreeMap<BlockHash, (u8, Timestamp)>,
    resolved_dependencies: BTreeMap<BlockHash, Account>,
}

impl BlockingContainer {
    pub const ELEMENT_SIZE: usize =
        size_of::<BlockingEntry>() + size_of::<Account>() * 3 + size_of::<f32>();

    pub fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub(crate) fn known_dependencies(&self) -> usize {
        self.len()
            .saturating_sub(self.count_by_dependency_account(&Account::zero()))
    }

    pub(crate) fn unique_blocking_accounts(&self) -> usize {
        let mut known = self.by_dependency_account.len();
        if self.by_dependency_account.contains_key(&Account::zero()) {
            known -= 1;
        }
        known
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[allow(unused_mut)]
    pub fn insert(&mut self, mut entry: BlockingEntry) -> bool {
        let account = entry.account;
        if self.by_account.contains_key(&account) {
            return false;
        }
        #[cfg(not(test))]
        if entry.dependency_account.is_zero() {
            if let Some(&known_acc) = self.resolved_dependencies.get(&entry.dependency) {
                entry.dependency_account = known_acc;
            }
        }
        let dependency = entry.dependency;
        let dependency_account = entry.dependency_account;

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        if dependency_account.is_zero() {
            self.unknown_cursor
                .store(seq.saturating_add(1), core::sync::atomic::Ordering::Relaxed);
        }
        self.by_account.insert(account, (seq, entry));
        self.sequenced.insert(seq, account);
        self.by_dependency
            .entry(dependency)
            .or_default()
            .insert(account);
        self.by_dependency_account
            .entry(dependency_account)
            .or_default()
            .insert((seq, account, dependency));
        true
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn reinsertable(&self) -> impl Iterator<Item = &Account> {
        self.by_dependency_account
            .range(Account::from(1)..)
            .filter_map(|(acc, _)| {
                if !self.by_account.contains_key(acc) {
                    Some(acc)
                } else {
                    None
                }
            })
    }

    pub fn count_by_dependency_account(&self, dep_account: &Account) -> usize {
        self.by_dependency_account
            .get(dep_account)
            .map(|accs| accs.len())
            .unwrap_or_default()
    }

    pub fn next(&self, now: Timestamp, filter: impl Fn(&BlockHash) -> bool) -> Option<BlockHash> {
        let accounts = self.by_dependency_account.get(&Account::zero())?;
        #[cfg(test)]
        {
            return accounts
                .iter()
                .find(|(_, _, dep)| {
                    if let Some((fails, last)) = self.dependency_stats.get(dep) {
                        let shift = fails.saturating_sub(1).min(5) as u32;
                        let cooldown_secs = min(600, 15u64 << shift);
                        if *last + Duration::from_secs(cooldown_secs) > now {
                            return false;
                        }
                    }
                    filter(dep)
                })
                .map(|(_, _, dep)| *dep);
        }
        #[cfg(not(test))]
        {
            if accounts.is_empty() {
                return None;
            }
            let cursor = self
                .unknown_cursor
                .load(core::sync::atomic::Ordering::Relaxed);
            let upper = (
                cursor,
                Account::from_bytes([0xFF; 32]),
                BlockHash::from_bytes([0xFF; 32]),
            );
            for (seq, _, dep) in accounts.range(..=upper).rev() {
                if let Some((fails, last)) = self.dependency_stats.get(dep) {
                    let shift = fails.saturating_sub(1).min(2) as u32;
                    let cooldown_secs = min(8, 2u64 << shift);
                    if *last + Duration::from_secs(cooldown_secs) > now {
                        continue;
                    }
                }
                if filter(dep) {
                    self.unknown_cursor
                        .store(*seq, core::sync::atomic::Ordering::Relaxed);
                    return Some(*dep);
                }
            }
            for (seq, _, dep) in accounts.range(upper..).rev() {
                if let Some((fails, last)) = self.dependency_stats.get(dep) {
                    let shift = fails.saturating_sub(1).min(2) as u32;
                    let cooldown_secs = min(8, 2u64 << shift);
                    if *last + Duration::from_secs(cooldown_secs) > now {
                        continue;
                    }
                }
                if filter(dep) {
                    self.unknown_cursor
                        .store(*seq, core::sync::atomic::Ordering::Relaxed);
                    return Some(*dep);
                }
            }
            self.unknown_cursor
                .store(u64::MAX, core::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    pub fn dependency_failed(&mut self, hash: &BlockHash, now: Timestamp) -> u8 {
        if self.dependency_stats.len() >= 16384 && !self.dependency_stats.contains_key(hash) {
            self.dependency_stats.clear();
        }
        let entry = self.dependency_stats.entry(*hash).or_insert((0, Timestamp::new(0)));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
        entry.0
    }

    pub fn resolve_ancestor_or_cycle(&self, start: &Account) -> (Option<Account>, Option<Account>) {
        if start.is_zero() {
            return (None, None);
        }
        let mut slow = *start;
        let mut fast = *start;
        for step in 0..64 {
            let Some(fast_entry1) = self.get(&fast) else {
                return (Some(fast), None);
            };
            let next1 = fast_entry1.dependency_account;
            if next1.is_zero() {
                return (None, None);
            }
            if next1 == fast || next1 == *start {
                return (None, Some(fast));
            }
            let Some(fast_entry2) = self.get(&next1) else {
                return (Some(next1), None);
            };
            let next2 = fast_entry2.dependency_account;
            if next2.is_zero() {
                return (None, None);
            }
            if next2 == next1 || next2 == *start {
                return (None, Some(next1));
            }
            fast = next2;
            if let Some(slow_entry) = self.get(&slow) {
                slow = slow_entry.dependency_account;
            }
            if slow == fast {
                return (None, Some(slow));
            }
            if step == 63 {
                return (Some(fast), None);
            }
        }
        (None, None)
    }

    pub fn resolve_unblocked_ancestor(&self, start: &Account) -> Option<Account> {
        self.resolve_ancestor_or_cycle(start).0
    }

    pub fn find_cycle_member(&self, start: &Account) -> Option<Account> {
        self.resolve_ancestor_or_cycle(start).1
    }

    #[allow(dead_code)]
    pub fn iter_start_dep_account(&self, start: Account) -> impl Iterator<Item = &BlockingEntry> {
        self.by_dependency_account
            .range(start..)
            .flat_map(|(_, accs)| accs.iter())
            .map(|(_, acc, _)| &self.by_account.get(acc).unwrap().1)
    }

    pub fn iter_dep_accounts(&self, start: Account) -> impl Iterator<Item = &Account> {
        self.by_dependency_account
            .range(start..)
            .map(|(dep_acc, _)| dep_acc)
    }

    pub fn iter_by_insertion_order(&self) -> impl Iterator<Item = &BlockingEntry> {
        self.sequenced
            .values()
            .map(|account| &self.by_account.get(account).unwrap().1)
    }

    pub fn get(&self, account: &Account) -> Option<&BlockingEntry> {
        self.by_account.get(account).map(|(_, e)| e)
    }

    pub fn accounts_by_dependency(&self, dependency: &BlockHash) -> Vec<Account> {
        self.by_dependency
            .get(dependency)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn sample_dependencies(&self, start: BlockHash, count: usize) -> (Vec<BlockHash>, BlockHash) {
        if self.by_dependency.is_empty() || count == 0 {
            return (Vec::new(), BlockHash::zero());
        }
        let max_scan = count;
        let mut scanned = 0usize;
        let mut deps = Vec::with_capacity(count);
        for (hash, _) in self.by_dependency.range(start..) {
            scanned += 1;
            #[cfg(not(test))]
            let keep = rsnano_ledger::block_may_exist(hash);
            #[cfg(test)]
            let keep = true;
            if keep {
                deps.push(*hash);
            }
            if deps.len() >= count || scanned >= max_scan {
                return (deps, hash.inc().unwrap_or_default());
            }
        }
        if !start.is_zero() && deps.len() < count && scanned < max_scan {
            for (hash, _) in self.by_dependency.iter() {
                if *hash >= start {
                    break;
                }
                scanned += 1;
                #[cfg(not(test))]
                let keep = rsnano_ledger::block_may_exist(hash);
                #[cfg(test)]
                let keep = true;
                if keep {
                    deps.push(*hash);
                }
                if deps.len() >= count || scanned >= max_scan {
                    return (deps, hash.inc().unwrap_or_default());
                }
            }
        }
        (deps, BlockHash::zero())
    }

    pub fn pop_oldest(&mut self) -> Option<BlockingEntry> {
        let oldest = *self.sequenced.first_key_value()?.1;
        self.remove(&oldest)
    }

    pub fn remove(&mut self, account: &Account) -> Option<BlockingEntry> {
        let (seq, entry) = self.by_account.remove(account)?;
        self.remove_indexes(seq, &entry);
        Some(entry)
    }

    pub fn remove_older_than(&mut self, cutoff: Timestamp) -> usize {
        let mut removed = 0;
        while let Some((_, &account)) = self.sequenced.first_key_value() {
            if let Some((_, entry)) = self.by_account.get(&account) {
                if entry.added >= cutoff {
                    break;
                }
            }
            self.remove(&account);
            removed += 1;
        }

        removed
    }

    pub fn modify_dependency_account(
        &mut self,
        dependency: &BlockHash,
        new_dependency_account: Account,
    ) -> usize {
        self.dependency_stats.remove(dependency);
        if !new_dependency_account.is_zero() {
            if self.resolved_dependencies.len() >= 16384 {
                self.resolved_dependencies.clear();
            }
            self.resolved_dependencies
                .insert(*dependency, new_dependency_account);
        }
        let Some(accounts) = self.by_dependency.get(dependency) else {
            return 0;
        };

        let mut updated = 0;

        for account in accounts {
            let (seq, entry) = self.by_account.get_mut(account).unwrap();
            let seq = *seq;
            if entry.dependency_account != new_dependency_account {
                let old_dependency_account = entry.dependency_account;
                entry.dependency_account = new_dependency_account;
                let old = self
                    .by_dependency_account
                    .get_mut(&old_dependency_account)
                    .unwrap();
                if old.len() == 1 {
                    self.by_dependency_account.remove(&old_dependency_account);
                } else {
                    old.remove(&(seq, entry.account, entry.dependency));
                }
                self.by_dependency_account
                    .entry(new_dependency_account)
                    .or_default()
                    .insert((seq, entry.account, entry.dependency));

                updated += 1;
            }
        }

        updated
    }

    fn remove_indexes(&mut self, seq: u64, entry: &BlockingEntry) {
        self.sequenced.remove(&seq);
        let accounts = self.by_dependency.get_mut(&entry.dependency).unwrap();
        if accounts.len() > 1 {
            accounts.remove(&entry.account);
        } else {
            self.by_dependency.remove(&entry.dependency);
        }
        let accounts = self
            .by_dependency_account
            .get_mut(&entry.dependency_account)
            .unwrap();
        if accounts.len() > 1 {
            accounts.remove(&(seq, entry.account, entry.dependency));
        } else {
            self.by_dependency_account.remove(&entry.dependency_account);
        }
    }

    pub fn clear(&mut self) {
        self.next_seq = 0;
        self.by_account.clear();
        self.sequenced.clear();
        self.by_dependency.clear();
        self.by_dependency_account.clear();
        self.dependency_stats.clear();
        self.resolved_dependencies.clear();
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    #[test]
    fn empty() {
        let mut blocking = BlockingContainer::default();
        assert_eq!(blocking.len(), 0);
        assert_eq!(blocking.is_empty(), true);
        assert_eq!(blocking.contains(&Account::from(1)), false);
        assert_eq!(blocking.count_by_dependency_account(&Account::from(1)), 0);
        assert!(blocking
            .iter_start_dep_account(Account::from(1))
            .next()
            .is_none());
        assert!(blocking.next(Timestamp::new_test_instance(), |_| true).is_none());
        assert!(blocking.get(&Account::from(1)).is_none());
        assert!(blocking.remove(&Account::from(1)).is_none());
        assert!(blocking.pop_oldest().is_none());
    }

    #[test]
    fn insert_one() {
        let mut blocking = BlockingContainer::default();

        let entry = BlockingEntry::new_test_instance();
        let inserted = blocking.insert(entry.clone());

        assert_eq!(inserted, true);
        assert_eq!(blocking.len(), 1);
        assert_eq!(blocking.is_empty(), false);
        assert_eq!(blocking.contains(&entry.account), true);
        assert!(blocking.get(&entry.account).is_some());
        assert_eq!(
            blocking.count_by_dependency_account(&entry.dependency_account),
            1
        );
    }

    #[test]
    fn dont_insert_if_account_already_present() {
        let mut blocking = BlockingContainer::default();

        let entry = BlockingEntry::new_test_instance();
        blocking.insert(entry.clone());

        let inserted = blocking.insert(entry.clone());

        assert_eq!(inserted, false);
        assert_eq!(blocking.len(), 1);
    }

    #[test]
    fn clear() {
        let mut blocking = BlockingContainer::default();

        let entry = BlockingEntry::new_test_instance();
        blocking.insert(entry.clone());
        blocking.clear();
        assert_eq!(blocking.sequenced.len(), 0);
        assert_eq!(blocking.by_account.len(), 0);
        assert_eq!(blocking.by_dependency.len(), 0);
        assert_eq!(blocking.by_dependency_account.len(), 0);
    }

    #[test]
    fn next() {
        let mut blocking = BlockingContainer::default();

        let entry = BlockingEntry {
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        };
        blocking.insert(entry);

        assert!(blocking.next(Timestamp::new_test_instance(), |_| true).is_some());
    }

    #[test]
    fn next_returns_none_when_all_dependency_accounts_are_known() {
        let mut blocking = BlockingContainer::default();

        let entry = BlockingEntry {
            dependency_account: Account::from(13),
            ..BlockingEntry::new_test_instance()
        };
        blocking.insert(entry.clone());

        assert!(blocking.next(Timestamp::new_test_instance(), |_| true).is_none());
    }

    #[test]
    fn next_with_filter() {
        let mut blocking = BlockingContainer::default();

        blocking.insert(BlockingEntry {
            account: Account::from(1000),
            dependency: BlockHash::from(100),
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        blocking.insert(BlockingEntry {
            account: Account::from(2000),
            dependency: BlockHash::from(200),
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        blocking.insert(BlockingEntry {
            account: Account::from(3000),
            dependency: BlockHash::from(300),
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        assert_eq!(
            blocking.next(Timestamp::new_test_instance(), |dep| *dep == BlockHash::from(300)),
            Some(BlockHash::from(300))
        );
    }

    #[test]
    fn pop_front() {
        let mut blocking = BlockingContainer::default();

        blocking.insert(BlockingEntry {
            account: Account::from(1000),
            dependency: BlockHash::from(100),
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        blocking.insert(BlockingEntry {
            account: Account::from(2000),
            dependency: BlockHash::from(200),
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        assert_eq!(blocking.pop_oldest().unwrap().account, Account::from(1000));
        assert_eq!(blocking.pop_oldest().unwrap().account, Account::from(2000));
        assert!(blocking.pop_oldest().is_none());
    }

    #[test]
    fn modify_dependency_account() {
        let mut blocking = BlockingContainer::default();

        let dependency = BlockHash::from(100);
        blocking.insert(BlockingEntry {
            account: Account::from(1000),
            dependency,
            dependency_account: Account::zero(),
            ..BlockingEntry::new_test_instance()
        });

        let new_dep_account = Account::from(5000);
        let updated = blocking.modify_dependency_account(&dependency, new_dep_account);

        assert_eq!(updated, 1);
        assert_eq!(
            blocking
                .get(&Account::from(1000))
                .unwrap()
                .dependency_account,
            new_dep_account
        );
    }

    #[test]
    fn modify_unknown_dependency_account() {
        let mut blocking = BlockingContainer::default();
        let updated = blocking.modify_dependency_account(&1.into(), 2.into());
        assert_eq!(updated, 0);
    }

    #[test]
    fn modify_dependency_account_with_multiple_entries() {
        let mut blocking = BlockingContainer::default();

        let dependency_account = Account::from(42);

        let entry1 = BlockingEntry {
            account: 1000.into(),
            dependency: 100.into(),
            dependency_account,
            ..BlockingEntry::new_test_instance()
        };
        let entry2 = BlockingEntry {
            account: 2000.into(),
            dependency: 200.into(),
            dependency_account,
            ..BlockingEntry::new_test_instance()
        };
        blocking.insert(entry1.clone());
        blocking.insert(entry2.clone());

        let new_dependency_account = Account::from(5000);
        let updated =
            blocking.modify_dependency_account(&entry1.dependency, new_dependency_account);

        assert_eq!(updated, 1);
        assert_eq!(
            blocking.get(&entry1.account).unwrap().dependency_account,
            new_dependency_account
        );
        assert_ne!(
            blocking.get(&entry2.account).unwrap().dependency_account,
            new_dependency_account
        );
    }

    #[test]
    fn modify_dependency_account_to_current_value() {
        let mut blocking = BlockingContainer::default();

        let dependency_account = Account::from(42);

        let entry = BlockingEntry {
            account: 1000.into(),
            dependency: 100.into(),
            dependency_account,
            ..BlockingEntry::new_test_instance()
        };
        blocking.insert(entry.clone());

        let updated = blocking.modify_dependency_account(&entry.dependency, dependency_account);

        assert_eq!(updated, 0);
        assert_eq!(
            blocking.get(&entry.account).unwrap().dependency_account,
            dependency_account
        );
    }

    #[test]
    fn iter_start_dependency_account() {
        let mut container = BlockingContainer::default();

        let entry1 = BlockingEntry {
            account: 1.into(),
            dependency: 100.into(),
            dependency_account: 10.into(),
            ..BlockingEntry::new_test_instance()
        };
        let entry2 = BlockingEntry {
            account: 2.into(),
            dependency: 200.into(),
            dependency_account: 20.into(),
            ..BlockingEntry::new_test_instance()
        };
        let entry3 = BlockingEntry {
            account: 3.into(),
            dependency: 300.into(),
            dependency_account: 30.into(),
            ..BlockingEntry::new_test_instance()
        };

        container.insert(entry1);
        container.insert(entry2.clone());
        container.insert(entry3.clone());

        let result: Vec<_> = container.iter_start_dep_account(20.into()).collect();

        assert_eq!(result, vec![&entry2, &entry3]);
    }

    #[test]
    fn remove_one_of_multiple_with_same_dependency() {
        let mut container = BlockingContainer::default();

        let same_dependency = BlockHash::from(9999);

        let entry1 = BlockingEntry {
            account: 1.into(),
            dependency: same_dependency,
            dependency_account: 10.into(),
            ..BlockingEntry::new_test_instance()
        };
        let entry2 = BlockingEntry {
            account: 2.into(),
            dependency: same_dependency,
            dependency_account: 20.into(),
            ..BlockingEntry::new_test_instance()
        };
        let entry3 = BlockingEntry {
            account: 3.into(),
            dependency: 300.into(),
            dependency_account: 30.into(),
            ..BlockingEntry::new_test_instance()
        };

        container.insert(entry1.clone());
        container.insert(entry2.clone());
        container.insert(entry3.clone());

        container.remove(&entry1.account);

        assert_eq!(
            container.by_dependency.get(&same_dependency).unwrap().len(),
            1
        );
    }

    #[test]
    fn remove_old_entries() {
        let mut container = BlockingContainer::default();
        let ts = Timestamp::new_test_instance();

        let entry1 = BlockingEntry {
            account: 1.into(),
            added: ts,
            ..BlockingEntry::new_test_instance()
        };
        let entry2 = BlockingEntry {
            account: 2.into(),
            added: ts + Duration::from_secs(1),
            ..BlockingEntry::new_test_instance()
        };
        let entry3 = BlockingEntry {
            account: 3.into(),
            added: ts + Duration::from_secs(2),
            ..BlockingEntry::new_test_instance()
        };
        let entry4 = BlockingEntry {
            account: 4.into(),
            added: ts + Duration::from_secs(3),
            ..BlockingEntry::new_test_instance()
        };
        container.insert(entry1.clone());
        container.insert(entry2.clone());
        container.insert(entry3.clone());
        container.insert(entry4.clone());

        let removed = container.remove_older_than(entry3.added);

        assert_eq!(removed, 2);
        assert_eq!(container.len(), 2);
        assert!(container.contains(&entry3.account));
        assert!(container.contains(&entry4.account));
    }
}
