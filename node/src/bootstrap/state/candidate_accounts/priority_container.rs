use rsnano_core::Account;
use rsnano_nullable_clock::Timestamp;
use alloc::collections::{BTreeMap, BTreeSet};
use core::mem::size_of;

use super::priority::{Priority, PriorityKeyDesc};

use super::pull_type_decider::PullType;

#[derive(Clone)]
pub(super) struct PriorityEntry {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
    pub last_request: Option<Timestamp>,
    pub pull_type: PullType,
}

impl Default for PriorityEntry {
    fn default() -> Self {
        Self {
            account: Default::default(),
            priority: Default::default(),
            fails: Default::default(),
            last_request: Default::default(),
            pull_type: PullType::Optimistic,
        }
    }
}

impl PriorityEntry {
    pub fn new(account: Account, priority: Priority, pull_type: PullType) -> Self {
        Self {
            account,
            priority,
            fails: 0,
            last_request: None,
            pull_type,
        }
    }

    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            account: Account::from(7),
            priority: Priority::new(10.0),
            fails: 0,
            last_request: None,
            pull_type: PullType::Optimistic,
        }
    }
}

/// Tracks the ongoing account priorities
/// This only stores account priorities > 1.0f.
#[derive(Default)]
pub(super) struct PriorityContainer {
    by_account: BTreeMap<Account, PriorityEntry>,
    by_priority: BTreeMap<PriorityKeyDesc, BTreeSet<(Option<Timestamp>, Account)>>, // descending priority, ascending last_request
}

pub(crate) enum ChangePriorityResult {
    Updated,
    Deleted,
    NotFound,
}

impl PriorityContainer {
    pub const ELEMENT_SIZE: usize =
        size_of::<PriorityEntry>() + size_of::<Account>() + size_of::<f32>() + size_of::<u64>() * 4;

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_account.is_empty()
    }

    pub fn get(&self, account: &Account) -> Option<&PriorityEntry> {
        self.by_account.get(account)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn insert(&mut self, entry: PriorityEntry) -> bool {
        let account = entry.account;
        if account.is_zero() {
            return false;
        }
        let priority = entry.priority;
        let last_request = entry.last_request;

        if self.by_account.contains_key(&account) {
            return false;
        }

        self.by_account.insert(account, entry);
        self.by_priority
            .entry(priority.into())
            .or_default()
            .insert((last_request, account));
        true
    }

    pub fn pop_lowest_prio(&mut self) -> Option<PriorityEntry> {
        let lowest_prio_account = {
            let (_, v) = self.by_priority.last_key_value()?;
            v.iter().next()?.1
        };
        Some(self.remove_account(&lowest_prio_account))
    }

    pub fn set_last_request(&mut self, account: &Account, timestamp: Option<Timestamp>) {
        if let Some(entry) = self.by_account.get_mut(account) {
            let old_ts = entry.last_request;
            if old_ts != timestamp {
                let prio = entry.priority;
                entry.last_request = timestamp;
                if let Some(ids) = self.by_priority.get_mut(&prio.into()) {
                    ids.remove(&(old_ts, *account));
                    ids.insert((timestamp, *account));
                }
            }
        }
    }

    pub fn modify(
        &mut self,
        account: &Account,
        mut f: impl FnMut(&mut PriorityEntry) -> bool,
    ) -> ChangePriorityResult {
        if let Some(entry) = self.by_account.get_mut(account) {
            let old_prio = entry.priority;
            let old_ts = entry.last_request;
            if f(entry) {
                let new_prio = entry.priority;
                let new_ts = entry.last_request;
                if new_prio != old_prio || new_ts != old_ts {
                    self.change_priority_internal(account, old_prio, old_ts, new_prio, new_ts);
                }
                ChangePriorityResult::Updated
            } else {
                self.remove_account(account);
                ChangePriorityResult::Deleted
            }
        } else {
            ChangePriorityResult::NotFound
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.by_priority
            .iter()
            .flat_map(|(prio, accs)| accs.iter().map(|(_, a)| (prio.0, a)))
    }

    pub fn next_priority(
        &self,
        cutoff: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> Option<&PriorityEntry> {
        for accs in self.by_priority.values() {
            for (last_req, account) in accs.iter() {
                if let Some(ts) = last_req {
                    if *ts > cutoff {
                        // All subsequent entries in this priority bucket have last_request >= *ts > cutoff
                        break;
                    }
                }
                if filter(account) {
                    return self.by_account.get(account);
                }
            }
        }
        None
    }

    pub fn remove(&mut self, account: &Account) -> Option<PriorityEntry> {
        if let Some(entry) = self.by_account.remove(account) {
            self.remove_priority(account, entry.priority, entry.last_request);
            Some(entry)
        } else {
            None
        }
    }

    fn change_priority_internal(
        &mut self,
        account: &Account,
        old_prio: Priority,
        old_ts: Option<Timestamp>,
        new_prio: Priority,
        new_ts: Option<Timestamp>,
    ) {
        self.remove_priority(account, old_prio, old_ts);
        self.by_priority
            .entry(new_prio.into())
            .or_default()
            .insert((new_ts, *account));
    }

    fn remove_account(&mut self, account: &Account) -> PriorityEntry {
        let entry = self.by_account.remove(account).unwrap();
        self.remove_priority(account, entry.priority, entry.last_request);
        entry
    }

    fn remove_priority(&mut self, account: &Account, priority: Priority, last_req: Option<Timestamp>) {
        let ids = self.by_priority.get_mut(&priority.into()).unwrap();
        if ids.len() > 1 {
            ids.remove(&(last_req, *account));
        } else {
            self.by_priority.remove(&priority.into());
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.by_account.clear();
        self.by_priority.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let mut priorities = PriorityContainer::default();
        assert_eq!(priorities.len(), 0);
        assert!(priorities.is_empty());
        assert!(priorities.get(&Account::from(1)).is_none());
        assert_eq!(priorities.contains(&Account::from(1)), false);
        assert!(priorities.pop_lowest_prio().is_none());
        assert!(priorities.remove(&Account::from(1)).is_none());
    }

    #[test]
    fn insert_one() {
        let mut priorities = PriorityContainer::default();
        let entry = PriorityEntry::new_test_instance();
        assert!(priorities.insert(entry.clone()));
        assert_eq!(priorities.len(), 1);
        assert_eq!(priorities.is_empty(), false);
        assert_eq!(priorities.contains(&entry.account), true);
        assert!(priorities.get(&entry.account).is_some());
    }

    #[test]
    fn insert_two() {
        let mut priorities = PriorityContainer::default();
        assert!(priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(2.5),
            PullType::Optimistic
        )));
        assert!(priorities.insert(PriorityEntry::new(
            Account::from(2),
            Priority::new(3.5),
            PullType::Optimistic
        )));
        assert_eq!(priorities.len(), 2);
        assert_eq!(priorities.is_empty(), false);
        assert_eq!(priorities.contains(&Account::from(1)), true);
        assert_eq!(priorities.contains(&Account::from(2)), true);
    }

    #[test]
    fn dont_insert_when_account_already_present() {
        let mut priorities = PriorityContainer::default();
        priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        let inserted = priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(3.5),
            PullType::Optimistic,
        ));
        assert_eq!(inserted, false);
        assert_eq!(priorities.len(), 1);
    }

    #[test]
    fn pop_front() {
        let mut priorities = PriorityContainer::default();
        priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(2),
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(3),
            Priority::new(2.5),
            PullType::Optimistic,
        ));

        assert_eq!(
            priorities.pop_lowest_prio().unwrap().account,
            Account::from(1)
        );
        assert_eq!(
            priorities.pop_lowest_prio().unwrap().account,
            Account::from(2)
        );
        assert_eq!(
            priorities.pop_lowest_prio().unwrap().account,
            Account::from(3)
        );
        assert!(priorities.pop_lowest_prio().is_none());
    }

    #[test]
    fn change_timestamp() {
        let account = Account::from(1);
        let mut priorities = PriorityContainer::default();
        priorities.insert(PriorityEntry::new(
            account,
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        let now = Timestamp::new_test_instance();

        priorities.set_last_request(&account, Some(now));

        assert_eq!(priorities.get(&account).unwrap().last_request, Some(now));
    }

    mod next_priority {
        use super::*;
        use core::time::Duration;

        #[test]
        fn empty() {
            let priorities = PriorityContainer::default();
            let next = priorities.next_priority(Timestamp::new_test_instance(), |_entry| true);
            assert!(next.is_none());
        }

        #[test]
        fn one_item() {
            let mut priorities = PriorityContainer::default();
            let account = Account::from(1);
            priorities.insert(PriorityEntry::new(
                account,
                Priority::new(2.5),
                PullType::Optimistic,
            ));

            let next = priorities
                .next_priority(Timestamp::new_test_instance(), |_entry| true)
                .unwrap();

            assert_eq!(next.account, account);
        }

        #[test]
        fn ordered_by_priority_desc() {
            let mut priorities = PriorityContainer::default();
            priorities.insert(PriorityEntry::new(
                Account::from(1),
                Priority::new(2.5),
                PullType::Optimistic,
            ));
            priorities.insert(PriorityEntry::new(
                Account::from(2),
                Priority::new(10.0),
                PullType::Optimistic,
            ));
            priorities.insert(PriorityEntry::new(
                Account::from(3),
                Priority::new(3.5),
                PullType::Optimistic,
            ));

            let next = priorities
                .next_priority(Timestamp::new_test_instance(), |_entry| true)
                .unwrap();

            assert_eq!(next.account, Account::from(2));
        }

        #[test]
        fn cutoff() {
            let now = Timestamp::new_test_instance();
            let a = PriorityEntry::new(Account::from(1), Priority::new(2.5), PullType::Optimistic);
            let mut b =
                PriorityEntry::new(Account::from(2), Priority::new(10.0), PullType::Optimistic);
            b.last_request = Some(now);
            let mut c =
                PriorityEntry::new(Account::from(3), Priority::new(3.5), PullType::Optimistic);
            c.last_request = Some(now - Duration::from_secs(60));
            let mut priorities = PriorityContainer::default();
            priorities.insert(a);
            priorities.insert(b);
            priorities.insert(c);

            let next = priorities
                .next_priority(now - Duration::from_secs(30), |_| true)
                .unwrap();

            assert_eq!(next.account, Account::from(3));
        }

        #[test]
        fn filter() {
            let a = PriorityEntry::new(Account::from(1), Priority::new(2.5), PullType::Optimistic);
            let b = PriorityEntry::new(Account::from(2), Priority::new(10.0), PullType::Optimistic);
            let c = PriorityEntry::new(Account::from(3), Priority::new(3.5), PullType::Optimistic);
            let mut priorities = PriorityContainer::default();
            priorities.insert(a);
            priorities.insert(b);
            priorities.insert(c);

            let next = priorities
                .next_priority(Timestamp::new_test_instance(), |account| {
                    *account == Account::from(1)
                })
                .unwrap();

            assert_eq!(next.account, Account::from(1));
        }
    }

    #[test]
    fn change_priority() {
        let mut priorities = PriorityContainer::default();
        priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(2),
            Priority::new(3.0),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(3),
            Priority::new(3.5),
            PullType::Optimistic,
        ));

        let mut old_priority = Priority::ZERO;
        let new_priority = Priority::new(10.0);

        priorities.modify(&Account::from(2), |entry| {
            old_priority = entry.priority;
            entry.priority = new_priority;
            true
        });

        assert_eq!(old_priority, Priority::new(3.0));
        assert_eq!(
            priorities.get(&Account::from(2)).unwrap().priority,
            new_priority
        );

        let next = priorities
            .next_priority(Timestamp::new_test_instance(), |_| true)
            .unwrap();
        assert_eq!(next.account, Account::from(2));
    }

    #[test]
    fn remove_by_priority_change() {
        let mut priorities = PriorityContainer::default();
        let account = Account::from(1);
        priorities.insert(PriorityEntry::new(
            account,
            Priority::new(2.5),
            PullType::Optimistic,
        ));

        priorities.modify(&account, |_| false);

        assert_eq!(priorities.len(), 0);
    }

    #[test]
    fn remove() {
        let mut priorities = PriorityContainer::default();
        priorities.insert(PriorityEntry::new(
            Account::from(1),
            Priority::new(2.5),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(2),
            Priority::new(3.0),
            PullType::Optimistic,
        ));
        priorities.insert(PriorityEntry::new(
            Account::from(3),
            Priority::new(3.5),
            PullType::Optimistic,
        ));

        let removed = priorities.remove(&Account::from(2)).unwrap();

        assert_eq!(removed.account, Account::from(2));
        assert_eq!(priorities.len(), 2);
    }
}
