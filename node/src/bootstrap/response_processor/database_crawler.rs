use alloc::boxed::Box;
use rsnano_core::{Account, AccountInfo};
use rsnano_ledger::OwningAnySet;

pub(super) struct DatabaseCrawler<'a, T>
where
    T: CrawlSource<'a>,
{
    source: T,
    it: Option<Box<dyn Iterator<Item = (Account, T::Value)> + 'a>>,
    pub current: Option<(Account, T::Value)>,
    end: Option<Account>,
    exhausted: bool,
}

const SEQUENTIAL_ATTEMPTS: usize = 10;

impl<'a, T> DatabaseCrawler<'a, T>
where
    T: CrawlSource<'a>,
{
    pub fn new(source: T) -> Self {
        Self {
            it: None,
            current: None,
            source,
            end: None,
            exhausted: false,
        }
    }

    pub fn set_end(&mut self, end: Option<Account>) {
        self.end = end;
        self.exhausted = false;
    }

    pub fn seek(&mut self, target: Account) {
        let mut it = self.source.iter(target, self.end);
        self.current = it.next();
        if self.current.is_some() {
            self.it = Some(it);
            self.exhausted = false;
        } else {
            self.it = None;
            self.exhausted = true;
        }
    }

    pub fn advance_to(&mut self, target: Account) {
        if self.exhausted || Self::target_reached(&self.current, &target) {
            return;
        }

        if !self.advance_sequentially(target) {
            // perform a fresh lookup
            self.seek(target);
        }
    }

    fn target_reached(current: &Option<(Account, T::Value)>, target: &Account) -> bool {
        if let Some((account, _)) = current {
            if account >= target {
                return true;
            }
        }
        false
    }

    /// It is faster to try to reuse the existing iterator than to
    /// perform a fresh seek.
    /// Only if this doesn't succeed we perform a fresh seek.
    fn advance_sequentially(&mut self, target: Account) -> bool {
        let Some(it) = &mut self.it else {
            return false;
        };

        for _ in 0..SEQUENTIAL_ATTEMPTS {
            self.current = it.next();
            if Self::target_reached(&self.current, &target) {
                return true;
            }

            if self.current.is_none() {
                self.it = None;
                self.exhausted = true;
                return true;
            }
        }

        false
    }
}

pub(crate) trait CrawlSource<'a> {
    type Value;
    fn iter(
        &self,
        start: Account,
        end: Option<Account>,
    ) -> Box<dyn Iterator<Item = (Account, Self::Value)> + 'a>;
}

pub(crate) struct AccountCrawlSource<'a> {
    any: &'a OwningAnySet<'a>,
}

impl<'a> AccountCrawlSource<'a> {
    pub(crate) fn new(any: &'a OwningAnySet<'a>) -> Self {
        Self { any }
    }
}

impl<'a> CrawlSource<'a> for AccountCrawlSource<'a> {
    type Value = AccountInfo;

    fn iter(
        &self,
        start: Account,
        end: Option<Account>,
    ) -> Box<dyn Iterator<Item = (Account, AccountInfo)> + 'a> {
        if let Some(end_acc) = end {
            Box::new(self.any.iter_account_range(start..=end_acc))
        } else {
            Box::new(self.any.iter_account_range(start..))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct VecCrawlSource {
        items: Vec<(Account, AccountInfo)>,
    }

    impl VecCrawlSource {
        fn new(mut items: Vec<(Account, AccountInfo)>) -> Self {
            items.sort_by_key(|(acc, _)| *acc);
            Self { items }
        }
    }

    impl<'a> CrawlSource<'a> for &'a VecCrawlSource {
        type Value = AccountInfo;

        fn iter(
            &self,
            start: Account,
            end: Option<Account>,
        ) -> Box<dyn Iterator<Item = (Account, AccountInfo)> + 'a> {
            Box::new(
                self.items
                    .iter()
                    .filter(move |(acc, _)| {
                        *acc >= start && end.map_or(true, |end_acc| *acc <= end_acc)
                    })
                    .cloned(),
            )
        }
    }

    #[test]
    fn seek_exact() {
        let account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info.clone())]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(account);
        assert_eq!(crawler.current, Some((account, info)));
    }

    #[test]
    fn seek_before() {
        let account = Account::from(2);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info.clone())]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(Account::from(1));
        assert_eq!(crawler.current, Some((account, info)));
    }

    #[test]
    fn seek_after() {
        let account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info)]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(Account::from(2));
        assert_eq!(crawler.current, None);
    }

    #[test]
    fn advance_not_found() {
        let account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info)]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(Account::from(2));
        crawler.advance_to(Account::from(99999));
        assert_eq!(crawler.current, None);
    }

    #[test]
    fn advance_exact() {
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![
            (account1, info.clone()),
            (account2, info.clone())
        ]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(account1);
        crawler.advance_to(account2);
        assert_eq!(crawler.current, Some((account2, info)));
    }

    #[test]
    fn advance_seek() {
        let first_account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let mut items = alloc::vec![(first_account, info.clone())];

        for i in 0..SEQUENTIAL_ATTEMPTS + 1 {
            items.push((Account::from(i as u64 + 2), info.clone()));
        }

        let source = VecCrawlSource::new(items);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(first_account);
        let target = Account::from(SEQUENTIAL_ATTEMPTS as u64 + 2);
        crawler.advance_to(target);
        assert_eq!(crawler.current, Some((target, info)));
    }

    #[test]
    fn advance_noop() {
        let account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info.clone())]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(account);
        crawler.advance_to(account);
        assert_eq!(crawler.current, Some((account, info)));
    }

    #[test]
    fn advance_after_nothing_found() {
        let account = Account::from(1);
        let info = AccountInfo::new_test_instance();
        let source = VecCrawlSource::new(alloc::vec![(account, info)]);
        let mut crawler = DatabaseCrawler::new(&source);
        crawler.seek(account);
        crawler.advance_to(account.inc_or_max());
        assert_eq!(crawler.current, None);
        crawler.advance_to(account.inc_or_max());
        assert_eq!(crawler.current, None);
    }
}

