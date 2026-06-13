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
    pub account: Option<Account>,
    pub result: Arc<AtomicU8>,
    pub finished: Arc<AtomicBool>,
}

pub struct SignatureChecker {
    tasks: Mutex<VecDeque<SignatureTask>>,
}

impl SignatureChecker {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(VecDeque::with_capacity(256)),
        }
    }

    pub fn push(&self, block: Arc<Block>, account: Option<Account>) -> (Arc<AtomicU8>, Arc<AtomicBool>) {
        let result = Arc::new(AtomicU8::new(SignatureResult::Unknown as u8));
        let finished = Arc::new(AtomicBool::new(false));
        let task = SignatureTask {
            block,
            account,
            result: result.clone(),
            finished: finished.clone(),
        };
        self.tasks.lock().push_back(task);
        (result, finished)
    }

    pub fn help_verify(&self) -> bool {
        let tasks = {
            let mut lock = self.tasks.lock();
            let mut vec = alloc::vec::Vec::with_capacity(4);
            while vec.len() < 4 {
                if let Some(t) = lock.pop_front() {
                    vec.push(t);
                } else {
                    break;
                }
            }
            vec
        };
        
        if tasks.is_empty() {
            return false;
        }
        
        self.verify_tasks_batched(tasks);
        true
    }

    pub fn try_help_verify(&self) -> bool {
        let tasks = {
            if let Some(mut lock) = self.tasks.try_lock() {
                let mut vec = alloc::vec::Vec::with_capacity(4);
                while vec.len() < 4 {
                    if let Some(t) = lock.pop_front() {
                        vec.push(t);
                    } else {
                        break;
                    }
                }
                vec
            } else {
                alloc::vec::Vec::new()
            }
        };
        
        if tasks.is_empty() {
            return false;
        }
        
        self.verify_tasks_batched(tasks);
        true
    }

    fn verify_tasks_batched(&self, tasks: alloc::vec::Vec<SignatureTask>) {
        if tasks.len() == 4 {
            let mut resolved_accounts = [Account::zero(); 4];
            let mut valid_accounts = true;
            for i in 0..4 {
                if let Some(acc) = tasks[i].account.clone().or_else(|| tasks[i].block.account_field()) {
                    resolved_accounts[i] = acc;
                } else {
                    valid_accounts = false;
                }
            }
            
            if valid_accounts {
                let pub_keys = [
                    resolved_accounts[0].as_bytes().try_into().unwrap(),
                    resolved_accounts[1].as_bytes().try_into().unwrap(),
                    resolved_accounts[2].as_bytes().try_into().unwrap(),
                    resolved_accounts[3].as_bytes().try_into().unwrap(),
                ];
                
                let hashes = [
                    tasks[0].block.hash(),
                    tasks[1].block.hash(),
                    tasks[2].block.hash(),
                    tasks[3].block.hash(),
                ];
                let messages = [
                    &hashes[0].as_bytes()[..],
                    &hashes[1].as_bytes()[..],
                    &hashes[2].as_bytes()[..],
                    &hashes[3].as_bytes()[..],
                ];
                
                let sig_bytes = [
                    tasks[0].block.signature().as_bytes().try_into().unwrap(),
                    tasks[1].block.signature().as_bytes().try_into().unwrap(),
                    tasks[2].block.signature().as_bytes().try_into().unwrap(),
                    tasks[3].block.signature().as_bytes().try_into().unwrap(),
                ];
                
                let valids = rsnano_sha::ed25519::verify_batch_4(pub_keys, messages, sig_bytes);
                for i in 0..4 {
                    tasks[i].result.store(
                        if valids[i] { SignatureResult::Valid as u8 } else { SignatureResult::Invalid as u8 },
                        Ordering::Release
                    );
                    tasks[i].finished.store(true, Ordering::Release);
                }
                return;
            }
        }
        
        // Graceful sequential fallback for less than 4 tasks or invalid accounts
        for task in tasks {
            let account = if let Some(acc) = task.account {
                acc
            } else if let Some(acc) = task.block.account_field() {
                acc
            } else {
                task.finished.store(true, Ordering::Release);
                continue;
            };

            let valid = task.block.verify_with_key(&account.as_key()).is_ok();
            task.result.store(
                if valid { SignatureResult::Valid as u8 } else { SignatureResult::Invalid as u8 },
                Ordering::Release
            );
            task.finished.store(true, Ordering::Release);
        }
    }

    pub fn total_tasks(&self) -> usize {
        self.tasks.lock().len()
    }
}
