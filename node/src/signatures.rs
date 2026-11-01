use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;
use rsnano_core::Account;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::block_processing::{SIG_STATE_DONE, SIG_STATE_IN_PROGRESS, SIG_STATE_UNCLAIMED};

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureResult {
    Unknown = 0,
    Valid = 1,
    Invalid = 2,
}

pub struct SignatureTask {
    pub ctx: Arc<crate::block_processing::BlockContext>,
    pub account: Option<Account>,
}

const MAX_BG_TASKS: usize = 131072;

pub struct SignatureChecker {
    tasks: Mutex<VecDeque<SignatureTask>>,
    bg_tasks: Mutex<VecDeque<Arc<crate::block_processing::BlockContext>>>,
    sync_count: AtomicUsize,
    bg_count: AtomicUsize,
}

impl SignatureChecker {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(VecDeque::with_capacity(512)),
            bg_tasks: Mutex::new(VecDeque::with_capacity(4096)),
            sync_count: AtomicUsize::new(0),
            bg_count: AtomicUsize::new(0),
        }
    }

    pub fn push_batch_front(
        &self,
        items: alloc::vec::Vec<(Arc<crate::block_processing::BlockContext>, Option<Account>)>,
    ) -> alloc::vec::Vec<Arc<crate::block_processing::BlockContext>> {
        let mut out = alloc::vec::Vec::with_capacity(items.len());
        if items.is_empty() {
            return out;
        }
        let mut lock = self.tasks.lock();
        for (ctx, account) in items.into_iter().rev() {
            if ctx.block.is_signature_verified()
                || ctx.sig_state.load(Ordering::Acquire) == SIG_STATE_DONE
            {
                continue;
            }
            if ctx.sig_state.load(Ordering::Acquire) == SIG_STATE_IN_PROGRESS {
                // Already claimed and actively being verified by a background worker hart!
                // Do not enqueue a duplicate task; just wait for it to reach SIG_STATE_DONE.
                out.push(ctx);
                continue;
            }
            lock.push_front(SignatureTask {
                ctx: ctx.clone(),
                account,
            });
            out.push(ctx);
        }
        self.sync_count.store(lock.len(), Ordering::Release);
        out
    }

    #[inline]
    pub fn enqueue_bg(&self, ctx: &Arc<crate::block_processing::BlockContext>) {
        if self.bg_count.load(Ordering::Relaxed) >= MAX_BG_TASKS {
            return;
        }
        if ctx.source == crate::block_processing::BlockSource::Local
            || ctx.block.is_signature_verified()
            || ctx.sig_state.load(Ordering::Relaxed) != SIG_STATE_UNCLAIMED
        {
            return;
        }
        let has_account = ctx
            .block
            .account_field()
            .or(ctx.account)
            .is_some_and(|a| !a.is_zero());
        if !has_account {
            return;
        }
        let mut lock = self.bg_tasks.lock();
        if lock.len() < MAX_BG_TASKS {
            lock.push_back(ctx.clone());
            self.bg_count.store(lock.len(), Ordering::Release);
        }
    }

    pub fn enqueue_bg_batch(&self, ctxs: &[Arc<crate::block_processing::BlockContext>]) {
        if self.bg_count.load(Ordering::Relaxed) >= MAX_BG_TASKS || ctxs.is_empty() {
            return;
        }
        let mut lock = self.bg_tasks.lock();
        for ctx in ctxs {
            if lock.len() >= MAX_BG_TASKS {
                break;
            }
            if ctx.source == crate::block_processing::BlockSource::Local
                || ctx.block.is_signature_verified()
                || ctx.sig_state.load(Ordering::Relaxed) != SIG_STATE_UNCLAIMED
            {
                continue;
            }
            let has_account = ctx
                .block
                .account_field()
                .or(ctx.account)
                .is_some_and(|a| !a.is_zero());
            if has_account {
                lock.push_back(ctx.clone());
            }
        }
        self.bg_count.store(lock.len(), Ordering::Release);
    }

    #[inline]
    pub fn has_sync_tasks(&self) -> bool {
        self.sync_count.load(Ordering::Relaxed) > 0
    }

    #[inline]
    pub fn is_empty_relaxed(&self) -> bool {
        self.sync_count.load(Ordering::Relaxed) == 0 && self.bg_count.load(Ordering::Relaxed) == 0
    }

    /// Used by Hart 6 (`BlockBatchProcessor`) to verify only synchronous batch tasks.
    pub fn help_verify_sync(&self) -> usize {
        if self.sync_count.load(Ordering::Relaxed) == 0 {
            return 0;
        }
        let mut tasks: [Option<SignatureTask>; 4] = [None, None, None, None];
        let mut count = 0;
        {
            let mut lock = self.tasks.lock();
            if lock.is_empty() {
                self.sync_count.store(0, Ordering::Relaxed);
                return 0;
            }
            while count < 4 {
                if let Some(t) = lock.pop_front() {
                    if t.ctx.block.is_signature_verified() {
                        t.ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
                        continue;
                    }
                    if t.ctx
                        .sig_state
                        .compare_exchange(
                            SIG_STATE_UNCLAIMED,
                            SIG_STATE_IN_PROGRESS,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    tasks[count] = Some(t);
                    count += 1;
                } else {
                    break;
                }
            }
            self.sync_count.store(lock.len(), Ordering::Relaxed);
        }

        if count == 0 {
            return 0;
        }

        self.verify_tasks_slice(&mut tasks[..count]);
        count
    }

    /// Used by Crypto Worker Harts: drains high-priority synchronous batch tasks first,
    /// and when idle, pre-verifies queued blocks from `bg_tasks`.
    pub fn help_verify(&self) -> usize {
        if self.sync_count.load(Ordering::Relaxed) > 0 {
            let mut tasks: [Option<SignatureTask>; 4] = [None, None, None, None];
            let mut count = 0;
            {
                let mut lock = self.tasks.lock();
                while count < 4 {
                    if let Some(t) = lock.pop_front() {
                        if t.ctx.block.is_signature_verified() {
                            t.ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
                            continue;
                        }
                        if t.ctx
                            .sig_state
                            .compare_exchange(
                                SIG_STATE_UNCLAIMED,
                                SIG_STATE_IN_PROGRESS,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_err()
                        {
                            continue;
                        }
                        tasks[count] = Some(t);
                        count += 1;
                    } else {
                        break;
                    }
                }
                self.sync_count.store(lock.len(), Ordering::Relaxed);
            }
            if count > 0 {
                self.verify_tasks_slice(&mut tasks[..count]);
                return count;
            }
        }

        if self.bg_count.load(Ordering::Relaxed) == 0 {
            return 0;
        }

        let mut tasks: [Option<SignatureTask>; 4] = [None, None, None, None];
        let mut count = 0;
        {
            let mut lock = self.bg_tasks.lock();
            if lock.is_empty() {
                self.bg_count.store(0, Ordering::Relaxed);
                return 0;
            }
            while count < 4 {
                if let Some(ctx) = lock.pop_front() {
                    if ctx.block.is_signature_verified() {
                        ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
                        continue;
                    }
                    let account = ctx
                        .block
                        .account_field()
                        .or(ctx.account)
                        .filter(|a| !a.is_zero());
                    if account.is_none() {
                        continue;
                    }
                    if ctx
                        .sig_state
                        .compare_exchange(
                            SIG_STATE_UNCLAIMED,
                            SIG_STATE_IN_PROGRESS,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    tasks[count] = Some(SignatureTask { ctx, account });
                    count += 1;
                } else {
                    break;
                }
            }
            self.bg_count.store(lock.len(), Ordering::Relaxed);
        }

        if count == 0 {
            return 0;
        }

        self.verify_tasks_slice(&mut tasks[..count]);
        count
    }

    pub fn try_help_verify(&self) -> bool {
        self.help_verify() > 0
    }

    fn verify_tasks_slice(&self, tasks: &mut [Option<SignatureTask>]) {
        if tasks.len() == 4 {
            let mut all_have_account = true;
            let mut accounts = [rsnano_core::Account::zero(); 4];
            for i in 0..4 {
                let t = tasks[i].as_ref().unwrap();
                if let Some(acc) = t.account {
                    accounts[i] = acc;
                } else if let Some(acc) = t.ctx.block.account_field() {
                    accounts[i] = acc;
                } else {
                    all_have_account = false;
                    break;
                }
            }

            if all_have_account {
                let t0 = tasks[0].as_ref().unwrap();
                let t1 = tasks[1].as_ref().unwrap();
                let t2 = tasks[2].as_ref().unwrap();
                let t3 = tasks[3].as_ref().unwrap();

                let keys_val = [
                    accounts[0].as_key(),
                    accounts[1].as_key(),
                    accounts[2].as_key(),
                    accounts[3].as_key(),
                ];
                let keys = [
                    keys_val[0].as_bytes(),
                    keys_val[1].as_bytes(),
                    keys_val[2].as_bytes(),
                    keys_val[3].as_bytes(),
                ];
                let hashes = [
                    t0.ctx.block.hash(),
                    t1.ctx.block.hash(),
                    t2.ctx.block.hash(),
                    t3.ctx.block.hash(),
                ];
                let msgs = [
                    hashes[0].as_bytes() as &[u8],
                    hashes[1].as_bytes() as &[u8],
                    hashes[2].as_bytes() as &[u8],
                    hashes[3].as_bytes() as &[u8],
                ];
                let sigs_val = [
                    t0.ctx.block.signature(),
                    t1.ctx.block.signature(),
                    t2.ctx.block.signature(),
                    t3.ctx.block.signature(),
                ];
                let sigs = [
                    sigs_val[0].as_bytes(),
                    sigs_val[1].as_bytes(),
                    sigs_val[2].as_bytes(),
                    sigs_val[3].as_bytes(),
                ];

                let results = rsnano_sha::ed25519::verify_batch_4(keys, msgs, sigs);
                for i in 0..4 {
                    let t = tasks[i].as_ref().unwrap();
                    if results[i] {
                        t.ctx.block.set_signature_verified(true);
                    }
                    t.ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
                }
                return;
            }
        }

        // Graceful sequential fallback for less than 4 tasks or invalid accounts
        for opt in tasks.iter_mut() {
            if let Some(task) = opt.take() {
                let account = if let Some(acc) = task.account {
                    acc
                } else if let Some(acc) = task.ctx.block.account_field() {
                    acc
                } else {
                    task.ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
                    continue;
                };

                let valid = task.ctx.block.verify_with_key(&account.as_key()).is_ok();
                if valid {
                    task.ctx.block.set_signature_verified(true);
                }
                task.ctx.sig_state.store(SIG_STATE_DONE, Ordering::Release);
            }
        }
    }

    pub fn total_tasks(&self) -> usize {
        self.tasks.lock().len()
    }
}
