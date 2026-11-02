use alloc::string::String;
use alloc::vec::Vec;
use kernel_common::storage::btree::KEY_SIZE;
use kernel_common::storage::WALLET_SHARD;
use rsnano_core::{PublicKey, RawKey, WalletId, WorkNonce};

const WALLET_META_MAGIC: u32 = 0x5741_4C31; // "WAL1"
const WALLET_KEY_MAGIC: u8 = 0x4B; // 'K'
const MAX_PASSWORD_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PersistedKeyType {
    Adhoc = 1,
    Deterministic = 2,
    Watch = 3,
}

impl PersistedKeyType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Adhoc),
            2 => Some(Self::Deterministic),
            3 => Some(Self::Watch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedWalletMeta {
    pub representative: PublicKey,
    pub seed: RawKey,
    pub deterministic_index: u32,
    pub locked: bool,
    pub password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistedWalletKey {
    pub key_type: PersistedKeyType,
    pub priv_key: [u8; 32],
    pub work: WorkNonce,
}

pub struct BtreeWalletStore {}

impl BtreeWalletStore {
    pub const TABLE_ID: u8 = 13;

    pub const fn new() -> Self {
        Self {}
    }

    #[inline]
    fn meta_key(wallet_id: &WalletId) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(wallet_id.as_bytes());
        // key[33..65] == [0u8; 32] identifies the wallet metadata record
        key
    }

    #[inline]
    fn account_key(wallet_id: &WalletId, pub_key: &PublicKey) -> [u8; KEY_SIZE] {
        let mut key = [0u8; KEY_SIZE];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(wallet_id.as_bytes());
        key[33..65].copy_from_slice(pub_key.as_bytes());
        key
    }

    fn encode_meta(meta: &PersistedWalletMeta, buf: &mut [u8; 256]) -> usize {
        buf[0..4].copy_from_slice(&WALLET_META_MAGIC.to_le_bytes());
        buf[4..36].copy_from_slice(meta.representative.as_bytes());
        buf[36..68].copy_from_slice(meta.seed.as_bytes());
        buf[68..72].copy_from_slice(&meta.deterministic_index.to_le_bytes());
        buf[72] = u8::from(meta.locked);

        let pw_bytes = meta.password.as_bytes();
        let mut pw_len = pw_bytes.len().min(MAX_PASSWORD_BYTES);
        while pw_len > 0 && !meta.password.is_char_boundary(pw_len) {
            pw_len -= 1;
        }
        buf[73] = pw_len as u8;
        buf[74..74 + pw_len].copy_from_slice(&pw_bytes[..pw_len]);
        74 + pw_len
    }

    fn decode_meta(val: &[u8]) -> Option<PersistedWalletMeta> {
        if val.len() < 74 {
            return None;
        }
        let magic = u32::from_le_bytes(val[0..4].try_into().ok()?);
        if magic != WALLET_META_MAGIC {
            return None;
        }
        let representative = PublicKey::from_bytes(val[4..36].try_into().ok()?);
        let seed = RawKey::from_bytes(val[36..68].try_into().ok()?);
        let deterministic_index = u32::from_le_bytes(val[68..72].try_into().ok()?);
        let locked = val[72] != 0;
        let pw_len = val[73] as usize;
        if val.len() < 74 + pw_len {
            return None;
        }
        let password = core::str::from_utf8(&val[74..74 + pw_len])
            .ok()
            .map(String::from)?;
        Some(PersistedWalletMeta {
            representative,
            seed,
            deterministic_index,
            locked,
            password,
        })
    }

    fn encode_key(entry: &PersistedWalletKey) -> [u8; 42] {
        let mut buf = [0u8; 42];
        buf[0] = WALLET_KEY_MAGIC;
        buf[1] = entry.key_type as u8;
        buf[2..34].copy_from_slice(&entry.priv_key);
        let work_u64: u64 = entry.work.into();
        buf[34..42].copy_from_slice(&work_u64.to_le_bytes());
        buf
    }

    fn decode_key(val: &[u8]) -> Option<PersistedWalletKey> {
        if val.len() < 42 || val[0] != WALLET_KEY_MAGIC {
            return None;
        }
        let key_type = PersistedKeyType::from_u8(val[1])?;
        let priv_key: [u8; 32] = val[2..34].try_into().ok()?;
        let work_u64 = u64::from_le_bytes(val[34..42].try_into().ok()?);
        Some(PersistedWalletKey {
            key_type,
            priv_key,
            work: WorkNonce::from(work_u64),
        })
    }

    pub fn put_wallet_meta(&self, wallet_id: &WalletId, meta: &PersistedWalletMeta) {
        let key = Self::meta_key(wallet_id);
        let mut buf = [0u8; 256];
        let len = Self::encode_meta(meta, &mut buf);
        let mut lock = WALLET_SHARD.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buf[..len]);
            let _ = db.flush();
        }
    }

    pub fn put_wallet_key(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
        entry: &PersistedWalletKey,
    ) {
        if pub_key.is_zero() {
            return;
        }
        let key = Self::account_key(wallet_id, pub_key);
        let buf = Self::encode_key(entry);
        let mut lock = WALLET_SHARD.lock();
        if let Some(db) = lock.as_mut() {
            db.insert(key, &buf);
            let _ = db.flush();
        }
    }

    pub fn del_wallet_key(&self, wallet_id: &WalletId, pub_key: &PublicKey) {
        if pub_key.is_zero() {
            return;
        }
        let key = Self::account_key(wallet_id, pub_key);
        let mut lock = WALLET_SHARD.lock();
        if let Some(db) = lock.as_mut() {
            db.delete(key);
            let _ = db.flush();
        }
    }

    pub fn destroy_wallet(&self, wallet_id: &WalletId) {
        let start_key = Self::meta_key(wallet_id);
        let mut lock = WALLET_SHARD.lock();
        let Some(db) = lock.as_mut() else {
            return;
        };
        let mut keys_to_delete = Vec::new();
        {
            let mut it = db.iter(start_key);
            while let Some(res) = it.next_kv_with(|k, _| {
                if k[0] != Self::TABLE_ID || &k[1..33] != wallet_id.as_bytes() {
                    return Err(());
                }
                Ok(*k)
            }) {
                match res {
                    Ok(k) => keys_to_delete.push(k),
                    Err(()) => break,
                }
            }
        }
        if !keys_to_delete.is_empty() {
            for k in keys_to_delete {
                db.delete(k);
            }
            let _ = db.flush();
        }
    }

    pub fn load_all(
        &self,
    ) -> Vec<(
        WalletId,
        PersistedWalletMeta,
        Vec<(PublicKey, PersistedWalletKey)>,
    )> {
        let mut start_key = [0u8; KEY_SIZE];
        start_key[0] = Self::TABLE_ID;

        let mut wallets: Vec<(
            WalletId,
            PersistedWalletMeta,
            Vec<(PublicKey, PersistedWalletKey)>,
        )> = Vec::new();

        let mut lock = WALLET_SHARD.lock();
        let Some(db) = lock.as_mut() else {
            return wallets;
        };
        let mut it = db.iter(start_key);
        while let Some(step) = it.next_kv_with(|k, v| {
            if k[0] != Self::TABLE_ID {
                return Err(());
            }
            let wid_bytes: [u8; 32] = k[1..33].try_into().unwrap();
            let wallet_id = WalletId::from_bytes(wid_bytes);
            let pk_bytes: [u8; 32] = k[33..65].try_into().unwrap();
            if pk_bytes == [0u8; 32] {
                Ok((wallet_id, None, Self::decode_meta(v), None))
            } else {
                let pub_key = PublicKey::from_bytes(pk_bytes);
                Ok((wallet_id, Some(pub_key), None, Self::decode_key(v)))
            }
        }) {
            match step {
                Ok((wallet_id, None, Some(meta), None)) => {
                    if let Some(entry) = wallets.iter_mut().find(|(id, _, _)| *id == wallet_id) {
                        entry.1 = meta;
                    } else {
                        wallets.push((wallet_id, meta, Vec::new()));
                    }
                }
                Ok((wallet_id, Some(pub_key), None, Some(key_rec))) => {
                    if let Some(entry) = wallets.iter_mut().find(|(id, _, _)| *id == wallet_id) {
                        entry.2.push((pub_key, key_rec));
                    } else {
                        let default_meta = PersistedWalletMeta {
                            representative: PublicKey::zero(),
                            seed: RawKey::zero(),
                            deterministic_index: 0,
                            locked: false,
                            password: String::new(),
                        };
                        let mut keys = Vec::new();
                        keys.push((pub_key, key_rec));
                        wallets.push((wallet_id, default_meta, keys));
                    }
                }
                Ok(_) => {}
                Err(()) => break,
            }
        }

        wallets
    }

    pub fn restore_all(
        &self,
        wallets: &[(
            WalletId,
            PersistedWalletMeta,
            Vec<(PublicKey, PersistedWalletKey)>,
        )],
    ) {
        for (wallet_id, meta, keys) in wallets {
            self.put_wallet_meta(wallet_id, meta);
            for (pub_key, key_rec) in keys {
                self.put_wallet_key(wallet_id, pub_key, key_rec);
            }
        }
    }
}
