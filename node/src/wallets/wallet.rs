use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use hashbrown::{HashMap, HashSet};
use spin::Mutex;

use tracing::warn;

use kernel_common::storage::wallet_crypto;
use rsnano_core::{
    deterministic_key, Amount, Block, BlockHash, Link, PrivateKey, PublicKey, RawKey, Root,
    StateBlockArgs, WorkNonce,
};
use rsnano_ledger::{
    AnySet, Ledger, PersistedKeyType, PersistedWalletKey, PersistedWalletMeta,
};
use rsnano_work::WorkThresholds;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KeyType {
    NotAnAccount,
    Unknown,
    Adhoc,
    Deterministic,
}

pub struct Wallet {
    pub representatives: Mutex<HashSet<PublicKey>>,
    pub representative: Mutex<PublicKey>,
    ledger: Arc<Ledger>,
    #[allow(dead_code)]
    work_thresholds: WorkThresholds,
    /// Decrypted private keys held in RAM only while the wallet is unlocked.
    pub keys: Mutex<HashMap<PublicKey, PrivateKey>>,
    /// Hardware-bound + KDF-encrypted 32-byte private keys indexed by public key.
    pub encrypted_keys: Mutex<HashMap<PublicKey, [u8; 32]>>,
    pub deterministic_keys: Mutex<HashSet<PublicKey>>,
    pub watch_keys: Mutex<HashSet<PublicKey>>,
    pub work_cache: Mutex<HashMap<PublicKey, WorkNonce>>,
    /// Decrypted wallet seed held in RAM only while the wallet is unlocked.
    pub seed: Mutex<RawKey>,
    pub encrypted_seed: Mutex<[u8; 32]>,
    pub salt: [u8; 32],
    pub wrapping_key: Mutex<[u8; 32]>,
    pub check_tag: Mutex<[u8; 32]>,
    pub deterministic_index: AtomicU32,
    pub has_password: AtomicBool,
    pub locked: AtomicBool,
}

impl Wallet {
    pub fn new(
        ledger: Arc<Ledger>,
        work_thresholds: WorkThresholds,
        representative: PublicKey,
    ) -> anyhow::Result<Self> {
        let seed = PrivateKey::new().raw_key();
        let salt = *PrivateKey::new().raw_key().as_bytes();
        let wrapping_key = wallet_crypto::derive_wrapping_key(&salt, "");
        let encrypted_seed = wallet_crypto::crypt_seed(&wrapping_key, &salt, seed.as_bytes());
        let check_tag = wallet_crypto::compute_check_tag(
            &wrapping_key,
            &salt,
            representative.as_bytes(),
            &encrypted_seed,
            false,
        );

        Ok(Self {
            representatives: Mutex::new(HashSet::new()),
            representative: Mutex::new(representative),
            ledger,
            work_thresholds,
            keys: Mutex::new(HashMap::new()),
            encrypted_keys: Mutex::new(HashMap::new()),
            deterministic_keys: Mutex::new(HashSet::new()),
            watch_keys: Mutex::new(HashSet::new()),
            work_cache: Mutex::new(HashMap::new()),
            seed: Mutex::new(seed),
            encrypted_seed: Mutex::new(encrypted_seed),
            salt,
            wrapping_key: Mutex::new(wrapping_key),
            check_tag: Mutex::new(check_tag),
            deterministic_index: AtomicU32::new(0),
            has_password: AtomicBool::new(false),
            locked: AtomicBool::new(false),
        })
    }

    pub fn from_persisted(
        ledger: Arc<Ledger>,
        work_thresholds: WorkThresholds,
        meta: PersistedWalletMeta,
        persisted_keys: alloc::vec::Vec<(PublicKey, PersistedWalletKey)>,
    ) -> Self {
        let mut encrypted_keys = HashMap::new();
        let mut deterministic_keys = HashSet::new();
        let mut watch_keys = HashSet::new();
        let mut work_cache = HashMap::new();

        for (pub_key, rec) in persisted_keys {
            if u64::from(rec.work) != 0 {
                work_cache.insert(pub_key, rec.work);
            }
            match rec.key_type {
                PersistedKeyType::Adhoc => {
                    encrypted_keys.insert(pub_key, rec.encrypted_priv_key);
                }
                PersistedKeyType::Deterministic => {
                    encrypted_keys.insert(pub_key, rec.encrypted_priv_key);
                    deterministic_keys.insert(pub_key);
                }
                PersistedKeyType::Watch => {
                    watch_keys.insert(pub_key);
                }
            }
        }

        let wallet = Self {
            representatives: Mutex::new(HashSet::new()),
            representative: Mutex::new(meta.representative),
            ledger,
            work_thresholds,
            keys: Mutex::new(HashMap::new()),
            encrypted_keys: Mutex::new(encrypted_keys),
            deterministic_keys: Mutex::new(deterministic_keys),
            watch_keys: Mutex::new(watch_keys),
            work_cache: Mutex::new(work_cache),
            seed: Mutex::new(RawKey::zero()),
            encrypted_seed: Mutex::new(meta.encrypted_seed),
            salt: meta.salt,
            wrapping_key: Mutex::new([0u8; 32]),
            check_tag: Mutex::new(meta.check_tag),
            deterministic_index: AtomicU32::new(meta.deterministic_index),
            has_password: AtomicBool::new(meta.has_password),
            locked: AtomicBool::new(true),
        };

        // Mode A: If the operator has not set a custom password (`has_password == false`),
        // automatically unseal the wallet into RAM using the Ky X1 SoC eFuse hardware root key.
        if !meta.has_password {
            let _ = wallet.try_unlock("");
        }

        wallet
    }

    /// Wipes plaintext private keys, seed, and wrapping key from RAM and marks the wallet locked.
    pub fn lock_in_ram(&self) {
        self.keys.lock().clear();
        *self.seed.lock() = RawKey::zero();
        *self.wrapping_key.lock() = [0u8; 32];
        self.locked.store(true, Ordering::SeqCst);
    }

    /// Attempts to derive the wrapping key from `(Ky X1 eFuse key, salt, password)`, verify
    /// `check_tag`, and decrypt `encrypted_seed` and `encrypted_keys` into RAM.
    pub fn try_unlock(&self, password: &str) -> bool {
        let has_pw = self.has_password.load(Ordering::SeqCst);
        if has_pw && password.is_empty() {
            self.lock_in_ram();
            return false;
        }
        if !has_pw && !password.is_empty() {
            self.lock_in_ram();
            return false;
        }

        let candidate_wk = wallet_crypto::derive_wrapping_key(&self.salt, password);
        let rep_bytes = *self.representative.lock().as_bytes();
        let enc_seed = *self.encrypted_seed.lock();
        let expected_tag = wallet_crypto::compute_check_tag(
            &candidate_wk,
            &self.salt,
            &rep_bytes,
            &enc_seed,
            has_pw,
        );
        if !wallet_crypto::verify_check_tag(&expected_tag, &self.check_tag.lock()) {
            self.lock_in_ram();
            return false;
        }

        let raw_seed = wallet_crypto::crypt_seed(&candidate_wk, &self.salt, &enc_seed);
        let mut decrypted_keys = HashMap::new();
        {
            let enc_guard = self.encrypted_keys.lock();
            for (pub_key, enc_priv) in enc_guard.iter() {
                let raw_priv = wallet_crypto::crypt_priv_key(
                    &candidate_wk,
                    &self.salt,
                    pub_key.as_bytes(),
                    enc_priv,
                );
                let priv_key = PrivateKey::from_bytes(&raw_priv);
                if priv_key.public_key() != *pub_key {
                    self.lock_in_ram();
                    return false;
                }
                decrypted_keys.insert(*pub_key, priv_key);
            }
        }

        *self.keys.lock() = decrypted_keys;
        *self.seed.lock() = RawKey::from_bytes(raw_seed);
        *self.wrapping_key.lock() = candidate_wk;
        self.locked.store(false, Ordering::SeqCst);
        true
    }

    /// Re-encrypts the wallet's seed and all private keys under a new password (combined with
    /// the Ky X1 SoC eFuse hardware root key).
    pub fn rekey(&self, new_password: &str) -> bool {
        if !self.valid_password() {
            return false;
        }
        let has_pw = !new_password.is_empty();
        let new_wk = wallet_crypto::derive_wrapping_key(&self.salt, new_password);
        let seed_bytes = *self.seed.lock().as_bytes();
        let new_enc_seed = wallet_crypto::crypt_seed(&new_wk, &self.salt, &seed_bytes);
        let rep_bytes = *self.representative.lock().as_bytes();
        let new_tag = wallet_crypto::compute_check_tag(
            &new_wk,
            &self.salt,
            &rep_bytes,
            &new_enc_seed,
            has_pw,
        );

        {
            let keys_guard = self.keys.lock();
            let mut enc_guard = self.encrypted_keys.lock();
            for (pub_key, priv_key) in keys_guard.iter() {
                let enc = wallet_crypto::crypt_priv_key(
                    &new_wk,
                    &self.salt,
                    pub_key.as_bytes(),
                    priv_key.raw_key().as_bytes(),
                );
                enc_guard.insert(*pub_key, enc);
            }
        }

        *self.wrapping_key.lock() = new_wk;
        *self.encrypted_seed.lock() = new_enc_seed;
        *self.check_tag.lock() = new_tag;
        self.has_password.store(has_pw, Ordering::SeqCst);
        true
    }

    pub fn refresh_meta_encryption(&self) {
        if !self.valid_password() {
            return;
        }
        let wk = *self.wrapping_key.lock();
        let seed_bytes = *self.seed.lock().as_bytes();
        let enc_seed = wallet_crypto::crypt_seed(&wk, &self.salt, &seed_bytes);
        let rep_bytes = *self.representative.lock().as_bytes();
        let has_pw = self.has_password.load(Ordering::SeqCst);
        let tag = wallet_crypto::compute_check_tag(&wk, &self.salt, &rep_bytes, &enc_seed, has_pw);
        *self.encrypted_seed.lock() = enc_seed;
        *self.check_tag.lock() = tag;
    }

    pub fn to_persisted_meta(&self) -> PersistedWalletMeta {
        PersistedWalletMeta {
            representative: *self.representative.lock(),
            deterministic_index: self.deterministic_index.load(Ordering::SeqCst),
            has_password: self.has_password.load(Ordering::SeqCst),
            salt: self.salt,
            encrypted_seed: *self.encrypted_seed.lock(),
            check_tag: *self.check_tag.lock(),
        }
    }

    pub fn to_persisted_key(&self, pub_key: &PublicKey) -> Option<PersistedWalletKey> {
        let work = self
            .work_cache
            .lock()
            .get(pub_key)
            .copied()
            .unwrap_or_else(|| WorkNonce::from(0));
        if self.deterministic_keys.lock().contains(pub_key) {
            let enc = *self.encrypted_keys.lock().get(pub_key)?;
            Some(PersistedWalletKey {
                key_type: PersistedKeyType::Deterministic,
                encrypted_priv_key: enc,
                work,
            })
        } else if let Some(&enc) = self.encrypted_keys.lock().get(pub_key) {
            Some(PersistedWalletKey {
                key_type: PersistedKeyType::Adhoc,
                encrypted_priv_key: enc,
                work,
            })
        } else if self.watch_keys.lock().contains(pub_key) {
            Some(PersistedWalletKey {
                key_type: PersistedKeyType::Watch,
                encrypted_priv_key: [0u8; 32],
                work,
            })
        } else {
            None
        }
    }

    pub fn deterministic_check(&self, index: u32) -> u32 {
        let mut result = index;
        let any = self.ledger.any();
        let seed = *self.seed.lock();
        let mut i = index + 1;
        let mut n = index + 64;
        while i < n {
            let raw = deterministic_key(&seed, i);
            let pair = PrivateKey::from(raw);
            if any.account_head(&pair.account()).is_some()
                || any.receivable_exists(pair.account())
            {
                result = i;
                n = i + 64 + (i / 64);
            }
            i += 1;
        }
        result
    }

    pub fn insert_key(&self, priv_key: PrivateKey) -> PublicKey {
        let pub_key = priv_key.public_key();
        let wk = *self.wrapping_key.lock();
        let enc = wallet_crypto::crypt_priv_key(
            &wk,
            &self.salt,
            pub_key.as_bytes(),
            priv_key.raw_key().as_bytes(),
        );
        self.encrypted_keys.lock().insert(pub_key, enc);
        self.keys.lock().insert(pub_key, priv_key);
        pub_key
    }

    pub fn insert_deterministic_at(&self, index: u32) -> PublicKey {
        let seed = *self.seed.lock();
        let raw = deterministic_key(&seed, index);
        let priv_key = PrivateKey::from(raw);
        let pub_key = self.insert_key(priv_key);
        self.deterministic_keys.lock().insert(pub_key);
        let current = self.deterministic_index.load(Ordering::SeqCst);
        if index >= current {
            self.deterministic_index.store(index + 1, Ordering::SeqCst);
        }
        pub_key
    }

    pub fn insert_deterministic(&self) -> PublicKey {
        let index = self.deterministic_index.fetch_add(1, Ordering::SeqCst);
        let seed = *self.seed.lock();
        let raw = deterministic_key(&seed, index);
        let priv_key = PrivateKey::from(raw);
        let pub_key = self.insert_key(priv_key);
        self.deterministic_keys.lock().insert(pub_key);
        pub_key
    }

    pub fn create_block(
        &self,
        pub_key: &PublicKey,
        previous: BlockHash,
        representative: PublicKey,
        balance: Amount,
        link: Link,
        work: WorkNonce,
    ) -> anyhow::Result<Block> {
        let keys = self.keys.lock();
        let prv = keys
            .get(pub_key)
            .ok_or_else(|| anyhow::anyhow!("Key not found in wallet"))?;
        let args = StateBlockArgs {
            key: prv,
            previous,
            representative,
            balance,
            link,
            work,
        };
        Ok(Block::from(args))
    }

    pub fn get_key(&self, pub_key: &PublicKey) -> Option<PrivateKey> {
        self.keys.lock().get(pub_key).cloned()
    }

    pub fn work_update(&self, pub_key: &PublicKey, root: &Root, work: WorkNonce) {
        let latest = self.ledger.any().latest_root(&(*pub_key).into());
        if latest == *root {
            self.work_cache.lock().insert(*pub_key, work);
        } else {
            warn!("Cached work no longer valid, discarding");
        }
    }

    pub fn live(&self) -> bool {
        true
    }

    pub fn exists(&self, pub_key: &PublicKey) -> bool {
        self.encrypted_keys.lock().contains_key(pub_key)
            || self.watch_keys.lock().contains(pub_key)
    }

    pub fn valid_password(&self) -> bool {
        !self.locked.load(Ordering::SeqCst)
    }

    pub fn key_type(&self, pub_key: &PublicKey) -> KeyType {
        if self.deterministic_keys.lock().contains(pub_key) {
            KeyType::Deterministic
        } else if self.encrypted_keys.lock().contains_key(pub_key)
            || self.watch_keys.lock().contains(pub_key)
        {
            KeyType::Adhoc
        } else {
            KeyType::NotAnAccount
        }
    }
}
