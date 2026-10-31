use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;
use rsnano_core::{Block, Account};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureResult {
    Unknown = 0,
    Valid = 1,
    Invalid = 2,
}

pub struct SignatureTask {
    pub block: Arc<Block>,
    pub ctx: Option<Arc<crate::block_processing::BlockContext>>,
    pub account: Option<Account>,
    pub result: Option<Arc<AtomicU8>>,
    pub finished: Option<Arc<AtomicBool>>,
}

pub struct SignatureChecker {
    tasks: Mutex<VecDeque<SignatureTask>>,
    pending_count: core::sync::atomic::AtomicUsize,
}

impl SignatureChecker {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(VecDeque::with_capacity(512)),
            pending_count: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn push(&self, block: Arc<Block>, account: Option<Account>) -> (Arc<AtomicU8>, Arc<AtomicBool>) {
        let result = Arc::new(AtomicU8::new(SignatureResult::Unknown as u8));
        let finished = Arc::new(AtomicBool::new(false));
        let task = SignatureTask {
            block,
            ctx: None,
            account,
            result: Some(result.clone()),
            finished: Some(finished.clone()),
        };
        let mut lock = self.tasks.lock();
        lock.push_front(task);
        self.pending_count.store(lock.len(), Ordering::Release);
        (result, finished)
    }

    pub fn push_batch_front(
        &self,
        items: alloc::vec::Vec<(Arc<crate::block_processing::BlockContext>, Option<Account>)>,
    ) -> alloc::vec::Vec<(Arc<crate::block_processing::BlockContext>, Arc<AtomicU8>, Arc<AtomicBool>)> {
        let mut out = alloc::vec::Vec::with_capacity(items.len());
        if items.is_empty() {
            return out;
        }
        let mut lock = self.tasks.lock();
        for (ctx, account) in items.into_iter().rev() {
            let result = Arc::new(AtomicU8::new(SignatureResult::Unknown as u8));
            let finished = Arc::new(AtomicBool::new(false));
            let task = SignatureTask {
                block: Arc::new(ctx.block.clone()),
                ctx: Some(ctx.clone()),
                account,
                result: Some(result.clone()),
                finished: Some(finished.clone()),
            };
            lock.push_front(task);
            out.push((ctx, result, finished));
        }
        self.pending_count.store(lock.len(), Ordering::Release);
        out
    }

    #[allow(dead_code)]
    pub fn enqueue_preverify_batch(
        &self,
        ctxs: &[Arc<crate::block_processing::BlockContext>],
    ) {
        if self.pending_count.load(Ordering::Relaxed) >= 512 || ctxs.is_empty() {
            return;
        }
        let mut playable_hashes: alloc::collections::BTreeMap<
            rsnano_core::BlockHash,
            Option<rsnano_core::Amount>,
        > = alloc::collections::BTreeMap::new();
        let mut to_enqueue = alloc::vec::Vec::new();

        for ctx in ctxs {
            if ctx.source == crate::block_processing::BlockSource::Local {
                continue;
            }
            let b = &ctx.block;
            let hash = b.hash();
            let prev = b.previous();
            if !prev.is_zero()
                && !playable_hashes.contains_key(&prev)
                && !rsnano_ledger::block_may_exist(&prev)
            {
                continue;
            }

            if !b.is_signature_verified() {
                let verify_account = b
                    .account_field()
                    .or(ctx.account)
                    .filter(|a| !a.is_zero());
                if verify_account.is_some() {
                    to_enqueue.push((ctx.clone(), verify_account));
                }
            }

            let mut gap_source = false;
            match b {
                Block::LegacyReceive(_) | Block::LegacyOpen(_) => {
                    let source = b.source_or_link();
                    if !source.is_zero()
                        && !playable_hashes.contains_key(&source)
                        && !rsnano_ledger::block_may_exist(&source)
                    {
                        gap_source = true;
                    }
                }
                Block::State(s) => {
                    if !s.link().is_zero() {
                        let source: rsnano_core::BlockHash = s.link().into();
                        let is_receive = if prev.is_zero() {
                            true
                        } else if let Some(Some(prev_bal)) = playable_hashes.get(&prev) {
                            s.balance() >= *prev_bal
                        } else {
                            false
                        };
                        if is_receive
                            && !playable_hashes.contains_key(&source)
                            && !rsnano_ledger::block_may_exist(&source)
                        {
                            gap_source = true;
                        }
                    }
                }
                _ => {}
            }

            if !gap_source {
                playable_hashes.insert(hash, b.balance_field());
            }
        }

        if to_enqueue.is_empty() {
            return;
        }
        if let Some(mut lock) = self.tasks.try_lock() {
            for (ctx, account) in to_enqueue {
                if lock.len() >= 512 {
                    break;
                }
                lock.push_back(SignatureTask {
                    block: Arc::new(ctx.block.clone()),
                    ctx: Some(ctx),
                    account,
                    result: None,
                    finished: None,
                });
            }
            self.pending_count.store(lock.len(), Ordering::Release);
        }
    }

    #[inline]
    pub fn is_empty_relaxed(&self) -> bool {
        self.pending_count.load(Ordering::Relaxed) == 0
    }

    pub fn help_verify(&self) -> bool {
        if self.is_empty_relaxed() {
            return false;
        }
        let mut tasks: [Option<SignatureTask>; 4] = [None, None, None, None];
        let mut count = 0;
        {
            let Some(mut lock) = self.tasks.try_lock() else {
                return false;
            };
            if lock.is_empty() {
                self.pending_count.store(0, Ordering::Relaxed);
                return false;
            }
            while count < 4 {
                if let Some(t) = lock.pop_front() {
                    let already_verified = t.block.is_signature_verified()
                        || t.ctx
                            .as_ref()
                            .is_some_and(|c| c.block.is_signature_verified());
                    if already_verified {
                        t.block.set_signature_verified(true);
                        if let Some(ctx) = &t.ctx {
                            ctx.block.set_signature_verified(true);
                        }
                        if let Some(res) = &t.result {
                            res.store(SignatureResult::Valid as u8, Ordering::Release);
                        }
                        if let Some(fin) = &t.finished {
                            fin.store(true, Ordering::Release);
                        }
                        continue;
                    }
                    tasks[count] = Some(t);
                    count += 1;
                } else {
                    break;
                }
            }
            self.pending_count.store(lock.len(), Ordering::Relaxed);
        }
        
        if count == 0 {
            return false;
        }
        
        self.verify_tasks_slice(&mut tasks[..count]);
        true
    }

    pub fn try_help_verify(&self) -> bool {
        self.help_verify()
    }

    fn verify_tasks_slice(&self, tasks: &mut [Option<SignatureTask>]) {
        if tasks.len() == 4 {
            let mut all_have_account = true;
            let mut accounts = [rsnano_core::Account::zero(); 4];
            for i in 0..4 {
                let t = tasks[i].as_ref().unwrap();
                if let Some(acc) = t.account {
                    accounts[i] = acc;
                } else if let Some(acc) = t.block.account_field() {
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
                    t0.block.hash(),
                    t1.block.hash(),
                    t2.block.hash(),
                    t3.block.hash(),
                ];
                let msgs = [
                    hashes[0].as_bytes() as &[u8],
                    hashes[1].as_bytes() as &[u8],
                    hashes[2].as_bytes() as &[u8],
                    hashes[3].as_bytes() as &[u8],
                ];
                let sigs_val = [
                    t0.block.signature(),
                    t1.block.signature(),
                    t2.block.signature(),
                    t3.block.signature(),
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
                        t.block.set_signature_verified(true);
                        if let Some(ctx) = &t.ctx {
                            ctx.block.set_signature_verified(true);
                        }
                    }
                    if let Some(res) = &t.result {
                        res.store(
                            if results[i] { SignatureResult::Valid as u8 } else { SignatureResult::Invalid as u8 },
                            Ordering::Release,
                        );
                    }
                    if let Some(fin) = &t.finished {
                        fin.store(true, Ordering::Release);
                    }
                }
                return;
            }
        }

        // Graceful sequential fallback for less than 4 tasks or invalid accounts
        for opt in tasks.iter_mut() {
            if let Some(task) = opt.take() {
                let account = if let Some(acc) = task.account {
                    acc
                } else if let Some(acc) = task.block.account_field() {
                    acc
                } else {
                    if let Some(fin) = &task.finished {
                        fin.store(true, Ordering::Release);
                    }
                    continue;
                };

                let valid = task.block.verify_with_key(&account.as_key()).is_ok();
                if valid {
                    if let Some(ctx) = &task.ctx {
                        ctx.block.set_signature_verified(true);
                    }
                }
                if let Some(res) = &task.result {
                    res.store(
                        if valid { SignatureResult::Valid as u8 } else { SignatureResult::Invalid as u8 },
                        Ordering::Release,
                    );
                }
                if let Some(fin) = &task.finished {
                    fin.store(true, Ordering::Release);
                }
            }
        }
    }

    pub fn total_tasks(&self) -> usize {
        self.tasks.lock().len()
    }
}
