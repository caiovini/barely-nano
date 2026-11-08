use alloc::vec::Vec;
use super::database_crawler::{AccountCrawlSource, DatabaseCrawler};
use crate::bootstrap::state::OutdatedAccounts;
use rsnano_core::{Account, Frontier};
use rsnano_ledger::{AnySet, OwningAnySet};

pub(crate) enum FrontierCheckResult {
    /// Account doesn't exist in the ledger and has no pending blocks, can't be prioritized right now
    UnknownAccount,
    /// Account exists and frontier is up-to-date
    UpToDate,
    /// Account exists but is outdated
    Outdated,
    /// Account doesn't exist but has pending blocks in the ledger
    Pending,
}

/// Checks if a given frontier is up to date or outdated
pub(crate) struct FrontierChecker<'a> {
    any: &'a OwningAnySet<'a>,
    account_crawler: DatabaseCrawler<'a, AccountCrawlSource<'a>>,
}

impl<'a> FrontierChecker<'a> {
    pub(crate) fn new(any: &'a OwningAnySet<'a>) -> Self {
        Self {
            any,
            account_crawler: DatabaseCrawler::new(AccountCrawlSource::new(any)),
        }
    }

    pub fn get_outdated_accounts(&mut self, frontiers: &[Frontier]) -> OutdatedAccounts {
        if frontiers.is_empty() {
            return Default::default();
        }

        let mut outdated = 0;
        let mut pending = 0;
        let mut accounts = Vec::new();
        let last_account = frontiers.last().map(|f| f.account);
        self.initialize(frontiers[0].account, last_account);

        for frontier in frontiers {
            match self.check_frontier(frontier) {
                FrontierCheckResult::Outdated => {
                    outdated += 1;
                    accounts.push(frontier.account);
                }
                FrontierCheckResult::Pending => {
                    pending += 1;
                    accounts.push(frontier.account);
                }
                FrontierCheckResult::UnknownAccount | FrontierCheckResult::UpToDate => {}
            }
        }

        OutdatedAccounts {
            accounts,
            outdated,
            pending,
            fontiers_received: frontiers.len(),
        }
    }

    fn check_frontier(&mut self, frontier: &Frontier) -> FrontierCheckResult {
        self.account_crawler.advance_to(frontier.account);

        // Check if account exists in our ledger
        if let Some((account, info)) = &self.account_crawler.current {
            if *account == frontier.account {
                // Check for frontier mismatch or missing local head block
                if info.head != frontier.hash
                    || (!cfg!(test) && !rsnano_ledger::block_may_exist(&frontier.hash))
                {
                    // Check if frontier block exists in our ledger
                    if !self.any.block_exists_or_pruned(&frontier.hash) {
                        return FrontierCheckResult::Outdated; // Frontier is outdated
                    }
                }
                return FrontierCheckResult::UpToDate;
            }
        }

        // Only check pending blocks when the account does not exist in our ledger.
        // On bare-metal, use the O(log N) single-key lookup `receivable_exists`
        // instead of advancing a cross-shard range iterator.
        if self.any.receivable_exists(frontier.account) {
            return FrontierCheckResult::Pending;
        }

        FrontierCheckResult::UnknownAccount
    }

    fn initialize(&mut self, start: Account, end: Option<Account>) {
        self.account_crawler.set_end(end);
        self.account_crawler.seek(start);
    }
}

